//! 以太网模块 (W5500 over SPI)
//!
//! 通过 ESP-IDF 内置的 SPI Ethernet 驱动接入 WIZnet W5500，
//! 由 LwIP 提供 TCP/IP 协议栈，对外暴露标准 lwip netif。
//!
//! W5500 特性:
//!   - 硬wired TCP/IP 协议栈 (本系统不使用, 由 LwIP 提供软件协议栈)
//!   - 内置 10/100 MAC + PHY, 32KB 缓冲, 8 个硬件 socket
//!   - SPI 接口, 最高 80MHz (本系统配置 40MHz)
//!
//! 链路监控范围：
//! - 每 5 秒检查本地 W5500 PHY 链路和 netif 的非零 IP，不发送对端 ping。
//! - 此模块只建立一个 W5500 网络接口，不实现双光口交换或环网冗余协议。
//! - GPIO39/40 的光纤检测与状态灯由 HAL 管理，不代表业务转发已验证。
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
