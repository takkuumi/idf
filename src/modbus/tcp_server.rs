//! Modbus TCP Server（四端口、单任务、多连接状态机）。
//!
//! 所有监听 socket 和最多 8 个客户端都由同一个非阻塞任务处理。连接建立不会
//! 创建 pthread，因此并发连接数不再线性消耗内部 SRAM，也不会因 pthread ENOMEM
//! 导致协议服务退出。MBAP/PDU 格式和四个监听端口保持不变。

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::config::modbus::tcp as cfg;
use crate::error::{AppError, AppResult};
use crate::health::{self, TaskHb};
use crate::modbus::shared::{BusBackend, PDU_BUF_SIZE};
use crate::safety::stack_budget;

const MBAP_PREFIX_LEN: usize = 6;
const MBAP_HEADER_LEN: usize = 7;
const MAX_MBAP_LENGTH: usize = 254;
const MAX_ADU_SIZE: usize = MBAP_PREFIX_LEN + MAX_MBAP_LENGTH;
const RX_BUFFER_SIZE: usize = MAX_ADU_SIZE * 2;

static CONN_COUNT: AtomicU32 = AtomicU32::new(0);
static NEXT_CONN_ID: AtomicU32 = AtomicU32::new(1);
static LISTEN_HB: TaskHb = TaskHb::new_with_stall("mb-tcp", 60);

struct Client {
    id: u32,
    peer: SocketAddr,
    stream: TcpStream,
    rx: [u8; RX_BUFFER_SIZE],
    rx_len: usize,
    tx: [u8; MAX_ADU_SIZE],
    tx_len: usize,
    tx_sent: usize,
    last_activity: Instant,
}

const CLIENT_STATE_ALLOCATION_BYTES: usize = std::mem::size_of::<Client>() * cfg::MAX_CONNECTIONS;
const _: () = assert!(CLIENT_STATE_ALLOCATION_BYTES > 4096);

impl Client {
    fn new(id: u32, peer: SocketAddr, stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            id,
            peer,
            stream,
            rx: [0; RX_BUFFER_SIZE],
            rx_len: 0,
            tx: [0; MAX_ADU_SIZE],
            tx_len: 0,
            tx_sent: 0,
            last_activity: Instant::now(),
        })
    }

    /// 返回 false 表示连接应关闭。每轮只完成有限工作，避免单客户端饿死其他连接。
    fn poll(&mut self, backend: &BusBackend) -> bool {
        if !self.flush_tx() {
            return false;
        }

        if self.tx_len == 0 && !self.process_buffered_request(backend) {
            return false;
        }
        if self.tx_len != 0 {
            return self.flush_tx() && !self.is_timed_out();
        }

        match self.stream.read(&mut self.rx[self.rx_len..]) {
            Ok(0) => return false,
            Ok(n) => {
                self.rx_len += n;
                self.last_activity = Instant::now();
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                log::debug!("[mb-tcp] conn_id={} read: {}", self.id, e);
                return false;
            }
        }

        if self.tx_len == 0 && !self.process_buffered_request(backend) {
            return false;
        }
        if self.tx_len != 0 && !self.flush_tx() {
            return false;
        }

        if self.is_timed_out() {
            log::debug!("[mb-tcp] conn_id={} idle timeout", self.id);
            return false;
        }
        true
    }

    fn is_timed_out(&self) -> bool {
        let timeout_ms = if self.tx_len == 0 {
            cfg::RX_TIMEOUT_MS
        } else {
            cfg::TX_TIMEOUT_MS
        };
        self.last_activity.elapsed() >= Duration::from_millis(timeout_ms)
    }

    fn flush_tx(&mut self) -> bool {
        while self.tx_sent < self.tx_len {
            match self.stream.write(&self.tx[self.tx_sent..self.tx_len]) {
                Ok(0) => return false,
                Ok(n) => {
                    self.tx_sent += n;
                    self.last_activity = Instant::now();
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::debug!("[mb-tcp] conn_id={} write: {}", self.id, e);
                    return false;
                }
            }
        }
        if self.tx_len != 0 {
            self.tx_len = 0;
            self.tx_sent = 0;
        }
        true
    }

    fn process_buffered_request(&mut self, backend: &BusBackend) -> bool {
        if self.rx_len < MBAP_PREFIX_LEN {
            return true;
        }

        let protocol = u16::from_be_bytes([self.rx[2], self.rx[3]]);
        let length = u16::from_be_bytes([self.rx[4], self.rx[5]]) as usize;
        if protocol != 0 || !(2..=MAX_MBAP_LENGTH).contains(&length) {
            log::warn!(
                "[mb-tcp] conn_id={} invalid MBAP protocol={} length={}",
                self.id,
                protocol,
                length
            );
            return false;
        }

        let request_len = MBAP_PREFIX_LEN + length;
        if self.rx_len < request_len {
            return true;
        }

        let response_len = match build_response(
            &self.rx[..request_len],
            backend,
            &mut self.tx,
        ) {
            Some(n) => n,
            None => return false,
        };
        self.tx_len = response_len;
        self.tx_sent = 0;

        self.rx.copy_within(request_len..self.rx_len, 0);
        self.rx_len -= request_len;
        true
    }
}

pub fn start() -> AppResult<()> {
    let mut listeners = Vec::new();
    listeners
        .try_reserve_exact(cfg::PORTS.len())
        .map_err(|e| AppError::Modbus(format!("listener allocation: {e}")))?;
    for &port in cfg::PORTS {
        let listener = TcpListener::bind(("0.0.0.0", port))
            .map_err(|e| AppError::Modbus(format!("bind {port}: {e}")))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| AppError::Modbus(format!("nonblocking {port}: {e}")))?;
        listeners.push(listener);
        log::info!("[mb-tcp] bound :{port}");
    }

    // 在 main 任务中一次性预留连接状态。运行中的 mb-tcp pthread 不扩容。
    let mut clients: Vec<Client> = Vec::new();
    clients
        .try_reserve_exact(cfg::MAX_CONNECTIONS)
        .map_err(|e| AppError::Modbus(format!("client state allocation: {e}")))?;
    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mb-tcp".into())
        .stack_size(stack_budget::MODBUS_TCP)
        .spawn(move || poll_loop(listeners, clients));
    health::reset_thread_core();

    if result.is_ok() {
        health::register_with_stack(&LISTEN_HB, stack_budget::MODBUS_TCP);
    }
    result.map_err(|e| AppError::Modbus(format!("spawn mb-tcp: {e}")))?;
    log::info!(
        "[mb-tcp] {} ports, max {} clients, one {}KB task",
        cfg::PORTS.len(),
        cfg::MAX_CONNECTIONS,
        stack_budget::MODBUS_TCP / stack_budget::KIB
    );
    Ok(())
}

fn poll_loop(listeners: Vec<TcpListener>, mut clients: Vec<Client>) {
    health::subscribe_wdt();
    let backend = BusBackend;
    loop {
        LISTEN_HB.tick();
        health::feed_wdt();
        accept_pending(&listeners, &mut clients);

        let mut i = 0;
        while i < clients.len() {
            if clients[i].poll(&backend) {
                i += 1;
            } else {
                let client = clients.swap_remove(i);
                log::info!(
                    "[mb-tcp] conn_id={} from {} closed",
                    client.id,
                    client.peer
                );
                CONN_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
        std::thread::sleep(Duration::from_millis(cfg::ACCEPT_POLL_INTERVAL_MS));
    }
}

fn accept_pending(listeners: &[TcpListener], clients: &mut Vec<Client>) {
    for listener in listeners {
        // 每端口每轮处理有限数量，持续 SYN/accept 洪泛不能饿死现有连接和 WDT。
        for _ in 0..cfg::MAX_CONNECTIONS {
            match listener.accept() {
                Ok((stream, peer)) if clients.len() >= cfg::MAX_CONNECTIONS => {
                    log::warn!("[mb-tcp] rejected {peer} (max={} reached)", cfg::MAX_CONNECTIONS);
                    drop(stream);
                }
                Ok((stream, peer)) => {
                    let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
                    match Client::new(id, peer, stream) {
                        Ok(client) => {
                            clients.push(client);
                            CONN_COUNT.store(clients.len() as u32, Ordering::Relaxed);
                            log::info!("[mb-tcp] conn_id={id} from {peer} accepted");
                        }
                        Err(e) => log::warn!("[mb-tcp] configure {peer}: {e}"),
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::warn!("[mb-tcp] accept: {e}");
                    break;
                }
            }
        }
    }
}

fn build_response(
    request: &[u8],
    backend: &BusBackend,
    response: &mut [u8; MAX_ADU_SIZE],
) -> Option<usize> {
    if request.len() < MBAP_HEADER_LEN + 1 {
        return None;
    }
    let func = request[7];
    let mut pdu = [0u8; PDU_BUF_SIZE];
    let pdu_len = crate::modbus::shared::handle_pdu(backend, func, &request[8..], &mut pdu);
    let total_len = MBAP_HEADER_LEN + pdu_len;
    if total_len > response.len() {
        return None;
    }

    response[..2].copy_from_slice(&request[..2]);
    response[2..4].copy_from_slice(&0u16.to_be_bytes());
    response[4..6].copy_from_slice(&((1 + pdu_len) as u16).to_be_bytes());
    response[6] = request[6];
    response[7..total_len].copy_from_slice(&pdu[..pdu_len]);
    Some(total_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tcp_stack_is_constant_for_all_connections() {
        assert_eq!(stack_budget::MODBUS_TCP, 16 * 1024);
        assert_eq!(cfg::MAX_CONNECTIONS, 8);
    }

    #[test]
    fn test_invalid_mbap_lengths_are_rejected() {
        assert!(!(2..=MAX_MBAP_LENGTH).contains(&1));
        assert!(!(2..=MAX_MBAP_LENGTH).contains(&255));
        assert!((2..=MAX_MBAP_LENGTH).contains(&2));
    }
}
