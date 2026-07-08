//! RS485 收发方向控制
//!
//! ESP32-S3 UART 自带 RS485 模式（UART_MODE_RS485_HALF_DUPLEX），
//! 自动管理 DE/RE 引脚（硬件连线把 DE+RE 接到同一个 RTS 输出即可）。
//!
//! 本模块提供高层封装：
//! - [`Rs485Port`]：单路 RS485 端口（含 UART 配置 + DE 引脚配置）
//! - [`Rs485Port::send_recv`]：发送并等待应答（带超时）

use crate::error::AppResult;

pub use port::Rs485Port;
pub use config::Rs485Config;

pub mod port;
pub mod config;

/// 创建一路 RS485 端口
pub fn open(_cfg: &Rs485Config) -> AppResult<Rs485Port> {
    Rs485Port::open(_cfg)
}
