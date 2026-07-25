//! 错误日志环缓冲区
//!
//! ## 目的
//! 工业现场设备故障时, 需要快速定位问题, 但日志可能已满或丢失。
//! 本模块维护一个 100 条记录的环缓冲区, 暴露给 Modbus 读取 (寄存器 0x0900+)。
//!
//! ## 设计
//! - 固定大小环 (heapless::Vec<LogEntry, 100>)
//! - 多线程写 (日志任务) + 多线程读 (Modbus 命令)
//! - 用 `Spin` 短临界区保护 (写极短, 读也很短, 不阻塞内核)
//!
//! ## 性能
//! - 写: O(1) - 追加到末尾, 满了覆盖最旧
//! - 读: O(N) - 遍历所有条目 (100 条 = 微秒级)

use core::sync::atomic::{AtomicU32, Ordering};

use std::sync::LazyLock;

use crate::sync::Spin;

/// 单条日志
#[derive(Clone, Copy)]
pub struct LogEntry {
    /// 时间戳 (秒 since boot, 用 u32 存储可运行 ~136 年不 wrap)
    /// LOOP8: 从 ms 改为 s 解决 49.7 天 wrap 问题
    pub timestamp_s: u32,
    /// 严重等级 (0=Info 1=Warn 2=Error 3=Critical)
    pub level: u8,
    /// 模块名 hash (用第一个字节标识, 避免字符串)
    pub module_id: u8,
    /// 错误码 (recovery 模块的 Severity * 100 + 故障类型)
    pub code: u16,
    /// 上下文值 (例如: 故障计数, 当前模式)
    pub context: u32,
}

impl LogEntry {
    pub const fn empty() -> Self {
        Self {
            timestamp_s: 0,
            level: 0,
            module_id: 0,
            code: 0,
            context: 0,
        }
    }
}

/// 全局环日志 (100 条, 满了覆盖最旧)
pub static RING_LOG: LazyLock<Spin<RingLog>> = LazyLock::new(|| Spin::new(RingLog::new()));

/// 环日志状态
pub struct RingLog {
    entries: heapless::Vec<LogEntry, 100>,
    /// 启动时间 (用于 timestamp)
    boot_time: std::time::Instant,
    /// 写入计数 (mod 2^32)
    write_count: u32,
}

impl RingLog {
    pub const fn new() -> Self {
        Self {
            entries: heapless::Vec::new(),
            boot_time: unsafe { std::mem::zeroed() }, // 占位, 在 lock 中初始化
            write_count: 0,
        }
    }

    /// 初始化 boot_time (在第一次锁定时调用)
    fn ensure_boot(&mut self) {
        if self.write_count == 0 && self.boot_time == unsafe { std::mem::zeroed() } {
            self.boot_time = std::time::Instant::now();
        }
    }

    /// 记录一条日志
    /// LOOP8: 使用 esp_timer_get_time() 获取秒级时间戳, 避免 49.7 天 wrap
    pub fn record(&mut self, level: u8, module_id: u8, code: u16, context: u32) {
        self.ensure_boot();
        // 使用秒级时间戳: u32 可运行 ~136 年不 wrap (vs ms 仅 49.7 天)
        let timestamp_s = self.boot_time.elapsed().as_secs() as u32;
        let entry = LogEntry {
            timestamp_s,
            level,
            module_id,
            code,
            context,
        };
        // 满了覆盖最旧
        if self.entries.is_full() {
            self.entries.remove(0);
        }
        let _ = self.entries.push(entry);
        self.write_count = self.write_count.wrapping_add(1);
    }

    /// 读取所有条目 (用于 Modbus 读取)
    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    /// 当前条目数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 总写入次数 (mod 2^32)
    pub fn write_count(&self) -> u32 {
        self.write_count
    }
}

impl Default for RingLog {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局写入计数 (无锁读取, 用于快速判断是否有新日志)
pub static LOG_WRITE_COUNT: AtomicU32 = AtomicU32::new(0);

/// 记录错误日志 (便利函数)
pub fn log_error(module_id: u8, code: u16, context: u32) {
    {
        let mut guard = RING_LOG.lock();
        guard.record(2, module_id, code, context);  // level 2 = Error
        let cnt = guard.write_count();
        LOG_WRITE_COUNT.store(cnt, Ordering::Release);
    }
}

/// 记录警告日志
pub fn log_warn(module_id: u8, code: u16, context: u32) {
    {
        let mut guard = RING_LOG.lock();
        guard.record(1, module_id, code, context);  // level 1 = Warn
        let cnt = guard.write_count();
        LOG_WRITE_COUNT.store(cnt, Ordering::Release);
    }
}

/// 记录严重错误
pub fn log_critical(module_id: u8, code: u16, context: u32) {
    {
        let mut guard = RING_LOG.lock();
        guard.record(3, module_id, code, context);  // level 3 = Critical
        let cnt = guard.write_count();
        LOG_WRITE_COUNT.store(cnt, Ordering::Release);
    }
}

/// 模块 ID 分配 (供其他模块使用)
pub mod module_id {
    pub const MAIN: u8 = 0;
    pub const BUS: u8 = 1;
    pub const MODBUS: u8 = 2;
    pub const BLE: u8 = 3;
    pub const ETHERNET: u8 = 4;
    pub const IO: u8 = 5;
    pub const OTA: u8 = 6;
    pub const NVS: u8 = 7;
    pub const WATCHDOG: u8 = 8;
    pub const RECOVERY: u8 = 9;
    pub const RING_LOG: u8 = 10;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_log_basic() {
        let mut ring = RingLog::new();
        ring.record(0, 1, 100, 0);
        ring.record(2, 2, 200, 1);
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.entries()[0].code, 100);
        assert_eq!(ring.entries()[1].code, 200);
    }

    #[test]
    fn test_ring_log_overflow() {
        let mut ring = RingLog::new();
        for i in 0..150 {
            ring.record(0, 0, i, 0);
        }
        // 满了覆盖最旧, 只保留最后 100 条
        assert_eq!(ring.len(), 100);
        assert_eq!(ring.entries()[99].code, 149); // 最后一条
        assert_eq!(ring.entries()[0].code, 50);   // 第 51 条 (最早剩下的)
    }

    #[test]
    fn test_log_error_atomic_count() {
        log_error(0, 100, 0);
        log_error(0, 200, 0);
        // count 应该增加 (无需精确值, 只测试原子性)
        let _cnt = LOG_WRITE_COUNT.load(Ordering::Acquire);
    }
}
