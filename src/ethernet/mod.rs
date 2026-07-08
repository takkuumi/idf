//! 以太网模块 (W5500 over SPI)
//!
//! 通过 ESP-IDF 内置的 SPI Ethernet 驱动接入 WIZnet W5500，
//! 由 LwIP 提供 TCP/IP 协议栈，对外暴露标准 lwip netif。
//!
//! W5500 特性:
//!   - 硬wired TCP/IP 协议栈 (本系统不使用, 由 LwIP 提供软件协议栈)
//!   - 内置 10/100 MAC + PHY, 32KB 缓冲, 8 个硬件 socket
//!   - SPI 接口, 最高 80MHz (本系统用 20MHz 保证稳定性)
//!
//! 简单冗余策略：
//! - 心跳监控：周期性 ping 对端，超时后触发复位
//! - 应用层重发：对关键命令在应用层做幂等 + 序号
//!
//! 启动入口：[`start`]

use std::sync::Arc;

use esp_idf_svc::eventloop::EspSystemEventLoop;

use crate::error::AppResult;
use crate::hal::Hal;

/// 启动以太网任务。返回后 W5500 已绑定到 LwIP netif，
/// 系统的 std::net / esp_idf_svc::netif 都可用。
pub fn start(_hal: Arc<Hal>, _sys_loop: EspSystemEventLoop) -> AppResult<()> {
    crate::ethernet::w5500::start(_hal, _sys_loop)
}

pub mod w5500;
