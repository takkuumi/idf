//! RS485 端口配置参数
//!
//! 由 `Rs485Config::from_rtu_master` / `from_rtu_slave` 从全局
//! `config::modbus` 构造，提供给 `Rs485Port::open` 使用。

use crate::config::modbus::{rtu_master, rtu_slave};

/// 单路 RS485 配置
#[derive(Clone, Copy, Debug)]
pub struct Rs485Config {
    /// UART 端口号: 0 = UART0, 1 = UART1
    pub uart_port: u8,
    /// TX 引脚
    pub tx_pin: u8,
    /// RX 引脚
    pub rx_pin: u8,
    /// DE/RE 共控引脚 (高=发送, 低=接收)
    pub de_pin: u8,
    /// 波特率
    pub baud: u32,
    /// 数据位 5/6/7/8
    pub data_bits: u8,
    /// 校验 'N'/'E'/'O'
    pub parity: char,
    /// 停止位 1/2
    pub stop_bits: u8,
    /// RTS 引脚 (用作硬件自动 RS485 DE 控制)，与 de_pin 二选一
    /// 若使用 UART 内置 RS485 模式，把 RTS 接到 DE 即可
    pub rts_pin: u8,
}

impl Default for Rs485Config {
    fn default() -> Self {
        Self {
            uart_port: 1,
            tx_pin: 4,
            rx_pin: 5,
            de_pin: 6,
            baud: 9600,
            data_bits: 8,
            parity: 'N',
            stop_bits: 1,
            rts_pin: 6,
        }
    }
}

impl Rs485Config {
    /// 从 `config::modbus::rtu_master` 构造 (RS485 #0 = UART1)
    pub fn from_rtu_master() -> Self {
        use crate::config::pins as p;
        Self {
            uart_port: rtu_master::UART_PORT,
            tx_pin: p::RS485_0_TX,
            rx_pin: p::RS485_0_RX,
            de_pin: p::RS485_0_DE,
            baud: rtu_master::BAUD,
            data_bits: rtu_master::DATA_BITS,
            parity: rtu_master::PARITY,
            stop_bits: rtu_master::STOP_BITS,
            rts_pin: p::RS485_0_DE,
        }
    }

    /// 从 `config::modbus::rtu_slave` 构造 (RS485 #1 = UART0)
    ///
    /// 注意: UART0 与下载串口复用，调试期间建议改用 UART1
    pub fn from_rtu_slave() -> Self {
        use crate::config::pins as p;
        Self {
            uart_port: rtu_slave::UART_PORT,
            tx_pin: p::RS485_1_TX,
            rx_pin: p::RS485_1_RX,
            de_pin: p::RS485_1_DE,
            baud: rtu_slave::BAUD,
            data_bits: rtu_slave::DATA_BITS,
            parity: rtu_slave::PARITY,
            stop_bits: rtu_slave::STOP_BITS,
            rts_pin: p::RS485_1_DE,
        }
    }
}
