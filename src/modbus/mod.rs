//! Modbus 协议模块
//!
//! 同时支持：
//! - RTU Master (RS485 #1) - 主站轮询从站设备
//! - RTU Slave  (RS485 #2) - 本机作为从站响应外部主站
//! - TCP Server (以太网)   - 监听 502 端口，多连接
//!
//! 数据通过 `bus::backends` 无锁全局函数访问，Modbus 寄存器映射见 `config::regs`。

use std::sync::Arc;

use crate::error::AppResult;
use crate::hal::Hal;

#[cfg(feature = "modbus-rtu")]
pub mod rtu_master;
#[cfg(feature = "modbus-rtu")]
pub mod rtu_port2;
#[cfg(feature = "modbus-rtu")]
pub mod rtu_slave;
pub mod shared;
#[cfg(feature = "modbus-tcp")]
pub mod tcp_server;

/// 启动 RTU Master + Slave 任务
#[cfg(feature = "modbus-rtu")]
pub fn start_rtu(_hal: Arc<Hal>) -> AppResult<()> {
    rtu_master::start(_hal.clone())?;
    rtu_slave::start(_hal.clone())?;

    // 可选启动 RS485 第 3 端口 (UART0, 对齐参考固件 RS485-3)
    // 注: UART0 与 USB CDC/JTAG 复用, 默认禁用以保留调试串口
    if crate::config::modbus::rtu_port2::ENABLED {
        rtu_port2::start(_hal.clone())?;
    }
    Ok(())
}

/// 初始化 TCP Server 主循环状态机
#[cfg(feature = "modbus-tcp")]
pub fn start_tcp() -> AppResult<()> {
    tcp_server::start()
}
