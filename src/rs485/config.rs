//! RS485 端口配置参数
//!
//! 由 `Rs485Config::from_rtu_master` / `from_rtu_slave` 从全局
//! `config::modbus` 构造，提供给 `Rs485Port::open` 使用。

/// 单路 RS485 配置
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    fn apply_saved(mut self, saved: &crate::device::system_config::Rs485Config) -> Self {
        self.baud = match saved.baudrate {
            1200 | 2400 | 4800 | 9600 | 14400 | 19200 | 38400 | 57600 | 115200 | 128000
            | 153600 | 230400 | 256000 | 460800 | 921600 => saved.baudrate,
            _ => 9600,
        };
        self.data_bits = if saved.data_bits == 7 { 7 } else { 8 };
        self.parity = match saved.parity {
            1 => 'O',
            2 => 'E',
            _ => 'N',
        };
        self.stop_bits = if saved.stop_bits == 2 { 2 } else { 1 };
        self
    }

    fn physical_port(index: usize) -> Option<Self> {
        use crate::config::pins as p;
        match index {
            0 => Self {
                uart_port: p::RS485_0_UART,
                tx_pin: p::RS485_0_TX,
                rx_pin: p::RS485_0_RX,
                de_pin: p::RS485_0_DE,
                rts_pin: p::RS485_0_DE,
                ..Self::default()
            },
            1 => Self {
                uart_port: p::RS485_1_UART,
                tx_pin: p::RS485_1_TX,
                rx_pin: p::RS485_1_RX,
                de_pin: p::RS485_1_DE,
                rts_pin: p::RS485_1_DE,
                ..Self::default()
            },
            2 => Self {
                uart_port: p::RS485_2_UART,
                tx_pin: p::RS485_2_TX,
                rx_pin: p::RS485_2_RX,
                de_pin: p::RS485_2_DE,
                rts_pin: p::RS485_2_DE,
                ..Self::default()
            },
            _ => return None,
        }
        .into()
    }

    /// 根据物理端口和同一份不可变配置快照构造串口参数。
    pub(crate) fn from_port_with_saved(
        index: usize,
        saved: &crate::device::system_config::Rs485Config,
    ) -> Option<Self> {
        Some(Self::physical_port(index)?.apply_saved(saved))
    }

    /// 根据物理端口构造，并覆盖当前 SystemConfig 中的串口参数。
    pub fn from_port(index: usize) -> Option<Self> {
        crate::bus::config_state::config_read_with(|state| {
            Self::from_port_with_saved(index, state.cfg.rs485.get(index)?)
        })
        .flatten()
    }

    /// 从 RS485 #0 (UART1) 构造。
    pub fn from_rtu_master() -> Self {
        Self::from_port(0).expect("RS485-1 exists")
    }

    /// 从 `config::modbus::rtu_slave` 构造 (RS485 #1 = UART2)
    ///
    /// 注意: UART0 与下载串口复用，调试期间建议改用 UART1
    pub fn from_rtu_slave() -> Self {
        Self::from_port(1).expect("RS485-2 exists")
    }

    /// 构造 RS485 #2 (第 3 端口 = UART0, 对齐参考固件 RS485-3)
    ///
    /// 默认 9600 8N1, 仅从站监听模式. UART0 与 USB CDC/JTAG 复用,
    /// 启用后将失去调试串口; 由 `config::modbus::rtu_port2::ENABLED` 控制是否启动.
    pub fn from_rtu_port2() -> Self {
        Self::from_port(2).expect("RS485-3 exists")
    }
}
