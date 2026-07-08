//! UART 参数容器
//!
//! 本模块**不实际打开 UART**，仅存储 uart0/uart1/uart2 的引脚号和默认配置，
//! 供 RS485 模块在自身初始化时读取。RS485 模块负责通过 ESP-IDF C API
//! 创建 UART 驱动并配置 DE/RE 引脚 (UART_MODE_RS485_HALF_DUPLEX)。
//!
//! # 设计原因
//!
//! ESP32-S3 有 3 个 UART (UART0/1/2)：
//!   - UART0: 下载/日志 (TX=43, RX=44)
//!   - UART1: RS485 #0 (Modbus 主站)
//!   - UART2: RS485 #1 (Modbus 从站)
//! 把 UART 实例化推迟到 RS485 模块可避免与 `EspSerial` 等高层封装冲突。

use crate::error::AppResult;

/// UART 配置参数
///
/// 字段与 `config::modbus::{rtu_master, rtu_slave}` 对齐。
#[derive(Clone, Copy, Debug)]
pub struct UartConfig {
    /// 波特率
    pub baud: u32,
    /// 数据位 (5/6/7/8)
    pub data_bits: u8,
    /// 校验位 ('N' / 'E' / 'O')
    pub parity: char,
    /// 停止位 (1/2)
    pub stop_bits: u8,
}

/// UART 端口容器
///
/// 仅持有 uart0/uart1/uart2 的引脚号和默认配置参数，不持有任何驱动句柄。
/// RS485 模块在 init 时通过 `hal.uart.uart1_tx` 等字段读取所需信息。
pub struct UartPort {
    /// UART0 TX 引脚 (下载/日志, 通常 GPIO43)
    pub uart0_tx: u8,
    /// UART0 RX 引脚 (下载/日志, 通常 GPIO44)
    pub uart0_rx: u8,
    /// RS485 #0 (UART1) TX 引脚
    pub uart1_tx: u8,
    /// RS485 #0 (UART1) RX 引脚
    pub uart1_rx: u8,
    /// RS485 #1 (UART2) TX 引脚
    pub uart2_tx: u8,
    /// RS485 #1 (UART2) RX 引脚
    pub uart2_rx: u8,
    /// UART0 默认配置 (下载/日志, 115200-N-8-1)
    pub uart0_cfg: UartConfig,
    /// UART1 (RS485 #0, Modbus 主站) 默认配置
    pub uart1_cfg: UartConfig,
    /// UART2 (RS485 #1, Modbus 从站) 默认配置
    pub uart2_cfg: UartConfig,
}

impl UartPort {
    /// 初始化 UART 参数容器。
    ///
    /// 参数为 uart0/uart1/uart2 的 TX/RX 引脚号。
    /// 默认串口配置取自 `config::modbus::{rtu_slave, rtu_master}`。
    pub fn init(uart0_tx: u8, uart0_rx: u8,
                uart1_tx: u8, uart1_rx: u8,
                uart2_tx: u8, uart2_rx: u8) -> AppResult<Self> {
        use crate::config::modbus::{rtu_master, rtu_slave};

        Ok(Self {
            uart0_tx,
            uart0_rx,
            uart1_tx,
            uart1_rx,
            uart2_tx,
            uart2_rx,
            uart0_cfg: UartConfig {
                baud: 115200,
                data_bits: 8,
                parity: 'N',
                stop_bits: 1,
            },
            uart1_cfg: UartConfig {
                baud: rtu_master::BAUD,
                data_bits: rtu_master::DATA_BITS,
                parity: rtu_master::PARITY,
                stop_bits: rtu_master::STOP_BITS,
            },
            uart2_cfg: UartConfig {
                baud: rtu_slave::BAUD,
                data_bits: rtu_slave::DATA_BITS,
                parity: rtu_slave::PARITY,
                stop_bits: rtu_slave::STOP_BITS,
            },
        })
    }
}
