//! IO 扩展聚合器 (I2C 总线 + 多片 MCP23017)
//!
//! 仅 F3/F4 版本启用, 提供 DI/DO 的统一访问接口。
//!
//! # 通道分配
//!
//! DI:
//! - F3 (16 DI): 1 片 MCP23017 (地址 0x20), bit0-15
//! - F4 (48 DI): 3 片 MCP23017 (地址 0x20/0x21/0x22), bit0-15 / bit16-31 / bit32-47
//!
//! DO:
//! - F3 (16 DO): 1 片 MCP23017 (地址 0x21), bit0-15
//! - F4 (16 DO): 1 片 MCP23017 (地址 0x23), bit0-15
//!
//! # 位序约定
//!
//! `read_di()` 返回 `u64`, bit i 对应 DI i:
//! - DI 0-15: 第一片 MCP23017 (地址 0x20)
//! - DI 16-31: 第二片 (地址 0x21, 仅 F4)
//! - DI 32-47: 第三片 (地址 0x22, 仅 F4)
//!
//! MCP23017 内部: bit0-7 = PORTA, bit8-15 = PORTB

use parking_lot::Mutex;

use crate::config::hw_version;
use crate::config::io_ext as cfg;
use crate::error::{AppError, AppResult};
use crate::hal::i2c_bus::I2cBus;
use crate::hal::mcp23017::Mcp23017;

/// 最大 DI 扩展芯片数 (F4 = 3 片)
const MAX_DI_CHIPS: usize = 3;

/// IO 扩展聚合器
///
/// 持有 I2C 总线 + 所有 MCP23017 芯片句柄。
/// 用 Mutex 保护, 多任务访问安全 (DI 扫描 / DO 输出 / Modbus 读状态)。
pub struct IoExtender {
    bus: Mutex<I2cBus>,
    di_chips: [Mcp23017; MAX_DI_CHIPS],
    di_chip_count: usize,
    do_chip: Mcp23017,
    /// DO 输出缓存 (避免每次读取 MCP23017)
    do_cache: Mutex<u64>,
}

impl IoExtender {
    /// 初始化 IO 扩展
    ///
    /// 流程:
    /// 1. 探测所有 MCP23017 (地址响应)
    /// 2. DI 芯片配置为输入 + 上拉
    /// 3. DO 芯片配置为输出 (初始 0)
    pub fn init(bus: I2cBus) -> AppResult<Self> {
        let mut bus_guard = bus;
        let di_addrs = cfg::DI_ADDRS;
        let do_addr = cfg::DO_ADDR;

        // 探测 DI 芯片
        let mut di_chips: [Mcp23017; MAX_DI_CHIPS] = [
            Mcp23017::new(0),
            Mcp23017::new(0),
            Mcp23017::new(0),
        ];
        for (i, &addr) in di_addrs.iter().enumerate() {
            let chip = Mcp23017::new(addr);
            if !chip.probe(&mut bus_guard) {
                return Err(AppError::Hal(format!(
                    "MCP23017 DI chip {} at 0x{:02X} not found",
                    i, addr
                )));
            }
            chip.init_as_input(&mut bus_guard)?;
            di_chips[i] = chip;
            log::info!("[io_ext] DI chip {} at 0x{:02X} initialized", i, addr);
        }

        // 探测 + 初始化 DO 芯片
        let do_chip = Mcp23017::new(do_addr);
        if !do_chip.probe(&mut bus_guard) {
            return Err(AppError::Hal(format!(
                "MCP23017 DO chip at 0x{:02X} not found",
                do_addr
            )));
        }
        do_chip.init_as_output(&mut bus_guard)?;
        log::info!("[io_ext] DO chip at 0x{:02X} initialized", do_addr);

        Ok(Self {
            bus: Mutex::new(bus_guard),
            di_chips,
            di_chip_count: di_addrs.len(),
            do_chip,
            do_cache: Mutex::new(0),
        })
    }

    /// 读取所有 DI 通道, 返回 bit i = DI i 状态
    ///
    /// F3: 16 bit, F4: 48 bit
    #[inline]
    pub fn read_di(&self) -> AppResult<u64> {
        let mut bus = self.bus.lock();
        let mut result: u64 = 0;
        for i in 0..self.di_chip_count {
            let bits = self.di_chips[i].read_inputs(&mut *bus)? as u64;
            result |= bits << (i * 16);
        }
        Ok(result)
    }

    /// 读取单个 DI 通道
    #[inline]
    pub fn read_di_channel(&self, idx: usize) -> AppResult<bool> {
        if idx >= hw_version::DI_COUNT {
            return Err(AppError::Io(format!("DI idx {} out of range", idx)));
        }
        let bits = self.read_di()?;
        Ok(bits & (1 << idx) != 0)
    }

    /// 写入所有 DO 通道, bit i = DO i 目标状态
    ///
    /// F3/F4: 16 bit (低 16 位有效)
    #[inline]
    pub fn write_do(&self, value: u64) -> AppResult<()> {
        // 缓存更新
        *self.do_cache.lock() = value;
        // 只写入低 16 位 (单芯片 16 DO)
        let mask = (1u64 << hw_version::DO_COUNT) - 1;
        let _do_bits = (value & mask) as u16;
        let mut bus = self.bus.lock();
        self.do_chip.write_outputs(&mut *bus, _do_bits)?;
        Ok(())
    }

    /// 写入单个 DO 通道
    #[inline]
    pub fn write_do_channel(&self, idx: usize, on: bool) -> AppResult<()> {
        if idx >= hw_version::DO_COUNT {
            return Err(AppError::Io(format!("DO idx {} out of range", idx)));
        }
        let mut current = *self.do_cache.lock();
        if on {
            current |= 1u64 << idx;
        } else {
            current &= !(1u64 << idx);
        }
        self.write_do(current)
    }

    /// 读取当前 DO 输出状态 (从缓存, 不读 I2C)
    #[inline]
    pub fn read_do_cached(&self) -> u64 {
        *self.do_cache.lock()
    }

    /// 读取当前 DO 输出状态 (从 MCP23017 读取, 慢)
    pub fn read_do_actual(&self) -> AppResult<u64> {
        let mut bus = self.bus.lock();
        let bits = self.do_chip.read_outputs(&mut *bus)? as u64;
        Ok(bits)
    }

    /// 获取 I2C 总线互斥锁 (供高级用例, 如扫描其它 I2C 设备)
    pub fn bus(&self) -> &Mutex<I2cBus> {
        &self.bus
    }
}

// ============================================================================
// DigitalIo trait 实现 (F3/F4 版本, I2C MCP23017 扩展)
// ============================================================================
// IoExtender 已有的方法签名与 DigitalIo trait 高度一致,
// 这里直接委托, 无额外逻辑。
impl crate::hal::digital_io::DigitalIo for IoExtender {
    fn di_count(&self) -> usize {
        hw_version::DI_COUNT
    }

    fn do_count(&self) -> usize {
        hw_version::DO_COUNT
    }

    /// 读取所有 DI 通道 (F3: 16 bit / F4: 48 bit)
    ///
    /// 内部逐片读取 MCP23017, F4 需 3 次 I2C 读 (约 600μs @ 400kHz)
    fn read_di_all(&self) -> AppResult<u64> {
        self.read_di()
    }

    /// 读取单个 DI 通道
    fn read_di(&self, idx: usize) -> AppResult<bool> {
        self.read_di_channel(idx)
    }

    /// 写入所有 DO 通道 (单次 I2C 写入, 约 200μs)
    fn write_do_all(&self, value: u64) -> AppResult<()> {
        self.write_do(value)
    }

    /// 写入单个 DO 通道 (读改写, 含一次 I2C 读 + 一次 I2C 写)
    fn write_do(&self, idx: usize, on: bool) -> AppResult<()> {
        self.write_do_channel(idx, on)
    }

    /// 读取 DO 缓存 (不读 I2C, <1μs)
    fn read_do_cached(&self) -> u64 {
        self.read_do_cached()
    }

    /// 读取 DO 实际硬件状态 (读 MCP23017 OLAT, 约 200μs)
    fn read_do_actual(&self) -> AppResult<u64> {
        self.read_do_actual()
    }
}
