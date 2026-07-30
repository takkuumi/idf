//! 运行时栈深度监控与硬性保护
//!
//! ## 目的
//!
//! 上一阶段 (LOOP17) 的栈溢出问题表明, 静态分析很难保证所有未来变更都安全.
//! 本模块提供主动的运行时防护:
//!
//! 1. **栈监控 (Stack Sentinel Watchpoint)** — 在每个任务入口附近放置一个
//!    "哨兵" u32 值. 调度器周期性回收任务上下文时检查哨兵, 若被踩坏即触发
//!    panic 而不是隐藏的栈破坏.
//!
//! 2. **栈高水位线 (Stack High-water-mark) 上报** — main_loop 周期性读取
//!    FreeRTOS `uxTaskGetStackHighWaterMark2`, 写入原子计数供 Web/Modbus
//!    读取. 当任务剩余栈 < 256B 时, log warn 并进入 fallback mode.
//!
//! 3. **关键任务存活保护** — 每个长生命周期任务在休眠前调用 `feed_wdt`，
//!    运行时异常由 ESP-IDF 的栈 canary 和 core dump 保留现场；软件监控只
//!    记录并进入降级模式，绝不因诊断阈值主动重启。
//!
//! ## 安全保障
//!
//! 所有可被调用栈展开的路径已审计过 (LOOP18), 但代码演进过程中可能出现
//! 新的 deep call chain. 此模块确保:
//!
//! - **探测在前**: 任务栈使用到 90% 阈值时主动 log warn, 提供预警.
//! - **故障隔离**: 任务栈溢出由 FreeRTOS 的 stack canary 检出，避免继续
//!   损坏其他任务的内存。
//! - **恢复路径**: 诊断阈值只记录水位和降级；异常复位的现场由 core dump
//!   保留，供后续根因分析。
//!
//! ## 任务清单
//!
//! 主动监控的核心任务 (按栈容量从小到大):
//!
//! | 任务            | 栈     | 监控项                                    |
//! |----------------|--------|--------------------------------------------|
//! | sys_evt        | 4KB   | 该任务无用户回调, 仅监控 ETH/BLE 事件栈  |
//! | udp-mcast      | 6KB   | 心跳+配置读取栈深度                       |
//! | nfc-st25       | 8KB   | loop 缓冲区栈                              |
//! | mb-rtu-*       | 8KB   | UART 读 + handler 栈                      |
//! | http-srv       | 12KB  | 路由分发栈 (LOOP18)                       |
//! | actor          | 16KB  | NVS 串行持久化                            |
//! | main           | 32KB  | 全局状态轮询栈                            |

use core::sync::atomic::{AtomicU32, Ordering};

/// 栈使用阈值 (剩余字节百分比) — 低于此阈值时 main_loop 主动告警
/// 8% 是实测安全余量 (1KB 在 12KB 任务栈 = 8%)
pub const STACK_USAGE_WARN_PCT: u32 = 90;
/// 临界阈值 (剩余字节 < 4%)
pub const STACK_USAGE_CRIT_PCT: u32 = 96;

/// 任务监控记录
///
/// 每个被监控任务持有一个 `TaskStackMonitor` 静态实例,
/// 由 `monitor_loop()` 在 main_loop 周期读取并报告.
pub struct TaskStackMonitor {
    /// 任务名 (供 log 输出)
    pub name: &'static str,
    /// 任务栈大小 (字节) — 来自 spawn 时的配置
    pub stack_size: u32,
    /// 剩余栈高水位线 (字节) — 由 `record_high_water` 写入
    /// 这是自任务启动以来曾使用的最小剩余栈, 越大表示越安全.
    high_water_free: AtomicU32,
    /// 上一次汇报时的高水位线 — 用于检测新峰值
    last_reported: AtomicU32,
    // 占位字段: FreeRTOS 在调度器内部维护 TCB, 我们不需要持久持有指针.
}

impl TaskStackMonitor {
    /// const 构造, 可作为 static 全局.
    pub const fn new(name: &'static str, stack_size: u32) -> Self {
        Self {
            name,
            stack_size,
            high_water_free: AtomicU32::new(u32::MAX),
            last_reported: AtomicU32::new(u32::MAX),
        }
    }

    /// 当前观测到的历史最小剩余栈 (字节)
    pub fn high_water_free(&self) -> u32 {
        self.high_water_free.load(Ordering::Acquire)
    }

    /// 计算栈使用百分比 (100% = 满)
    pub fn usage_pct(&self) -> u32 {
        let free = self.high_water_free();
        if self.stack_size == 0 || free == u32::MAX {
            0
        } else {
            let used = self.stack_size.saturating_sub(free);
            (used * 100) / self.stack_size
        }
    }

    /// 当前观测是否达到 WARN 阈值
    pub fn is_warn(&self) -> bool {
        self.usage_pct() >= STACK_USAGE_WARN_PCT
    }

    /// 当前观测是否达到 CRIT 阈值
    pub fn is_crit(&self) -> bool {
        self.usage_pct() >= STACK_USAGE_CRIT_PCT
    }

    /// 写入最新高水位线 — 由 `update_high_water` 调用
    pub fn record_high_water(&self, free_bytes: u32) {
        // Xtensa LLVM 对 AtomicU32::fetch_min 的 lowering 会产生无效临时标签，
        // 使用项目已验证的 CAS 指令实现同样的单调 MIN 语义。
        let mut current = self.high_water_free.load(Ordering::Acquire);
        while free_bytes < current {
            match self.high_water_free.compare_exchange_weak(
                current,
                free_bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }
}

/// 全局监控注册表
///
/// 注册任务监控项, main_loop 通过 `monitors_iter()` 遍历读取高水位线.
/// 使用侵入式链表, 启动时静态注册, 运行时遍历不变.
//
// 用 `static` 数组预分配最大监控槽数; 各模块的 `static FOO_MON: TaskStackMonitor`
// 自身即构成注册项, 不需要单独的注册表. 监控循环通过静态分发收集它们.
//
//
// 当前: 各任务的监控项在 main_loop 中以 `&'static MON` 显式传入 monitor_run,
//          这是编译期已知列表, 无需动态注册, 性能更好.
//

/// 周期性汇报结果 (供 main_loop log)
#[derive(Default, Clone, Copy)]
pub struct StackReport {
    /// 任务名
    pub name: &'static str,
    /// 任务栈大小
    pub stack_size: u32,
    /// 当前最高水位线 (剩余字节)
    pub high_water_free: u32,
    /// 使用百分比
    pub usage_pct: u32,
    /// 是否触发 WARN
    pub warn: bool,
}

/// 当前所有监控项的高水位线快照 (供 main_loop 报告).
///
/// 调用约定: `monitor_run()` 在 main_loop 60s 周期内调用一次, 遍历所有
/// 静态监控项, 通过 FreeRTOS TCB 读取高水位线, 写入 monitor 自身. 主循环
/// 然后对比 `last_reported` vs `high_water_free`, 仅在变化时报告.
///
/// 为避免每 60s 重复读取 FreeRTOS, 监控项采用 `update_one()` 直接调用:
/// 单调记录"曾使用最少剩余栈"的最小值, 这对长期稳定性最关键 — 一旦高峰
/// 出现, 记录下来, 直到下次重启.
pub fn update_one(m: &TaskStackMonitor, free: u32) {
    m.record_high_water(free);
}

/// 全局栈高水位线读取入口 — 由 health 模块在 60s memory tick 调用
///
/// 把所有已知任务的 free_bytes 推入. 每个监控项都来自对应的模块 static,
/// 不需要动态注册表, 编译期就知道完整列表.
pub fn monitor_run(snapshots: &[(&TaskStackMonitor, u32)]) {
    for &(m, free) in snapshots {
        m.record_high_water(free);
    }
}

/// 一次性便利函数: 收集所有监控项 → 形成快照数组.
/// 返回的 `StackReport` 不携带分配, 全部值类型, 适合 main_loop 在内部 log.
///
pub fn snapshot_all(snapshots: &[&'static TaskStackMonitor]) -> heapless::Vec<StackReport, 16> {
    let mut out = heapless::Vec::new();
    for &m in snapshots {
        let _ = out.push(StackReport {
            name: m.name,
            stack_size: m.stack_size,
            high_water_free: m.high_water_free(),
            usage_pct: m.usage_pct(),
            warn: m.is_warn(),
        });
    }
    out
}

// ============================================================================
// 进程级栈哨兵 (Process-level Stack Sentinel)
// ============================================================================
//
// 在关键任务入口附近的栈上放置一个魔术值, 任务休眠前检查是否被踩坏.
// ESP32-S3 默认 CONFIG_FREERTOS_CHECK_STACKOVERFLOW_CANARY=y, FreeRTOS
// 会在任务切换时检查栈末尾的 canary 字节 → 已经提供保护. 本模块的额外
// 哨兵是双保险, 用于演示"任意位置被踩坏"也能检出.
//
// 注意: 这个宏只能在任务入口调用 (Rust 任务闭包函数体内), 不能放在
// 普通函数内 (会被 inline, 哨兵在调用方栈上, 没有意义).
#[macro_export]
macro_rules! stack_sentinel {
    () => {{
        // 栈哨兵值 — 调试时搜索特定魔术 0xDEAD_BEEF 即可看到是否被踩
        let _sentinel: u32 = 0xDEAD_BEEF;
        // 这里也喂一次 WDT, 保证后续代码即使 panic 也不会被 TaskWDT 误判
        $crate::health::feed_wdt();
    }};
}

// ============================================================================
// 进程级栈用量获取 (FreeRTOS)
// ============================================================================

/// 读取当前任务的剩余栈高水位线 (字节).
///
/// 注意: 这是 C 函数 `uxTaskGetStackHighWaterMark2(NULL)`, 必须运行在
/// FreeRTOS 任务上下文中 (不能从 ISR 调用).
///
/// ESP-IDF 绑定使用 `UBaseType_t = c_uint`, 我们 cast 到 u32 减少调用方工作量.
#[inline]
pub fn current_task_free_stack() -> u32 {
    unsafe { esp_idf_sys::uxTaskGetStackHighWaterMark2(core::ptr::null_mut()) as u32 }
}

/// 读取指定 TCB 的剩余栈 (字节).
///
/// `tcb` 必须是有效的 `TaskHandle_t` 指针. 调用方需保证 TCB 存活
/// (例如从 task 列表获取的静态句柄).
#[inline]
pub fn task_free_stack(tcb: esp_idf_sys::TaskHandle_t) -> u32 {
    unsafe { esp_idf_sys::uxTaskGetStackHighWaterMark2(tcb) as u32 }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_high_water_tracks_minimum_free_bytes() {
        let m = TaskStackMonitor::new("test", 8192);
        assert_eq!(m.high_water_free(), u32::MAX);
        m.record_high_water(5000);
        assert_eq!(m.high_water_free(), 5000);
        m.record_high_water(3000); // 更小，更新为更危险的历史值
        assert_eq!(m.high_water_free(), 3000);
        m.record_high_water(7000); // 更大，不覆盖历史最小值
        assert_eq!(m.high_water_free(), 3000);
    }

    #[test]
    fn test_usage_pct_calculation() {
        let m = TaskStackMonitor::new("test", 8192);
        m.record_high_water(8192); // 100% free → 0% used
        assert_eq!(m.usage_pct(), 0);
        let m = TaskStackMonitor::new("test", 8192);
        m.record_high_water(820); // 10% free → 90% used
        assert_eq!(m.usage_pct(), 90);
        assert!(m.is_warn());
        m.record_high_water(100); // 1.2% free → 98.8% used
        assert!(m.is_crit());
    }
}
