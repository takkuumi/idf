//! 全局错误类型
//!
//! 统一所有模块的错误返回，便于 `?` 传播。
//!
//! 子模块:
//! - [`recovery`]: 分级故障恢复 (替代直接 esp_restart)

pub mod recovery;
pub mod ringlog;

pub use ringlog::{log_warn, module_id};

use core::fmt;
use std::io;

/// 应用层错误
#[derive(Debug)]
pub enum AppError {
    /// 硬件初始化失败
    Hal(String),
    /// 以太网相关
    Ethernet(String),
    /// BLE Mesh 相关
    BleMesh(String),
    /// Modbus 协议
    Modbus(String),
    /// RS485 收发
    Rs485(String),
    /// IO 点
    Io(String),
    /// AI/AO 通道
    Channel(String),
    /// 配置错误
    Config(String),
    /// ESP-IDF 系统错误
    Sys(String),
    /// OTA 升级
    Ota(String),
    /// IO 错误
    IoErr(io::Error),
    /// 其它
    Other(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Hal(s) => write!(f, "hal: {s}"),
            AppError::Ethernet(s) => write!(f, "ethernet: {s}"),
            AppError::BleMesh(s) => write!(f, "ble-mesh: {s}"),
            AppError::Modbus(s) => write!(f, "modbus: {s}"),
            AppError::Rs485(s) => write!(f, "rs485: {s}"),
            AppError::Io(s) => write!(f, "io: {s}"),
            AppError::Channel(s) => write!(f, "channel: {s}"),
            AppError::Config(s) => write!(f, "config: {s}"),
            AppError::Sys(s) => write!(f, "sys: {s}"),
            AppError::Ota(s) => write!(f, "ota: {s}"),
            AppError::IoErr(e) => write!(f, "io-err: {e}"),
            AppError::Other(s) => write!(f, "other: {s}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<io::Error> for AppError {
    fn from(e: io::Error) -> Self {
        AppError::IoErr(e)
    }
}

impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError::Other(s.to_string())
    }
}

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Other(s)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Other(e.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[macro_export]
macro_rules! bail_err {
    ($variant:ident, $msg:expr) => {
        return Err($crate::error::AppError::$variant($msg.to_string()))
    };
    ($variant:ident, $fmt:expr, $($arg:tt)*) => {
        return Err($crate::error::AppError::$variant(format!($fmt, $($arg)*)))
    };
}
