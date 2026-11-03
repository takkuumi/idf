//! 数字 IO 抽象 trait (设备抽象层)
//!
//! 统一 DI (数字输入) / DO (数字输出) 的访问接口, 屏蔽底层硬件差异:
//! - 所有版本: 软件 I2C PCA9555 组 ([`crate::hal::pca9555::Pca9555Duo`])
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
/// 底层实现需自行保证线程安全；PCA9555 通过独立 worker 和原子快照提供数据。
pub trait DigitalIo: Send + Sync {
    /// DI 通道数 (默认/F3 16 / F4 48)
    fn di_count(&self) -> usize;

    /// DO 通道数 (默认/F3 16 / F4 48)
    fn do_count(&self) -> usize;

    /// 读取所有 DI 通道, 返回 bit i = DI i 状态 (1=高, 0=低)
    ///
    /// bit 数量 = `di_count()`, 超出部分为 0。
    /// PCA9555 软件 I2C worker 提供的最新有效采样
    fn read_di_all(&self) -> AppResult<u64>;

    /// 返回采样序号；异步驱动用来避免把同一缓存误计为多次去抖采样。
    /// 同步驱动每次读取都是新样本，因此序号为 None。外层 None 表示发布中，稍后再读。
    fn read_di_sample(&self) -> AppResult<Option<(u64, Option<u32>)>> {
        self.read_di_all().map(|bits| Some((bits, None)))
    }

    /// 读取单个 DI 通道 (idx 从 0 开始)
    ///
    /// 返回 `true` = 高电平, `false` = 低电平。
    /// idx 越界返回 `Err(AppError::Io)`。
    fn read_di(&self, idx: usize) -> AppResult<bool>;

    /// 写入所有 DO 通道, bit i = DO i 目标状态 (1=高, 0=低)
    ///
    /// bit 数量 = `do_count()`, 超出部分忽略。
    /// PCA9555 worker 队列提交；Busy 表示尚未接受，调用方需重试。
    fn write_do_all(&self, value: u64) -> AppResult<()>;

    /// 写入单个 DO 通道 (idx 从 0 开始)
    ///
    /// idx 越界返回 `Err(AppError::Io)`。
    fn write_do(&self, idx: usize, on: bool) -> AppResult<()>;

    /// 读取当前 DO 输出缓存 (不读硬件, 快)
    ///
    /// 用于 Modbus 读线圈状态, 避免每次都读 I2C/GPIO。
    /// 读取 PCA9555 输出缓存
    fn read_do_cached(&self) -> u64;

    /// 读取输出寄存器状态；异步驱动返回有时效检查的最近硬件读回。
    /// 这不是继电器触点或负载电流反馈。
    ///
    /// 读取 worker 最近确认的 PCA9555 OUTPUT 寄存器值
    fn read_do_actual(&self) -> AppResult<u64>;
}
