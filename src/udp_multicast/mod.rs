//! UDP 组播同步接收 (MCA 一体机分布式资源)
//!
//! 移植自参考固件 MCA_F16V2_1_F48_BLE.ino 的 UDP 组播接收逻辑:
//! - `udp.begin(localPort)` — 绑定本地端口 (默认 5003)
//! - `udp.beginMulticast(groupIP, groupPort)` — 加入组播组
//! - `receiveMultiCastData()` — 每秒解析包, 仅接受来自 `SWITCH_IP` 的源地址
//! - 收到的数据 (≤32B) 填入 `recvBuffer[]`, 由 Modbus FC=04 读取 0x0090-0x0100
//!
//! 本实现使用 `std::net::UdpSocket` (ESP-IDF LwIP 提供的 POSIX 套接字).
//! 组播组加入通过 `IP_ADD_MEMBERSHIP` setsockopt 完成.
//!
//! 配置源: 保持寄存器 (HOLD_CFG_BASE 偏移)
//! - 2190: MULTICAST_IP1_2 (组播 IP octet1<<8 | octet2)
//! - 2191: MULTICAST_IP3_4 (组播 IP octet3<<8 | octet4)
//! - 2192: MULTICAST_PORT (组播端口, 默认 5003)
//! - 2193: SWITCH_IP1_2 (源 IP 过滤 octet1<<8 | octet2)
//! - 2194: SWITCH_IP3_4 (源 IP 过滤 octet3<<8 | octet4)
//!
//! 可靠性设计:
//! - 单独线程, 启动失败仅记日志, 不阻断主流程
//! - 网络未就绪时退避重试 (5s 间隔)
//! - 配置变更需重启生效 (对齐参考固件: `udp.begin` 只在启动时调用一次)

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use crate::bus::config_state::config_read_with;
use crate::config::regs;
use crate::error::AppResult;
use crate::health::{self, TaskHb};
use crate::sync::Spin;

/// 组播接收任务心跳 (阈值 30s, 每秒一次轮询, 余量充足)
static TASK_HB: TaskHb = TaskHb::new_with_stall("udp-mcast", 30);

/// 接收缓冲区 (对齐参考固件 recvBuffer[32])
/// 存放最近一次收到的组播数据, 通过 Modbus FC=04 0x0090 段读取.
static RECV_BUF: LazyLock<Spin<[u8; regs::MULTICAST_BUF_SIZE]>> =
    LazyLock::new(|| Spin::new([0u8; regs::MULTICAST_BUF_SIZE]));

/// 最近一次接收的字节数 (0 = 尚未收到)
static RECV_LEN: AtomicU16 = AtomicU16::new(0);

/// 默认组播地址 (参考固件未配置时的回退值: 239.0.0.1)
const DEFAULT_MCAST_ADDR: [u8; 4] = [239, 0, 0, 1];
/// 默认组播端口 (参考固件 localPort = 5003)
const DEFAULT_MCAST_PORT: u16 = 5003;
/// 接收缓冲区读超时 (让线程能周期性喂狗 + 检查配置变更)
const RECV_TIMEOUT_MS: u64 = 1000;
/// 配置缺失/网络未就绪时的退避
const BACKOFF_MS: u64 = 5000;

/// 从 CONFIG RCU 读取组播配置 (端口 + 组地址 + 源 IP 过滤)
///
/// LOOP11: 零拷贝, 闭包内构造 MulticastConfig 返回 (避免 udp-mcast 6KB 栈上 clone ~2.5KB).
fn read_multicast_config() -> MulticastConfig {
    config_read_with(|cs| {
        // 2190-2194 是 HOLD_CFG_BASE (0x0880) 偏移的子寄存器, 通过 SystemConfig::read_reg 读取
        let mcast_ip12 = cs.cfg.read_reg(regs::INREG_MULTICAST_IP1_2).unwrap_or(0);
        let mcast_ip34 = cs.cfg.read_reg(regs::INREG_MULTICAST_IP3_4).unwrap_or(0);
        let mcast_port = cs.cfg.read_reg(regs::INREG_MULTICAST_PORT).unwrap_or(0);
        let switch_ip12 = cs.cfg.read_reg(regs::INREG_SWITCH_IP1_2).unwrap_or(0);
        let switch_ip34 = cs.cfg.read_reg(regs::INREG_SWITCH_IP3_4).unwrap_or(0);

        let group = if mcast_ip12 == 0 && mcast_ip34 == 0 {
            DEFAULT_MCAST_ADDR
        } else {
            [
                (mcast_ip12 >> 8) as u8,
                (mcast_ip12 & 0xFF) as u8,
                (mcast_ip34 >> 8) as u8,
                (mcast_ip34 & 0xFF) as u8,
            ]
        };
        let port = if mcast_port == 0 {
            DEFAULT_MCAST_PORT
        } else {
            mcast_port
        };
        let switch_ip = if switch_ip12 == 0 && switch_ip34 == 0 {
            None // 不过滤源 IP
        } else {
            Some([
                (switch_ip12 >> 8) as u8,
                (switch_ip12 & 0xFF) as u8,
                (switch_ip34 >> 8) as u8,
                (switch_ip34 & 0xFF) as u8,
            ])
        };
        MulticastConfig {
            group,
            port,
            switch_ip,
        }
    }).unwrap_or_else(MulticastConfig::default)
}

struct MulticastConfig {
    group: [u8; 4],
    port: u16,
    /// 源 IP 过滤 (None = 接受任意源, 对齐参考固件 SWITCH_IP=0 时不接收)
    switch_ip: Option<[u8; 4]>,
}

impl MulticastConfig {
    fn default() -> Self {
        Self {
            group: DEFAULT_MCAST_ADDR,
            port: DEFAULT_MCAST_PORT,
            switch_ip: None,
        }
    }
}

/// 启动 UDP 组播接收任务 (后台线程, 失败不阻断主流程)
pub fn start() -> AppResult<()> {
    health::register(&TASK_HB);
    std::thread::Builder::new()
        .name("udp-mcast".into())
        .stack_size(6 * 1024)
        .spawn(recv_loop)
        .map_err(|e| crate::error::AppError::Sys(format!("spawn udp-mcast: {e}")))?;
    log::info!("[udp-mcast] receiver thread spawned");
    Ok(())
}

/// 接收主循环
fn recv_loop() {
    // LOOP9: 订阅硬件 WDT, 否则 feed_wdt() 高频报 "task not found" 刷屏
    health::subscribe_wdt();
    loop {
        TASK_HB.tick();
        health::feed_wdt();

        let cfg = read_multicast_config();
        match bind_and_recv(&cfg) {
            Ok(()) => {
                // 正常情况下 bind_and_recv 内部循环直到 socket 失效
                log::warn!("[udp-mcast] recv loop exited, reconnecting in {}ms", BACKOFF_MS);
            }
            Err(e) => {
                log::warn!("[udp-mcast] setup failed: {}, retry in {}ms", e, BACKOFF_MS);
            }
        }
        std::thread::sleep(Duration::from_millis(BACKOFF_MS));
    }
}

/// 绑定组播 socket 并持续接收
fn bind_and_recv(cfg: &MulticastConfig) -> Result<(), String> {
    // 1. 绑定本地端口 (INADDR_ANY)
    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), cfg.port);
    let sock = UdpSocket::bind(bind_addr).map_err(|e| format!("bind {bind_addr}: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(RECV_TIMEOUT_MS)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    sock.set_broadcast(true)
        .map_err(|e| format!("set_broadcast: {e}"))?;

    // 2. 加入组播组 (IP_ADD_MEMBERSHIP)
    // ESP-IDF LwIP setsockopt level=IPPROTO_IP, optname=IP_ADD_MEMBERSHIP
    // imr_interface=INADDR_ANY, imr_address=组播组地址
    join_multicast_group(&sock, cfg.group)
        .map_err(|e| format!("join multicast {:?}: {e}", cfg.group))?;

    log::info!(
        "[udp-mcast] listening on port {}, group {}.{}.{}.{}",
        cfg.port, cfg.group[0], cfg.group[1], cfg.group[2], cfg.group[3]
    );
    if let Some(sip) = cfg.switch_ip {
        log::info!(
            "[udp-mcast] source IP filter: {}.{}.{}.{}",
            sip[0], sip[1], sip[2], sip[3]
        );
    } else {
        log::info!("[udp-mcast] source IP filter: DISABLED (accept all)");
    }

    let mut buf = [0u8; regs::MULTICAST_BUF_SIZE];
    loop {
        TASK_HB.tick();
        health::feed_wdt();

        match sock.recv_from(&mut buf) {
            Ok((len, src)) => {
                // 源 IP 过滤 (对齐参考固件: 仅接受来自 SWITCH_IP 的包)
                if let Some(sip) = cfg.switch_ip {
                    if !src_ip_matches(&src, sip) {
                        log::debug!(
                            "[udp-mcast] dropped packet from {} (filter mismatch)",
                            src.ip()
                        );
                        continue;
                    }
                }
                // 写入全局 RECV_BUF (供 Modbus FC=04 读取)
                let n = len.min(regs::MULTICAST_BUF_SIZE);
                {
                    let mut guard = RECV_BUF.lock();
                    guard[..n].copy_from_slice(&buf[..n]);
                    // 剩余字节清零 (对齐参考固件: recvBuffer 仅前 len 字节有效)
                    for b in &mut guard[n..] {
                        *b = 0;
                    }
                }
                RECV_LEN.store(n as u16, Ordering::Release);
                log::debug!(
                    "[udp-mcast] received {} bytes from {} (stored {})",
                    len, src.ip(), n
                );
            }
            Err(e) => {
                // 超时是正常的 (set_read_timeout), 仅在非超时时告警
                if e.kind() != std::io::ErrorKind::WouldBlock
                    && e.kind() != std::io::ErrorKind::TimedOut
                {
                    log::warn!("[udp-mcast] recv_from error: {} (kind={:?})", e, e.kind());
                    return Err(format!("recv_from: {e}"));
                }
            }
        }
    }
}

/// 检查源地址是否匹配过滤 IP
fn src_ip_matches(src: &SocketAddr, filter: [u8; 4]) -> bool {
    match src.ip() {
        IpAddr::V4(v4) => v4.octets() == filter,
        _ => false,
    }
}

/// 通过 setsockopt(IPPROTO_IP, IP_ADD_MEMBERSHIP) 加入 IPv4 组播组
///
/// LOOP9 字节序修复 (最终正确版): `in_addr.s_addr` 期望**网络字节序** (BE),
/// `group`/`if_ip` 已是主机序 octet 数组, 必须用 `from_be_bytes` 转 BE u32.
///
/// (前一个 LOOP9 错误版本用 `from_ne_bytes` — 在 ESP32-S3 LE 平台上等价
/// `from_le_bytes`, 产生 `0x010000EF` 而非 `0xEF000001`, 组播加入失败。)
///
/// 同时, LwIP 中 `imr_interface=INADDR_ANY` 可能因无默认 netif 而失败 (errno=125),
/// 因此使用 CONFIG RCU 中存储的实际设备 IP 作为接口地址。
fn join_multicast_group(sock: &UdpSocket, group: [u8; 4]) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;
    // ip_mreq { imr_multiaddr: in_addr, imr_interface: in_addr }
    // in_addr { s_addr: u32 (network byte order) }
    #[repr(C)]
    struct IpMreq {
        imr_multiaddr: u32,
        imr_interface: u32,
    }

    // LOOP11: 零拷贝读设备 IP (避免 udp-mcast 6KB 栈上 clone ~2.5KB)
    let if_ip = config_read_with(|cs| cs.cfg.ip)
        .unwrap_or([0, 0, 0, 0]);
    log::info!(
        "[udp-mcast] join group={}.{}.{}.{} iface={}.{}.{}.{}",
        group[0], group[1], group[2], group[3],
        if_ip[0], if_ip[1], if_ip[2], if_ip[3]
    );

    let mreq = IpMreq {
        // s_addr = network byte order: group/if_ip 是主机序 octet 数组, 用 from_be_bytes 转 BE
        imr_multiaddr: u32::from_be_bytes(group),
        imr_interface: u32::from_be_bytes(if_ip),
    };
    const IPPROTO_IP: i32 = 0;
    let ip_add_membership = esp_idf_sys::IP_ADD_MEMBERSHIP as i32;
    let fd = sock.as_raw_fd();
    let ret = unsafe {
        esp_idf_sys::lwip_setsockopt(
            fd,
            IPPROTO_IP,
            ip_add_membership,
            &mreq as *const _ as *mut _,
            std::mem::size_of::<IpMreq>() as u32,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// 公开 API (供 Modbus FC=04 读取)
// ----------------------------------------------------------------------------

/// 读取组播接收缓冲区指定偏移 (相对 0x0090 基址的 U16 索引)
///
/// 对齐参考固件: `status[i] = recvBuffer[reg + i - REG_STATU_SWITCH_START]`
/// recvBuffer 按 U8 存储, Modbus 按 U16 读取 → 每 U16 = 2 字节 (LE).
pub fn read_switch_status(word_idx: u16) -> Option<u16> {
    let idx = word_idx as usize;
    let total_words = (regs::MULTICAST_BUF_SIZE + 1) / 2;
    // LOOP9: 修复 && → || (AND 恒真 → 所有地址返回 Some; OR 正确拒绝越界)
    if idx >= total_words as usize || word_idx >= regs::INREG_SWITCH_STATUS_COUNT {
        return None;
    }
    let guard = RECV_BUF.lock();
    let b0 = guard.get(idx * 2).copied().unwrap_or(0);
    let b1 = guard.get(idx * 2 + 1).copied().unwrap_or(0);
    // 参考固件 recvBuffer 为 U8 数组, Modbus 读 U16 时直接取连续两字节
    // 字节序: 保持 LE (与 ESP32 内存序一致), 不做 BE 转换
    Some(u16::from_le_bytes([b0, b1]))
}

/// 最近一次接收的字节数 (供诊断)
pub fn recv_len() -> usize {
    RECV_LEN.load(Ordering::Acquire) as usize
}
