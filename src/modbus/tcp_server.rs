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
    for &port in cfg::PORTS {
        spawn_listener(port)?;
    }
    log::info!("[mb-tcp] listening on {} ports: {:?}", cfg::PORTS.len(), cfg::PORTS);
    Ok(())
}

fn spawn_listener(port: u16) -> AppResult<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .map_err(|e| AppError::Modbus(format!("bind {}: {}", port, e)))?;

    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name(format!("mb-tcp-{}", port))
        
        .spawn(move || {
            log::info!("[mb-tcp] listening on :{}", port);
            // 设置非阻塞以确保心跳可以定期 tick (避免任务被误判为停滞)
            listener.set_nonblocking(true).ok();
            loop {
                LISTEN_HB.tick();
                let stream = match listener.accept() {
                    Ok((s, _addr)) => s,
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // 200ms 后再尝试, 期间心跳已 tick
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    Err(e) => {
                        log::warn!("[mb-tcp] accept: {}", e);
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                };
                let cur = CONN_COUNT.load(Ordering::SeqCst);
                if cur >= cfg::MAX_CONNECTIONS as u32 {
                    log::warn!("[mb-tcp] rejected, max={} reached", cfg::MAX_CONNECTIONS);
                    drop(stream);
                    continue;
                }
                CONN_COUNT.fetch_add(1, Ordering::SeqCst);
                let id = CONN_COUNT.load(Ordering::SeqCst);
                health::set_next_thread_core(health::CORE_NET);
                std::thread::Builder::new()
                    .name(format!("mb-tcp-conn-{id}"))
                    
                    .spawn(move || {
                        if let Err(e) = handle_conn(stream) {
                            log::debug!("[mb-tcp-conn-{id}] closed: {}", e);
                        }
                        CONN_COUNT.fetch_sub(1, Ordering::SeqCst);
                    })
                    .ok();
                health::reset_thread_core();
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
        if length < 2 || length > 254 {
            log::warn!("[mb-tcp] bad length {}", length);
            return Ok(());
        }

        // 读 PDU: 标准 Modbus TCP — MBAP length = unit_id + PDU
        // PDU 不含 unit_id, 首字节即为功能码
        let pdu_len = length - 1;  // 减去 MBAP 中的 unit_id
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
        if pdu_len < 1 { return Ok(()); }
        let func = pdu[0];  // PDU 首字节 = 功能码
        let resp_pdu = build_pdu(&backend, func, &pdu[1..pdu_len]);
        // MBAP.length 约定: 不含 unit_id (与 pymodbus/libmodbus 一致)
        //   响应格式: [MBAP(7)][PDU(N)], MBAP.length = PDU.len()
        //   unit_id 仅存在于 MBAP[6], 不重复出现在 PDU 前
        // 这样客户端可直接用 MBAP.length 读取 N 字节 PDU, 不需拆分
        let mbap_len = resp_pdu.len();
        let mut mbap = [0u8; 7];
        mbap[..2].copy_from_slice(&header[..2]); // echo tx_id
        mbap[2..4].copy_from_slice(&0u16.to_be_bytes()); // proto = 0
        mbap[4..6].copy_from_slice(&(mbap_len as u16).to_be_bytes());
        mbap[6] = unit_id;

        // 合并 MBAP + PDU 为单次写入, 避免 W5500 分批发送导致对端收到不完整帧
        // 优化: 使用栈缓冲区避免每次响应 1 次堆分配 (高频 Modbus TCP 关键)
        const MAX_RESP: usize = 280;
        let mut response = [0u8; MAX_RESP];
        let total_len = 7 + resp_pdu.len();
        response[..7].copy_from_slice(&mbap);
        response[7..total_len].copy_from_slice(&resp_pdu);
        stream.write_all(&response[..total_len])?;
        stream.flush()?;
    }
}

fn build_pdu(backend: &BusBackend, func: u8, payload: &[u8]) -> Vec<u8> {
    super::shared::handle_pdu(backend, func, payload)
}
