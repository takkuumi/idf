//! Modbus TCP Server（四端口、主循环多连接状态机）。
//!
//! 所有监听 socket 和最多 8 个客户端都由 main_loop 的 5ms 非阻塞 tick 处理。
//! 模块不创建 pthread，因此不再申请 16KB internal SRAM 任务栈，连接建立也不会
//! 增加任务数量。MBAP/PDU 格式和四个监听端口保持不变。

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::config::modbus::tcp as cfg;
use crate::error::{AppError, AppResult};
use crate::modbus::shared::{
    BusBackend, MODBUS_TCP_MAX_ADU_LEN, MODBUS_TCP_MAX_MBAP_LENGTH, PDU_BUF_SIZE,
};
use crate::sync::MainLoopCell;

const MBAP_PREFIX_LEN: usize = 6;
const MBAP_HEADER_LEN: usize = 7;
const MAX_MBAP_LENGTH: usize = MODBUS_TCP_MAX_MBAP_LENGTH;
const MAX_ADU_SIZE: usize = MODBUS_TCP_MAX_ADU_LEN;
/// 同一连接在单次 5ms tick 内最多处理的流水请求数。
/// 上限 4 可消除“每 tick 一帧”的吞吐瓶颈，同时保证 8 客户端公平调度。
const MAX_REQUESTS_PER_POLL: usize = 4;
/// 服务器每个 5ms tick 的总请求预算。标准最大 FC03 响应实机峰值约 6ms，
/// 因此每轮只执行一个业务请求；已有响应发送和超时检查仍会轮询所有客户端。
const MAX_REQUESTS_PER_TICK: usize = 1;
/// 每轮最多接收两个新连接，并在四个监听端口间轮转，连接风暴不能占满主循环。
const MAX_ACCEPTS_PER_TICK: usize = 2;
const LISTENER_RECOVERY_DELAY: Duration = Duration::from_secs(1);
const LISTENER_RECOVERY_BACKOFF: Duration = Duration::from_secs(5);
const REJECT_LOG_INTERVAL: Duration = Duration::from_secs(10);
const _: () = assert!(MAX_ADU_SIZE == MBAP_PREFIX_LEN + MAX_MBAP_LENGTH);

static CONN_COUNT: AtomicU32 = AtomicU32::new(0);
static NEXT_CONN_ID: AtomicU32 = AtomicU32::new(1);

struct Client {
    id: u32,
    peer: SocketAddr,
    stream: TcpStream,
    // 一个完整标准 ADU 足够：首帧处理后其余流水数据继续保留在 socket 接收队列。
    rx: [u8; MAX_ADU_SIZE],
    rx_len: usize,
    tx: [u8; MAX_ADU_SIZE],
    tx_len: usize,
    tx_sent: usize,
    last_activity: Instant,
    partial_frame_started: Option<Instant>,
    tx_started: Option<Instant>,
}

const CLIENT_STATE_ALLOCATION_BYTES: usize = std::mem::size_of::<Client>() * cfg::MAX_CONNECTIONS;
const _: () = assert!(CLIENT_STATE_ALLOCATION_BYTES > 4096);

impl Client {
    fn new(id: u32, peer: SocketAddr, stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        if let Err(error) = configure_keepalive(&stream) {
            // keepalive 是半开连接回收增强项；某些旧 LwIP 构建可能不支持
            // TCP_KEEP* 细项，不能因此拒绝一个本可正常服务的客户端。
            log::warn!("[mb-tcp] keepalive setup for {peer} incomplete: {error}");
        }
        Ok(Self {
            id,
            peer,
            stream,
            rx: [0; MAX_ADU_SIZE],
            rx_len: 0,
            tx: [0; MAX_ADU_SIZE],
            tx_len: 0,
            tx_sent: 0,
            last_activity: Instant::now(),
            partial_frame_started: None,
            tx_started: None,
        })
    }

    /// 返回 false 表示连接应关闭。每轮只完成有限工作，避免单客户端饿死其他连接。
    fn poll(&mut self, backend: &BusBackend, budget: &mut usize) -> bool {
        for _ in 0..MAX_REQUESTS_PER_POLL {
            if !self.flush_tx() {
                return false;
            }
            // socket 发送窗口已满，保留响应并立即让出，不继续读新请求。
            if self.tx_len != 0 {
                break;
            }
            if *budget == 0 {
                break;
            }

            if !self.process_buffered_request(backend) {
                return false;
            }
            if self.tx_len != 0 {
                *budget = budget.saturating_sub(1);
                continue;
            }

            match self.stream.read(&mut self.rx[self.rx_len..]) {
                Ok(0) => return false,
                Ok(n) => {
                    if self.rx_len == 0 {
                        self.partial_frame_started = Some(Instant::now());
                    }
                    self.rx_len += n;
                    self.last_activity = Instant::now();
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::debug!("[mb-tcp] conn_id={} read: {}", self.id, e);
                    return false;
                }
            }

            if !self.process_buffered_request(backend) {
                return false;
            }
            if self.tx_len != 0 {
                *budget = budget.saturating_sub(1);
            }
        }

        if self.is_timed_out() {
            log::debug!("[mb-tcp] conn_id={} idle timeout", self.id);
            return false;
        }
        true
    }

    fn is_timed_out(&self) -> bool {
        if self.partial_frame_started.is_some_and(|started| {
            started.elapsed() >= Duration::from_millis(cfg::PARTIAL_FRAME_TIMEOUT_MS)
        }) {
            return true;
        }
        if self
            .tx_started
            .is_some_and(|started| started.elapsed() >= Duration::from_millis(cfg::TX_TIMEOUT_MS))
        {
            return true;
        }
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
            self.tx_started = None;
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
            log::debug!(
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
        self.tx_started = Some(Instant::now());

        self.rx.copy_within(request_len..self.rx_len, 0);
        self.rx_len -= request_len;
        if self.rx_len == 0 {
            self.partial_frame_started = None;
        }
        true
    }
}

fn configure_keepalive(stream: &TcpStream) -> std::io::Result<()> {
    let fd = stream.as_raw_fd();
    set_socket_option(fd, esp_idf_sys::SOL_SOCKET, esp_idf_sys::SO_KEEPALIVE, 1)?;
    set_socket_option(
        fd,
        esp_idf_sys::IPPROTO_TCP,
        esp_idf_sys::TCP_KEEPIDLE,
        cfg::KEEPALIVE_IDLE_S,
    )?;
    set_socket_option(
        fd,
        esp_idf_sys::IPPROTO_TCP,
        esp_idf_sys::TCP_KEEPINTVL,
        cfg::KEEPALIVE_INTERVAL_S,
    )?;
    set_socket_option(
        fd,
        esp_idf_sys::IPPROTO_TCP,
        esp_idf_sys::TCP_KEEPCNT,
        cfg::KEEPALIVE_COUNT,
    )
}

fn set_socket_option(fd: i32, level: u32, name: u32, value: i32) -> std::io::Result<()> {
    let result = unsafe {
        esp_idf_sys::lwip_setsockopt(
            fd,
            level as i32,
            name as i32,
            &value as *const _ as *const _,
            std::mem::size_of_val(&value) as u32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

struct TcpServerState {
    listeners: Vec<TcpListener>,
    clients: Vec<Client>,
    active_ports: [u16; 4],
    next_port_check: Instant,
    client_cursor: usize,
    listener_cursor: usize,
    listener_rebind_due: Option<Instant>,
    next_reject_log: Instant,
    rejected_connections: u32,
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
            client_cursor: 0,
            listener_cursor: 0,
            listener_rebind_due: None,
            next_reject_log: Instant::now(),
            rejected_connections: 0,
        })
        .map_err(|_| AppError::Modbus("TCP state busy during init".into()))?;
    log::info!(
        "[mb-tcp] {} ports, max {} clients, main-loop polling",
        ports.len(),
        cfg::MAX_CONNECTIONS
    );
    Ok(())
}

/// main_loop 每 5ms 调用一次。所有 socket 均为 nonblocking，每轮工作有界。
pub fn tick_tcp_server() {
    let _ = SERVER_STATE.with_mut(|state| {
        let now = Instant::now();
        recover_listeners_if_due(state, now);
        if now >= state.next_port_check {
            state.next_port_check = now + Duration::from_secs(1);
            let desired = configured_ports();
            if desired != state.active_ports {
                if !ports_are_valid(&desired) {
                    log::error!(
                        "[mb-tcp] rejected invalid/duplicate port set: {:?}",
                        desired
                    );
                } else {
                    match rebind_ports_transactional(
                        &state.active_ports,
                        &desired,
                        &mut state.listeners,
                    ) {
                        Ok(()) => {
                            state.active_ports = desired;
                            state.listener_cursor = 0;
                            state.listener_rebind_due = None;
                            log::info!("[mb-tcp] listeners rebound: {:?}", state.active_ports);
                        }
                        Err((port, e)) => {
                            log::error!(
                                "[mb-tcp] rebind :{} failed: {}; old listeners retained",
                                port,
                                e
                            );
                        }
                    }
                }
            }
            if state.listeners.len() != state.active_ports.len()
                || state
                    .listeners
                    .iter()
                    .any(|listener| !matches!(listener.take_error(), Ok(None)))
            {
                schedule_listener_recovery(state, now);
            }
        }
        if accept_pending(state, now) {
            schedule_listener_recovery(state, now);
        }

        let mut request_budget = MAX_REQUESTS_PER_TICK;
        let client_count = state.clients.len();
        let start = state.client_cursor.min(client_count.saturating_sub(1));
        let mut dead = [false; cfg::MAX_CONNECTIONS];
        for offset in 0..client_count {
            let index = (start + offset) % client_count;
            // 每个客户端每轮最多消费一个全局请求额度；发送已有响应和超时检查
            // 不受额度影响，避免前序繁忙连接让后序连接永久饥饿。
            let mut client_budget = request_budget.min(1);
            let before = client_budget;
            if !state.clients[index].poll(&BusBackend, &mut client_budget) {
                dead[index] = true;
            }
            request_budget -= before - client_budget;
        }
        let next_id = if client_count == 0 {
            None
        } else {
            Some(state.clients[(start + MAX_REQUESTS_PER_TICK) % client_count].id)
        };
        for i in (0..client_count).rev() {
            if dead[i] {
                let client = state.clients.swap_remove(i);
                log::debug!("[mb-tcp] conn_id={} from {} closed", client.id, client.peer);
                CONN_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
        }
        state.client_cursor = next_id
            .and_then(|id| state.clients.iter().position(|client| client.id == id))
            .unwrap_or(0);
    });
}

fn schedule_listener_recovery(state: &mut TcpServerState, now: Instant) {
    if state.listener_rebind_due.is_none() {
        state.listener_rebind_due = Some(now + LISTENER_RECOVERY_DELAY);
        log::warn!("[mb-tcp] listener fault detected; recovery scheduled");
    }
}

fn recover_listeners_if_due(state: &mut TcpServerState, now: Instant) {
    if state.listener_rebind_due.is_none_or(|due| now < due) {
        return;
    }
    match bind_ports(&state.active_ports, &mut state.listeners) {
        Ok(()) => {
            state.listener_cursor = 0;
            state.listener_rebind_due = None;
            log::info!("[mb-tcp] listeners recovered: {:?}", state.active_ports);
        }
        Err((port, error)) => {
            state.listener_rebind_due = Some(now + LISTENER_RECOVERY_BACKOFF);
            log::warn!("[mb-tcp] listener recovery :{} failed: {}", port, error);
        }
    }
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

/// 启动和故障恢复使用的完整绑定。调用时会关闭现有 listener；运行期正常配置
/// 切换必须使用 `rebind_ports_transactional` 保留旧服务直到新端口全部成功。
fn bind_ports(
    ports: &[u16; 4],
    listeners: &mut Vec<TcpListener>,
) -> Result<(), (u16, std::io::Error)> {
    listeners.clear();
    for &port in ports {
        let listener = bind_listener(port)?;
        listeners.push(listener);
        log::info!("[mb-tcp] bound :{port}");
    }
    Ok(())
}

fn bind_listener(port: u16) -> Result<TcpListener, (u16, std::io::Error)> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| (port, e))?;
    listener.set_nonblocking(true).map_err(|e| (port, e))?;
    Ok(listener)
}

/// 先绑定全部新增端口，只有都成功后才移动保留端口并关闭废弃 listener。
/// 这样错误端口或暂时的 LwIP 资源不足不会中断仍在工作的旧 TCP 服务。
fn rebind_ports_transactional(
    old_ports: &[u16; 4],
    desired: &[u16; 4],
    listeners: &mut Vec<TcpListener>,
) -> Result<(), (u16, std::io::Error)> {
    if listeners.len() != old_ports.len() {
        return Err((
            desired[0],
            std::io::Error::other("listener state does not match active ports"),
        ));
    }

    let mut replacements: Vec<(u16, TcpListener)> = Vec::new();
    replacements
        .try_reserve_exact(desired.len())
        .map_err(|error| {
            (
                desired[0],
                std::io::Error::other(format!("listener allocation: {error}")),
            )
        })?;
    for &port in desired {
        if !old_ports.contains(&port) {
            replacements.push((port, bind_listener(port)?));
        }
    }

    let mut next = Vec::new();
    next.try_reserve_exact(desired.len()).map_err(|error| {
        (
            desired[0],
            std::io::Error::other(format!("listener allocation: {error}")),
        )
    })?;
    for &port in desired {
        if let Some(old_index) = old_ports.iter().position(|&old_port| old_port == port) {
            let listener = listeners[old_index]
                .try_clone()
                .map_err(|error| (port, error))?;
            next.push(listener);
        } else {
            let Some(replacement_index) = replacements
                .iter()
                .position(|(new_port, _)| *new_port == port)
            else {
                return Err((port, std::io::Error::other("new listener missing")));
            };
            next.push(replacements.swap_remove(replacement_index).1);
        }
        log::info!("[mb-tcp] bound :{port}");
    }
    *listeners = next;
    Ok(())
}

fn accept_pending(state: &mut TcpServerState, now: Instant) -> bool {
    let listener_count = state.listeners.len();
    if listener_count == 0 {
        return true;
    }
    let mut accepted = 0;
    let mut faulted = false;
    for offset in 0..listener_count {
        if accepted >= MAX_ACCEPTS_PER_TICK {
            break;
        }
        let index = (state.listener_cursor + offset) % listener_count;
        match state.listeners[index].accept() {
            Ok((stream, _peer)) if state.clients.len() >= cfg::MAX_CONNECTIONS => {
                state.rejected_connections = state.rejected_connections.saturating_add(1);
                if now >= state.next_reject_log {
                    log::warn!(
                        "[mb-tcp] max connections reached; rejected={} in interval",
                        state.rejected_connections
                    );
                    state.rejected_connections = 0;
                    state.next_reject_log = now + REJECT_LOG_INTERVAL;
                }
                drop(stream);
                accepted += 1;
            }
            Ok((stream, peer)) => {
                accepted += 1;
                let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
                match Client::new(id, peer, stream) {
                    Ok(client) => {
                        state.clients.push(client);
                        CONN_COUNT.store(state.clients.len() as u32, Ordering::Relaxed);
                        log::debug!("[mb-tcp] conn_id={id} from {peer} accepted");
                    }
                    Err(e) => log::warn!("[mb-tcp] configure {peer}: {e}"),
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                log::warn!("[mb-tcp] accept: {e}");
                faulted = true;
            }
        }
    }
    state.listener_cursor = (state.listener_cursor + 1) % listener_count;
    faulted
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
        assert_eq!(MAX_REQUESTS_PER_POLL, 4);
        assert_eq!(MAX_REQUESTS_PER_TICK, 1);
        assert_eq!(MAX_ACCEPTS_PER_TICK, 2);
        assert!(cfg::PARTIAL_FRAME_TIMEOUT_MS < cfg::IDLE_TIMEOUT_MS);
        assert!(cfg::KEEPALIVE_COUNT > 0);
        assert!(CLIENT_STATE_ALLOCATION_BYTES > 4096);
    }

    #[test]
    fn test_invalid_mbap_lengths_are_rejected() {
        assert!(!(2..=MAX_MBAP_LENGTH).contains(&1));
        assert!(!(2..=MAX_MBAP_LENGTH).contains(&255));
        assert!((2..=MAX_MBAP_LENGTH).contains(&2));
        assert!((2..=MAX_MBAP_LENGTH).contains(&MODBUS_TCP_MAX_MBAP_LENGTH));
        assert_eq!(MAX_ADU_SIZE, 260);
    }

    #[test]
    fn test_mbap_exception_response_golden_vector() {
        let request = [0x12, 0x34, 0, 0, 0, 2, 1, 0x7F];
        let mut response = [0u8; MAX_ADU_SIZE];
        let len = build_response(&request, &BusBackend, &mut response).expect("response");
        assert_eq!(&response[..len], &[0x12, 0x34, 0, 0, 0, 3, 1, 0xFF, 0x01]);
    }

    #[test]
    fn test_mbap_read_response_has_exact_length_no_trailing_zero() {
        let request = [0x12, 0x35, 0, 0, 0, 6, 1, 0x03, 0x08, 0xA5, 0, 1];
        let mut response = [0u8; MAX_ADU_SIZE];
        let len = build_response(&request, &BusBackend, &mut response).expect("response");
        assert_eq!(len, 11);
        assert_eq!(&response[4..6], &[0, 5]); // unit(1) + PDU(4)
        assert_eq!(&response[7..9], &[0x03, 0x02]);
    }

    #[test]
    fn test_pc_device_mmp_83_word_tcp_response() {
        let request = [0x22, 0x13, 0, 0, 0, 6, 1, 0x03, 0x08, 0x94, 0, 83];
        let mut response = [0u8; MAX_ADU_SIZE];
        let len = build_response(&request, &BusBackend, &mut response).expect("response");
        assert_eq!(len, 7 + 2 + 83 * 2);
        assert_eq!(u16::from_be_bytes([response[4], response[5]]), 169);
        assert_eq!(&response[7..9], &[0x03, 166]);
    }

    #[test]
    fn test_tcp_port_set_validation() {
        assert!(ports_are_valid(&[502, 503, 504, 5002]));
        assert!(!ports_are_valid(&[502, 502, 504, 5002]));
        assert!(!ports_are_valid(&[502, 503, 0, 5002]));
    }
}
