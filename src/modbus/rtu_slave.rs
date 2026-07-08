//! Modbus RTU 从站
//!
//! 通过 RS485 #1 (UART0) 响应外部主站请求。
//! 支持 FC=01/02/03/04/05/06/0F/10, 错误时返回 Modbus 异常响应。
//!
//! 本模块**手写 Modbus RTU 帧 + CRC16**, 不依赖 umodbus crate。

use std::sync::Arc;
use std::time::Duration;

use crate::config::modbus::rtu_slave as cfg;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::shared::{exc, modbus_crc16, BusBackend, ModbusBackend};
use crate::rs485::{Rs485Config, Rs485Port};

/// 任务心跳记录 (静态分配, main_loop 监控)
/// 最大停滞阈值放宽到 10 (从站监听周期 1s, 允许长时间无请求)
static TASK_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-slave", 10);

pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    health::register(&TASK_HB);
    let port_cfg = Rs485Config::from_rtu_slave();
    let mut port = Rs485Port::open(&port_cfg)?;
    let backend = BusBackend;

    std::thread::Builder::new()
        .name("mb-rtu-slave".into())
        .spawn(move || {
            crate::health::pin_current_to_core(crate::health::CORE_NET);
            let mut buf = [0u8; 256];
            loop {
                // 心跳: 每次循环 (即使无请求也 1s 返回一次)
                TASK_HB.tick();
                match port.read(&mut buf, 1000) {
                    Ok(0) => continue,
                    Ok(n) => {
                        if let Err(e) = handle_request(&mut port, &backend, &buf[..n]) {
                            log::warn!("[mb-rtu-slave] handle: {}", e);
                        }
                    }
                    Err(e) => {
                        log::warn!("[mb-rtu-slave] read: {}", e);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        })
        .map_err(|e| crate::error::AppError::Modbus(format!("spawn: {e}")))?;

    log::info!("[mb-rtu-slave] started on uart{} addr={}", cfg::UART_PORT, cfg::ADDR);
    Ok(())
}

fn handle_request(port: &mut Rs485Port, backend: &BusBackend, req: &[u8]) -> AppResult<()> {
    // 最小帧: slave(1) + func(1) + crc(2) = 4
    if req.len() < 4 {
        return Ok(());
    }

    let slave = req[0];
    // 广播 0 不响应
    if slave != 0 && slave != cfg::ADDR {
        return Ok(());
    }

    // CRC 校验
    let n = req.len();
    let crc = modbus_crc16(&req[..n - 2]);
    let recv_crc = u16::from_le_bytes([req[n - 2], req[n - 1]]);
    if crc != recv_crc {
        log::debug!("[mb-rtu-slave] crc mismatch: {:#06x}!={:#06x}", crc, recv_crc);
        return Ok(());
    }

    let func = req[1];
    let resp = build_response(backend, slave, func, &req[2..n - 2]);

    // 广播不返回响应
    if slave == 0 {
        return Ok(());
    }

    port.write(&resp)?;
    Ok(())
}

fn build_response(backend: &BusBackend, slave: u8, func: u8, pdu: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.push(slave);

    let body: Vec<u8> = match func {
        // FC=01 Read Coils
        0x01 => read_bits(backend, func, pdu, |b, a, c| b.read_coils(a, c)),
        // FC=02 Read Discrete Inputs
        0x02 => read_bits(backend, func, pdu, |b, a, c| b.read_discrete_inputs(a, c)),
        // FC=03 Read Holding Registers
        0x03 => read_regs(backend, func, pdu, |b, a, c| b.read_holding_registers(a, c)),
        // FC=04 Read Input Registers
        0x04 => read_regs(backend, func, pdu, |b, a, c| b.read_input_registers(a, c)),
        // FC=05 Write Single Coil
        0x05 => write_single_coil(backend, func, pdu),
        // FC=06 Write Single Register
        0x06 => write_single_reg(backend, func, pdu),
        // FC=0F Write Multiple Coils
        0x0F => write_multi_coils(backend, func, pdu),
        // FC=10 Write Multiple Registers
        0x10 => write_multi_regs(backend, func, pdu),
        _ => exc_response(func, exc::ILLEGAL_FUNCTION),
    };

    out.extend_from_slice(&body);
    let crc = modbus_crc16(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

fn read_bits<F>(backend: &BusBackend, func: u8, pdu: &[u8], f: F) -> Vec<u8>
where
    F: Fn(&BusBackend, u16, u16) -> Vec<bool>,
{
    if pdu.len() < 4 {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 2000 {
        return exc_response(func, exc::ILLEGAL_DATA_VALUE);
    }
    let bits = f(backend, addr, count);
    let byte_count = ((count as usize) + 7) / 8;
    let mut out = Vec::with_capacity(2 + byte_count);
    out.push(byte_count as u8);
    let mut bytes = vec![0u8; byte_count];
    for (i, b) in bits.iter().enumerate() {
        if *b {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    out.extend_from_slice(&bytes);
    out
}

fn read_regs<F>(backend: &BusBackend, func: u8, pdu: &[u8], f: F) -> Vec<u8>
where
    F: Fn(&BusBackend, u16, u16) -> Vec<u16>,
{
    if pdu.len() < 4 {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 125 {
        return exc_response(func, exc::ILLEGAL_DATA_VALUE);
    }
    let regs = f(backend, addr, count);
    let mut out = Vec::with_capacity(1 + regs.len() * 2);
    out.push((regs.len() * 2) as u8);
    for r in regs {
        out.extend_from_slice(&r.to_be_bytes());
    }
    out
}

fn write_single_coil(backend: &BusBackend, func: u8, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 4 {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    let on = value == 0xFF00;
    if !on && value != 0 {
        return exc_response(func, exc::ILLEGAL_DATA_VALUE);
    }
    if !backend.write_single_coil(addr, on) {
        return exc_response(func, exc::ILLEGAL_DATA_ADDRESS);
    }
    pdu.to_vec()
}

fn write_single_reg(backend: &BusBackend, func: u8, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 4 {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    if !backend.write_single_register(addr, value) {
        return exc_response(func, exc::ILLEGAL_DATA_ADDRESS);
    }
    pdu.to_vec()
}

fn write_multi_coils(backend: &BusBackend, func: u8, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 5 {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    if pdu.len() < 5 + byte_count {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let mut bits = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let byte = pdu[5 + i / 8];
        bits.push(byte & (1 << (i % 8)) != 0);
    }
    if !backend.write_multiple_coils(addr, &bits) {
        return exc_response(func, exc::ILLEGAL_DATA_ADDRESS);
    }
    pdu[..4].to_vec()
}

fn write_multi_regs(backend: &BusBackend, func: u8, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 5 {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    if pdu.len() < 5 + byte_count || byte_count != count as usize * 2 {
        return exc_response(func, exc::SLAVE_DEVICE_FAILURE);
    }
    let mut regs = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        regs.push(u16::from_be_bytes([pdu[5 + 2 * i], pdu[6 + 2 * i]]));
    }
    if !backend.write_multiple_registers(addr, &regs) {
        return exc_response(func, exc::ILLEGAL_DATA_ADDRESS);
    }
    pdu[..4].to_vec()
}

fn exc_response(func: u8, code: u8) -> Vec<u8> {
    vec![func | 0x80, code]
}
