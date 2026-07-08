//! MCP23017 16 通道 I2C IO 扩展芯片驱动
//!
//! 每片提供 16 个 GPIO (PORTA 8 + PORTB 8)。
//!
//! 用法:
//! - DI 模式: `init_as_input()` + `read_inputs()` → u16
//! - DO 模式: `init_as_output()` + `write_outputs(u16)`
//!
//! 寄存器 (BANK=0, 默认 IOCON.BANK=0):
//! - 0x00 IODIRA, 0x01 IODIRB: 方向 (1=输入, 0=输出)
//! - 0x0C GPPUA, 0x0D GPPUB: 上拉 (1=使能)
//! - 0x12 GPIOA, 0x13 GPIOB: 端口数据
//! - 0x14 OLATA, 0x15 OLATB: 输出锁存

use crate::config::io_ext as cfg;
use crate::error::AppResult;
use crate::hal::i2c_bus::I2cBus;

/// MCP23017 句柄 (不持有 bus, 每次操作传入)
///
/// 多片共享一条 I2C 总线, 通过 I2C 地址区分。
#[derive(Clone, Copy)]
pub struct Mcp23017 {
    addr: u8,
}

impl Mcp23017 {
    /// 创建句柄 (不立即通信, 仅记录地址)
    pub const fn new(addr: u8) -> Self {
        Self { addr }
    }

    /// 初始化为输入模式 (DI 用)
    ///
    /// - PORTA + PORTB 全部配置为输入
    /// - 内部上拉使能 (光耦/按钮无外部上拉时必需)
    pub fn init_as_input(&self, bus: &mut I2cBus) -> AppResult<()> {
        // IODIRA = 0xFF, IODIRB = 0xFF (全部输入)
        bus.write_reg_byte(self.addr, cfg::REG_IODIRA, 0xFF)?;
        bus.write_reg_byte(self.addr, cfg::REG_IODIRB, 0xFF)?;
        // GPPUA = 0xFF, GPPUB = 0xFF (内部上拉 100kΩ)
        bus.write_reg_byte(self.addr, cfg::REG_GPPUA, 0xFF)?;
        bus.write_reg_byte(self.addr, cfg::REG_GPPUB, 0xFF)?;
        log::debug!("[mcp23017] 0x{:02X} init as input (16-ch DI)", self.addr);
        Ok(())
    }

    /// 初始化为输出模式 (DO 用)
    ///
    /// - PORTA + PORTB 全部配置为输出
    /// - 初始输出 0 (低电平)
    pub fn init_as_output(&self, bus: &mut I2cBus) -> AppResult<()> {
        // IODIRA = 0, IODIRB = 0 (全部输出)
        bus.write_reg_byte(self.addr, cfg::REG_IODIRA, 0x00)?;
        bus.write_reg_byte(self.addr, cfg::REG_IODIRB, 0x00)?;
        // OLATA = 0, OLATB = 0 (初始低电平)
        bus.write_reg_byte(self.addr, cfg::REG_OLATA, 0x00)?;
        bus.write_reg_byte(self.addr, cfg::REG_OLATB, 0x00)?;
        log::debug!("[mcp23017] 0x{:02X} init as output (16-ch DO)", self.addr);
        Ok(())
    }

    /// 读取 16 路输入 (PORTA = bit0-7, PORTB = bit8-15)
    ///
    /// 通过 GPIOA 寄存器顺序读 2 字节 (BANK=0 模式下地址自动 +1)
    pub fn read_inputs(&self, bus: &mut I2cBus) -> AppResult<u16> {
        let mut buf = [0u8; 2];
        bus.read_reg(self.addr, cfg::REG_GPIOA, &mut buf)?;
        // DI 编号: bit0-7 = PORTA, bit8-15 = PORTB
        Ok(buf[0] as u16 | ((buf[1] as u16) << 8))
    }

    /// 写入 16 路输出 (PORTA = bit0-7, PORTB = bit8-15)
    ///
    /// 通过 OLATA 寄存器顺序写 2 字节
    pub fn write_outputs(&self, bus: &mut I2cBus, value: u16) -> AppResult<()> {
        let port_a = (value & 0xFF) as u8;
        let port_b = (value >> 8) as u8;
        bus.write_reg(self.addr, cfg::REG_OLATA, &[port_a, port_b])?;
        Ok(())
    }

    /// 读取当前输出锁存值 (OLATA + OLATB)
    pub fn read_outputs(&self, bus: &mut I2cBus) -> AppResult<u16> {
        let mut buf = [0u8; 2];
        bus.read_reg(self.addr, cfg::REG_OLATA, &mut buf)?;
        Ok(buf[0] as u16 | ((buf[1] as u16) << 8))
    }

    /// 探测设备 (读 IODIRA 寄存器, 不报错即存在)
    pub fn probe(&self, bus: &mut I2cBus) -> bool {
        bus.read_reg_byte(self.addr, cfg::REG_IODIRA).is_ok()
    }

    /// I2C 地址
    #[inline]
    pub fn addr(&self) -> u8 {
        self.addr
    }
}
