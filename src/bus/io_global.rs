//! 全局 IO 状态 (原子, 无锁)
//!
//! 包含: di, do_, ai, ao, sys 全部使用原子操作, 零锁开销。

use std::sync::atomic::AtomicU32;
use std::sync::LazyLock;

use super::io_state::*;

/// 全局 IO 状态 (无锁, 全原子操作)
pub static IO: LazyLock<IoBundle> = LazyLock::new(IoBundle::new);

/// DO NVS 持久化: 上次成功写入 NVS 的位图 (用于去重, 避免重复写相同值).
/// LOOP13: 使用 AtomicBits64 (Xtensa 无 AtomicU64 的真无锁实现).
pub static DO_PERSIST_LAST: crate::sync::AtomicBits64 = crate::sync::AtomicBits64::new(0);
/// DO NVS 持久化: 上次写 NVS 的 monotonic 毫秒数 (用于 1s 节流)
pub static DO_PERSIST_LAST_MS: AtomicU32 = AtomicU32::new(0);

/// IO 状态集合 (5 个独立原子结构)
pub struct IoBundle {
    pub di: DiState,
    pub do_: DoState,
    pub ai: AiState,
    pub ao: AoState,
    pub sys: SysState,
}

impl IoBundle {
    pub const fn new() -> Self {
        // 注意: 在 const fn 中不能直接构造 Default
        // 我们用 const-friendly 的方式初始化
        Self {
            di: DiState {
                bits: crate::sync::AtomicBits64::new(0),
            },
            do_: DoState {
                bits: crate::sync::AtomicBits64::new(0),
            },
            ai: AiState {
                raw: [
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                ],
                scaled: [
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                ],
            },
            ao: AoState {
                scaled: [
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                    std::sync::atomic::AtomicU16::new(0),
                ],
                duty: [
                    std::sync::atomic::AtomicU32::new(0),
                    std::sync::atomic::AtomicU32::new(0),
                    std::sync::atomic::AtomicU32::new(0),
                    std::sync::atomic::AtomicU32::new(0),
                ],
            },
            sys: SysState {
                firmware_version: std::sync::atomic::AtomicU16::new(0),
                uptime_s: std::sync::atomic::AtomicU32::new(0),
                reset_count: std::sync::atomic::AtomicU16::new(0),
                reset_reason: std::sync::atomic::AtomicU8::new(0),
                reset_request: std::sync::atomic::AtomicU8::new(0),
                log_level: std::sync::atomic::AtomicU8::new(2),
            },
        }
    }
}

impl Default for IoBundle {
    fn default() -> Self {
        Self::new()
    }
}
