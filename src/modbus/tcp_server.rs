//! Modbus TCP Server
//!
//! 监听 TCP 502 端口, 多连接 (MAX_CONNECTIONS=4)。
//! 解析 MBAP header (7 字节) 后剥离 unit_id, 处理 PDU。
//! 不依赖 umodbus crate, 手写 MBAP + PDU 处理。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::config::modbus::tcp as cfg;
use crate::error::{AppError, AppResult};
use crate::health::{self, TaskHb};
use crate::modbus::shared::{exc, BusBackend, ModbusBackend};

static CONN_COUNT: AtomicU32 = AtomicU32::new(0);

/// 监听任务心跳 (静态分配)
/// 阈值放宽到 60 (监听 accept 长时间阻塞, 允许较长静默)
static LISTEN_HB: TaskHb = TaskHb::new_with_stall("mb-tcp-listen", 60);

pub fn start() -> AppResult<()> {
    health::register(&LISTEN_HB);
    let listener = TcpListener::bind(("0.0.0.0", cfg::PORT))
        .map_err(|e| AppError::Modbus(format!("bind {}: {}", cfg::PORT, e)))?;

    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mb-tcp-listen".into())
        .spawn(move || {
            log::info!("[mb-tcp] listening on :{}", cfg::PORT);
            for stream in listener.incoming() {
                // 心跳: 每次 accept 返回 (有连接或错误)
                LISTEN_HB.tick();
                match stream {
                    Ok(s) => {
                        let cur = CONN_COUNT.load(Ordering::SeqCst);
                        if cur >= cfg::MAX_CONNECTIONS as u32 {
                            log::warn!("[mb-tcp] rejected, max={} reached", cfg::MAX_CONNECTIONS);
                            drop(s);
                            continue;
                        }
                        CONN_COUNT.fetch_add(1, Ordering::SeqCst);

                        let id = CONN_COUNT.load(Ordering::SeqCst);
                        health::set_next_thread_core(health::CORE_NET);
                        std::thread::Builder::new()
                            .name(format!("mb-tcp-conn-{id}"))
                            .spawn(move || {
                                if let Err(e) = handle_conn(s) {
                                    log::debug!("[mb-tcp-conn-{id}] closed: {}", e);
                                }
                                CONN_COUNT.fetch_sub(1, Ordering::SeqCst);
                            })
                            .ok();
                        health::reset_thread_core();
                    }
                    Err(e) => log::warn!("[mb-tcp] accept: {}", e),
                }
            }
        });
    health::reset_thread_core();
    result.map_err(|e| AppError::Modbus(format!("spawn: {e}")))?;

    Ok(())
}

fn handle_conn(mut stream: TcpStream) -> AppResult<()> {
    stream.set_read_timeout(Some(Duration::from_millis(cfg::RX_TIMEOUT_MS)))?;
    stream.set_write_timeout(Some(Duration::from_millis(cfg::TX_TIMEOUT_MS)))?;
    // keepalive: 空闲超过 RX_TIMEOUT_MS 的连接由 set_read_timeout 触发 WouldBlock,
    // 下次循环返回 WouldBlock 时主动关闭, 释放 MAX_CONNECTIONS 名额

    let backend = BusBackend;
    let mut header = [0u8; 7]; // MBAP: tx_id(2) + proto(2) + length(2) + unit(1)
    let mut pdu = [0u8; 253]; // PDU 最大 253 字节

    loop {
        // 读 MBAP header
        // 区分: Ok = 继续, WouldBlock = keepalive 超时, 其他错误 = 关闭
        match stream.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                log::info!("[mb-tcp] keepalive timeout, closing");
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                log::debug!("[mb-tcp] peer closed");
                return Ok(());
            }
            Err(e) => {
                log::debug!("[mb-tcp] read err: {}", e);
                return Ok(());
            }
        }

        let proto = u16::from_be_bytes([header[2], header[3]]);
        if proto != 0 {
            log::warn!("[mb-tcp] proto != 0 ({})", proto);
            return Ok(());
        }

        let length = u16::from_be_bytes([header[4], header[5]]) as usize;
        if length < 2 || length > 253 + 1 {
            log::warn!("[mb-tcp] bad length {}", length);
            return Ok(());
        }

        // 读 PDU
        let pdu_len = length - 1;
        match stream.read_exact(&mut pdu[..pdu_len]) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                log::debug!("[mb-tcp] peer closed (mid pdu)");
                return Ok(());
            }
            Err(e) => {
                log::debug!("[mb-tcp] pdu read err: {}", e);
                return Ok(());
            }
        }

        let unit_id = header[6];
        let func = pdu[0];
        let resp_pdu = build_pdu(&backend, func, &pdu[1..pdu_len]);
        let resp_len = 1 + resp_pdu.len();
        let mut mbap = [0u8; 7];
        mbap[..2].copy_from_slice(&header[..2]); // echo tx_id
        mbap[2..4].copy_from_slice(&0u16.to_be_bytes()); // proto = 0
        mbap[4..6].copy_from_slice(&(resp_len as u16).to_be_bytes());
        mbap[6] = unit_id;

        stream.write_all(&mbap)?;
        stream.write_all(&resp_pdu)?;
        stream.flush()?;
    }
}

fn build_pdu(backend: &BusBackend, func: u8, payload: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.push(func);

    let body: Vec<u8> = match func {
        0x01 => read_bits_pdu(backend, payload, |b, a, c| b.read_coils(a, c)),
        0x02 => read_bits_pdu(backend, payload, |b, a, c| b.read_discrete_inputs(a, c)),
        0x03 => read_regs_pdu(backend, payload, |b, a, c| b.read_holding_registers(a, c)),
        0x04 => read_regs_pdu(backend, payload, |b, a, c| b.read_input_registers(a, c)),
        0x05 => write_single_coil(backend, payload),
        0x06 => write_single_reg(backend, payload),
        0x0F => write_multi_coils(backend, payload),
        0x10 => write_multi_regs(backend, payload),
        _ => return vec![func | 0x80, exc::ILLEGAL_FUNCTION],
    };

    out.extend_from_slice(&body);
    out
}

fn read_bits_pdu<F>(backend: &BusBackend, pdu: &[u8], f: F) -> Vec<u8>
where
    F: Fn(&BusBackend, u16, u16) -> Vec<bool>,
{
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 2000 {
        return vec![exc::ILLEGAL_DATA_VALUE];
    }
    let bits = f(backend, addr, count);
    let byte_count = ((count as usize) + 7) / 8;
    let mut out = Vec::with_capacity(1 + byte_count);
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

fn read_regs_pdu<F>(backend: &BusBackend, pdu: &[u8], f: F) -> Vec<u8>
where
    F: Fn(&BusBackend, u16, u16) -> Vec<u16>,
{
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 125 {
        return vec![exc::ILLEGAL_DATA_VALUE];
    }
    let regs = f(backend, addr, count);
    let mut out = Vec::with_capacity(1 + regs.len() * 2);
    out.push((regs.len() * 2) as u8);
    for r in regs {
        out.extend_from_slice(&r.to_be_bytes());
    }
    out
}

fn write_single_coil(backend: &BusBackend, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    let on = value == 0xFF00;
    if !on && value != 0 {
        return vec![exc::ILLEGAL_DATA_VALUE];
    }
    if !backend.write_single_coil(addr, on) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    pdu.to_vec()
}

fn write_single_reg(backend: &BusBackend, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    if !backend.write_single_register(addr, value) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    pdu.to_vec()
}

fn write_multi_coils(backend: &BusBackend, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 5 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    if pdu.len() < 5 + byte_count {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let mut bits = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        bits.push(pdu[5 + i / 8] & (1 << (i % 8)) != 0);
    }
    if !backend.write_multiple_coils(addr, &bits) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    pdu[..4].to_vec()
}

fn write_multi_regs(backend: &BusBackend, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 5 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    if pdu.len() < 5 + byte_count || byte_count != count as usize * 2 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let mut regs = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        regs.push(u16::from_be_bytes([pdu[5 + 2 * i], pdu[6 + 2 * i]]));
    }
    if !backend.write_multiple_registers(addr, &regs) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    pdu[..4].to_vec()
}
