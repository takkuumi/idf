//! 数字 IO 抽象 trait (设备抽象层)
//!
//! 统一 DI (数字输入) / DO (数字输出) 的访问接口, 屏蔽底层硬件差异:
//! - 默认版本: GPIO 直驱 ([`crate::hal::gpio::GpioBank`])
//! - F3/F4 版本: I2C MCP23017 扩展 ([`crate::hal::io_ext::IoExtender`])
//!
//! # 设计目标
//!
//! 1. **统一接口**: 上层 (io/di.rs, io/do_.rs, Modbus) 不关心 DI/DO 走 GPIO 还是 I2C
//! 2. **扩展性**: 新增硬件版本只需实现 `DigitalIo` trait, 无需修改上层代码
//! 3. **零成本抽象**: trait object 的 vtable 调用约 1-2ns, 相对 I2C 600μs 完全可忽略
//!
//! # 使用方式
//!
//! ```no_run
//! // Hal 提供 dio() 方法返回 &dyn DigitalIo
//! let dio = hal.dio();
//! let di_bits = dio.read_di_all()?;      // 读取所有 DI
//! dio.write_do_all(0xFFFF)?;             // 写入所有 DO
//! let ch0 = dio.read_di(0)?;             // 读取单个 DI
//! ```

use crate::error::AppResult;

/// 数字 IO 抽象 trait
///
/// 实现 `Send + Sync` 以支持多线程访问 (DI 扫描 + DO 输出 + Modbus 读状态)。
/// 底层实现需自行保证线程安全 (GpioBank 用 `&self` 读 / Mutex 写; IoExtender 全 Mutex)。
pub trait DigitalIo: Send + Sync {
    /// DI 通道数 (默认 8 / F3 16 / F4 48)
    fn di_count(&self) -> usize;

    /// DO 通道数 (默认 8 / F3 16 / F4 16)
    fn do_count(&self) -> usize;

    /// 读取所有 DI 通道, 返回 bit i = DI i 状态 (1=高, 0=低)
    ///
    /// bit 数量 = `di_count()`, 超出部分为 0。
    /// 默认版本 (GPIO): 无 I2C 延迟, <1μs
    /// F3/F4 (I2C): 400kHz 下 F4 约 600μs (3 片 MCP23017)
    fn read_di_all(&self) -> AppResult<u64>;

    /// 读取单个 DI 通道 (idx 从 0 开始)
    ///
    /// 返回 `true` = 高电平, `false` = 低电平。
    /// idx 越界返回 `Err(AppError::Io)`。
    fn read_di(&self, idx: usize) -> AppResult<bool>;

    /// 写入所有 DO 通道, bit i = DO i 目标状态 (1=高, 0=低)
    ///
    /// bit 数量 = `do_count()`, 超出部分忽略。
    /// 默认版本 (GPIO): 逐通道写, <10μs
    /// F3/F4 (I2C): 单次 I2C 写入, 约 200μs
    fn write_do_all(&self, value: u64) -> AppResult<()>;

    /// 写入单个 DO 通道 (idx 从 0 开始)
    ///
    /// idx 越界返回 `Err(AppError::Io)`。
    fn write_do(&self, idx: usize, on: bool) -> AppResult<()>;

    /// 读取当前 DO 输出缓存 (不读硬件, 快)
    ///
    /// 用于 Modbus 读线圈状态, 避免每次都读 I2C/GPIO。
    /// 默认版本: 读 GPIO 实际电平 (也是缓存效果, GPIO 输出无读回延迟)
    /// F3/F4: 读 do_cache (MCP23017 读回较慢, 用缓存)
    fn read_do_cached(&self) -> u64;

    /// 读取 DO 实际硬件状态 (慢, 用于诊断)
    ///
    /// 默认版本: 同 `read_do_cached` (GPIO 读回快)
    /// F3/F4: 读 MCP23017 OLAT 寄存器 (额外 I2C 读取)
    fn read_do_actual(&self) -> AppResult<u64>;
}
