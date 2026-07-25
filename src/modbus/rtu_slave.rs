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
use crate::modbus::shared::{modbus_crc16, BusBackend};
use crate::rs485::{Rs485Config, Rs485Port};

/// 任务心跳记录 (静态分配, main_loop 监控)
/// 最大停滞阈值放宽到 10 (从站监听周期 1s, 允许长时间无请求)
static TASK_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-slave", 10);

pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    health::register(&TASK_HB);
    let port_cfg = Rs485Config::from_rtu_slave();
    let mut port = Rs485Port::open(&port_cfg)?;
    let backend = BusBackend;

    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mb-rtu-slave".into())
        .spawn(move || {
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
        });
    health::reset_thread_core();
    result.map_err(|e| crate::error::AppError::Modbus(format!("spawn: {e}")))?;

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

fn build_response(backend: &BusBackend, slave: u8, func: u8, pdu: &[u8]) -> heapless::Vec<u8, 256> {
    let mut out: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = out.push(slave);
    // 无堆分配: handle_pdu 写入栈缓冲区
    let mut pdu_buf = [0u8; crate::modbus::shared::PDU_BUF_SIZE];
    let pdu_len = crate::modbus::shared::handle_pdu(backend, func, pdu, &mut pdu_buf);
    if pdu_len >= 2 {
        // handle_pdu 写完整 PDU: [func, body...] 或 [func|0x80, code]
        let _ = out.extend_from_slice(&pdu_buf[..pdu_len]);
    }
    let crc = modbus_crc16(&out);
    let _ = out.push(crc as u8);
    let _ = out.push((crc >> 8) as u8);
    out
}
