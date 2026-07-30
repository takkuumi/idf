//! Modbus TCP Server（四端口、主循环多连接状态机）。
//!
//! 所有监听 socket 和最多 8 个客户端都由 main_loop 的 20ms 非阻塞 tick 处理。
//! 模块不创建 pthread，因此不再申请 16KB internal SRAM 任务栈，连接建立也不会
//! 增加任务数量。MBAP/PDU 格式和四个监听端口保持不变。

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::config::modbus::tcp as cfg;
use crate::error::{AppError, AppResult};
use crate::modbus::shared::{BusBackend, PDU_BUF_SIZE};
use crate::sync::MainLoopCell;

const MBAP_PREFIX_LEN: usize = 6;
const MBAP_HEADER_LEN: usize = 7;
const MAX_MBAP_LENGTH: usize = 254;
const MAX_ADU_SIZE: usize = MBAP_PREFIX_LEN + MAX_MBAP_LENGTH;
const RX_BUFFER_SIZE: usize = MAX_ADU_SIZE * 2;

static CONN_COUNT: AtomicU32 = AtomicU32::new(0);
static NEXT_CONN_ID: AtomicU32 = AtomicU32::new(1);

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
            cfg::IDLE_TIMEOUT_MS
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

        let response_len = match build_response(&self.rx[..request_len], backend, &mut self.tx) {
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

struct TcpServerState {
    listeners: Vec<TcpListener>,
    clients: Vec<Client>,
    active_ports: [u16; 4],
    next_port_check: Instant,
}

static SERVER_STATE: MainLoopCell<TcpServerState> = MainLoopCell::new();

pub fn start() -> AppResult<()> {
    if SERVER_STATE.is_initialized() {
        return Ok(());
    }
    let ports = configured_ports();
    if !ports_are_valid(&ports) {
        return Err(AppError::Modbus(format!(
            "invalid/duplicate TCP port set: {ports:?}"
        )));
    }

    // 先预留全部连接状态，再创建 socket；失败时不会留下部分监听器占用 LwIP 堆。
    let mut clients: Vec<Client> = Vec::new();
    clients
        .try_reserve_exact(cfg::MAX_CONNECTIONS)
        .map_err(|e| AppError::Modbus(format!("client state allocation: {e}")))?;
    let mut listeners = Vec::new();
    listeners
        .try_reserve_exact(ports.len())
        .map_err(|e| AppError::Modbus(format!("listener allocation: {e}")))?;
    bind_ports(&ports, &mut listeners)
        .map_err(|(port, e)| AppError::Modbus(format!("bind {port}: {e}")))?;

    SERVER_STATE
        .init(TcpServerState {
            listeners,
            clients,
            active_ports: ports,
            next_port_check: Instant::now(),
        })
        .map_err(|_| AppError::Modbus("TCP state busy during init".into()))?;
    log::info!(
        "[mb-tcp] {} ports, max {} clients, main-loop polling",
        ports.len(),
        cfg::MAX_CONNECTIONS
    );
    Ok(())
}

/// main_loop 每 20ms 调用一次。所有 socket 均为 nonblocking，每轮工作有界。
pub fn tick_tcp_server() {
    let _ = SERVER_STATE.with_mut(|state| {
        if Instant::now() >= state.next_port_check {
            state.next_port_check = Instant::now() + Duration::from_secs(1);
            let desired = configured_ports();
            if desired != state.active_ports {
                if !ports_are_valid(&desired) {
                    log::error!(
                        "[mb-tcp] rejected invalid/duplicate port set: {:?}",
                        desired
                    );
                } else {
                    let previous = state.active_ports;
                    match bind_ports(&desired, &mut state.listeners) {
                        Ok(()) => {
                            state.active_ports = desired;
                            log::info!("[mb-tcp] listeners rebound: {:?}", state.active_ports);
                        }
                        Err((port, e)) => {
                            log::error!(
                                "[mb-tcp] rebind :{} failed: {}; restoring {:?}",
                                port,
                                e,
                                previous
                            );
                            if let Err((rollback_port, rollback_error)) =
                                bind_ports(&previous, &mut state.listeners)
                            {
                                log::error!(
                                    "[mb-tcp] listener rollback :{} failed: {} (will retry)",
                                    rollback_port,
                                    rollback_error
                                );
                            }
                        }
                    }
                }
            }
        }
        accept_pending(&state.listeners, &mut state.clients);

        let mut i = 0;
        while i < state.clients.len() {
            if state.clients[i].poll(&BusBackend) {
                i += 1;
            } else {
                let client = state.clients.swap_remove(i);
                log::info!("[mb-tcp] conn_id={} from {} closed", client.id, client.peer);
                CONN_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
    });
}

fn configured_ports() -> [u16; 4] {
    crate::bus::config_state::config_read_with(|state| state.cfg.tcp_ports)
        .unwrap_or(crate::config::regs::TCP_PORTS_DEFAULT)
}

fn ports_are_valid(ports: &[u16; 4]) -> bool {
    ports.iter().all(|&port| port != 0)
        && ports
            .iter()
            .enumerate()
            .all(|(i, port)| !ports[..i].contains(port))
}

/// 复用启动期预留的 Vec 容量。运行中的 TCP 状态机不扩容；发生失败时调用方
/// 可用旧端口集回滚，已建立的客户端 socket 不受 listener 重绑影响。
fn bind_ports(
    ports: &[u16; 4],
    listeners: &mut Vec<TcpListener>,
) -> Result<(), (u16, std::io::Error)> {
    listeners.clear();
    for &port in ports {
        let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| (port, e))?;
        listener.set_nonblocking(true).map_err(|e| (port, e))?;
        listeners.push(listener);
        log::info!("[mb-tcp] bound :{port}");
    }
    Ok(())
}

fn accept_pending(listeners: &[TcpListener], clients: &mut Vec<Client>) {
    for listener in listeners {
        // 每端口每轮处理有限数量，持续 SYN/accept 洪泛不能饿死现有连接和 WDT。
        for _ in 0..cfg::MAX_CONNECTIONS {
            match listener.accept() {
                Ok((stream, peer)) if clients.len() >= cfg::MAX_CONNECTIONS => {
                    log::warn!(
                        "[mb-tcp] rejected {peer} (max={} reached)",
                        cfg::MAX_CONNECTIONS
                    );
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
    fn test_tcp_uses_one_bounded_main_loop_state() {
        assert_eq!(cfg::MAX_CONNECTIONS, 8);
        assert!(CLIENT_STATE_ALLOCATION_BYTES > 4096);
    }

    #[test]
    fn test_invalid_mbap_lengths_are_rejected() {
        assert!(!(2..=MAX_MBAP_LENGTH).contains(&1));
        assert!(!(2..=MAX_MBAP_LENGTH).contains(&255));
        assert!((2..=MAX_MBAP_LENGTH).contains(&2));
    }

    #[test]
    fn test_mbap_exception_response_golden_vector() {
        let request = [0x12, 0x34, 0, 0, 0, 2, 1, 0x7F];
        let mut response = [0u8; MAX_ADU_SIZE];
        let len = build_response(&request, &BusBackend, &mut response).expect("response");
        assert_eq!(&response[..len], &[0x12, 0x34, 0, 0, 0, 3, 1, 0xFF, 0x01]);
    }

    #[test]
    fn test_tcp_port_set_validation() {
        assert!(ports_are_valid(&[502, 503, 504, 5002]));
        assert!(!ports_are_valid(&[502, 502, 504, 5002]));
        assert!(!ports_are_valid(&[502, 503, 0, 5002]));
    }
}
