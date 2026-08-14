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

use crate::sync::{AtomicBits64, Spin};

use crate::config::hw_version;
use crate::config::io_ext as cfg;
use crate::error::{AppError, AppResult};
use crate::hal::i2c_bus::I2cBus;
use crate::hal::mcp23017::Mcp23017;

/// 最大 DI 扩展芯片数 (F4 = 3 片)
const MAX_DI_CHIPS: usize = 3;
/// 最大 DO 扩展芯片数 (F4 = 3 片, 每片 16 DO → 48 DO)
const MAX_DO_CHIPS: usize = 3;

/// IO 扩展聚合器
///
/// 持有 I2C 总线 + 所有 MCP23017 芯片句柄。
/// 用 Mutex 保护, 多任务访问安全 (DI 扫描 / DO 输出 / Modbus 读状态)。
pub struct IoExtender {
    bus: Spin<I2cBus>,
    di_chips: [Mcp23017; MAX_DI_CHIPS],
    di_chip_count: usize,
    /// DO 扩展芯片数组 (F3: 1 片; F4: 3 片)
    do_chips: [Mcp23017; MAX_DO_CHIPS],
    do_chip_count: usize,
    /// DO 输出缓存 (避免每次读取 MCP23017); 真无锁 (AtomicBits64)
    do_cache: AtomicBits64,
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
        let do_addrs = cfg::DO_ADDRS;

        // 探测 DI 芯片
        let mut di_chips: [Mcp23017; MAX_DI_CHIPS] =
            [Mcp23017::new(0), Mcp23017::new(0), Mcp23017::new(0)];
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

        // 探测 + 初始化 DO 芯片 (F3: 1 片; F4: 3 片)
        let mut do_chips: [Mcp23017; MAX_DO_CHIPS] =
            [Mcp23017::new(0), Mcp23017::new(0), Mcp23017::new(0)];
        for (i, &addr) in do_addrs.iter().enumerate() {
            let chip = Mcp23017::new(addr);
            if !chip.probe(&mut bus_guard) {
                return Err(AppError::Hal(format!(
                    "MCP23017 DO chip {} at 0x{:02X} not found",
                    i, addr
                )));
            }
            chip.init_as_output(&mut bus_guard)?;
            do_chips[i] = chip;
            log::info!("[io_ext] DO chip {} at 0x{:02X} initialized", i, addr);
        }

        Ok(Self {
            bus: Spin::new(bus_guard),
            di_chips,
            di_chip_count: di_addrs.len(),
            do_chips,
            do_chip_count: do_addrs.len(),
            do_cache: AtomicBits64::new(0),
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
    /// F3: 16 bit (单芯片 16 DO)
    /// F4: 48 bit (3 片 MCP23017, 每片写 16 bit)
    #[inline]
    pub fn write_do(&self, value: u64) -> AppResult<()> {
        // 限制为 DO_COUNT 位 (超出 bit 忽略)
        let mask = if hw_version::DO_COUNT >= 64 {
            u64::MAX
        } else {
            (1u64 << hw_version::DO_COUNT) - 1
        };
        let value = value & mask;
        // 逐片写入 MCP23017, 每片 16 bit
        let mut bus = self.bus.lock();
        for i in 0..self.do_chip_count {
            let shift = i * 16;
            let do_bits = ((value >> shift) & 0xFFFF) as u16;
            self.do_chips[i].write_outputs(&mut *bus, do_bits)?;
        }
        // 只有全部芯片写成功后，缓存才代表已确认的硬件状态。
        self.do_cache.store_bits(value);
        Ok(())
    }

    /// 写入单个 DO 通道
    /// F4 (DO_COUNT=0): 总是返回 Err(Io)
    #[inline]
    pub fn write_do_channel(&self, idx: usize, on: bool) -> AppResult<()> {
        if idx >= hw_version::DO_COUNT {
            return Err(AppError::Io(format!(
                "DO idx {} out of range (DO_COUNT={})",
                idx,
                hw_version::DO_COUNT
            )));
        }
        let current = self.do_cache.load_bits();
        let new_val = if on {
            current | (1u64 << idx)
        } else {
            current & !(1u64 << idx)
        };
        self.write_do(new_val)
    }

    /// 读取当前 DO 输出状态 (从缓存, 不读 I2C)
    #[inline]
    pub fn read_do_cached(&self) -> u64 {
        self.do_cache.load_bits()
    }

    /// 读取当前 DO 输出状态 (从所有 MCP23017 读取, F4 需 3 次 I2C 读)
    pub fn read_do_actual(&self) -> AppResult<u64> {
        let mut bus = self.bus.lock();
        let mut result: u64 = 0;
        for i in 0..self.do_chip_count {
            let bits = self.do_chips[i].read_outputs(&mut *bus)? as u64;
            result |= bits << (i * 16);
        }
        Ok(result)
    }

    /// 获取 I2C 总线互斥锁 (供高级用例, 如扫描其它 I2C 设备)
    pub fn bus(&self) -> &Spin<I2cBus> {
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
    /// F4 (无 DO): 空操作, 仅更新缓存
    fn write_do_all(&self, value: u64) -> AppResult<()> {
        self.write_do(value)
    }

    /// 写入单个 DO 通道 (读改写, 含一次 I2C 读 + 一次 I2C 写)
    /// F4 (无 DO): idx 越界返回 Err
    fn write_do(&self, idx: usize, on: bool) -> AppResult<()> {
        self.write_do_channel(idx, on)
    }

    /// 读取 DO 缓存 (不读 I2C, <1μs)
    /// F4 (无 DO): 永远返回 0
    fn read_do_cached(&self) -> u64 {
        IoExtender::read_do_cached(self)
    }

    /// 读取 DO 实际硬件状态 (读 MCP23017 OLAT, 约 200μs)
    /// F4 (无 DO): 永远返回 0
    fn read_do_actual(&self) -> AppResult<u64> {
        IoExtender::read_do_actual(self)
    }
}

// ============================================================================
// 单元测试 — IoExtender 通道范围检查
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::hw_version;

    #[test]
    #[cfg(feature = "f3")]
    fn test_f3_channel_count() {
        // F3: 16 DI + 16 DO
        assert_eq!(hw_version::DI_COUNT, 16);
        assert_eq!(hw_version::DO_COUNT, 16);
    }

    #[test]
    #[cfg(feature = "f4")]
    fn test_f4_channel_count() {
        // F4: 48 DI + 48 DO
        assert_eq!(hw_version::DI_COUNT, 48);
        assert_eq!(hw_version::DO_COUNT, 48);
        assert_eq!(hw_version::DO_EXT_CHIPS, 3);
    }

    #[test]
    #[cfg(feature = "f4")]
    fn test_f4_do_addr_list() {
        // F4 DO 地址: 0x23/0x24/0x25 (DI 已占 0x20/0x21/0x22)
        assert_eq!(crate::config::io_ext::DO_ADDRS.len(), 3);
        assert_eq!(crate::config::io_ext::DO_ADDRS[0], 0x23);
        assert_eq!(crate::config::io_ext::DO_ADDRS[1], 0x24);
        assert_eq!(crate::config::io_ext::DO_ADDRS[2], 0x25);
    }

    #[test]
    fn test_di_addr_list() {
        // 验证 F3/F4 DI 地址列表
        #[cfg(feature = "f3")]
        {
            assert_eq!(crate::config::io_ext::DI_ADDRS.len(), 1);
            assert_eq!(crate::config::io_ext::DI_ADDRS[0], 0x20);
        }
        #[cfg(feature = "f4")]
        {
            assert_eq!(crate::config::io_ext::DI_ADDRS.len(), 3);
            assert_eq!(crate::config::io_ext::DI_ADDRS[0], 0x20);
            assert_eq!(crate::config::io_ext::DI_ADDRS[1], 0x21);
            assert_eq!(crate::config::io_ext::DI_ADDRS[2], 0x22);
        }
    }

    #[test]
    #[cfg(feature = "f4")]
    fn test_f4_no_do_addr() {
        // F4 无 DO 芯片, 地址占位为 0
        assert_eq!(crate::config::io_ext::DO_ADDR, 0);
    }
}
