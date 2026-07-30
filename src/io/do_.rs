//! DO 数字输出
//!
//! - main_loop 每 20ms 检查 `BUS.do_.bits`，变化时更新 PCA9555 输出
//! - Modbus/业务写入后调用 `notify()` 置 dirty，下一周期刷新
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::hw_version;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::sync::MainLoopCell;

/// 10s 兜底轮询: 即使 dirty 未触发, 也每 10s 检查一次硬件同步
const FALLBACK_TICKS: u32 = 10_000 / crate::config::MAIN_LOOP_PERIOD_MS as u32;

/// Modbus/其他模块写入 DO 后置位, 通知 DO 任务立即刷新
static DO_DIRTY: AtomicBool = AtomicBool::new(false);

/// 通知 DO 任务有新数据需要立即刷新 (事件驱动, 非阻塞)
pub fn notify() {
    DO_DIRTY.store(true, Ordering::Release);
}

// 架构改造 Phase 2: DO 输出合并到 main_loop
// 原始: 独立 pthread 1ms tick + notify 事件驱动
// 改造后: main_loop 20ms poll + dirty 标志 (Modbus 写入时置位)

struct DoState {
    last: u64,
    /// 主循环 tick 计数器, 用于 10s 兜底定时
    tick_count: u32,
}

static DO_STATE: MainLoopCell<DoState> = MainLoopCell::new();

#[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
pub fn start_output_task(_hal: Arc<Hal>) -> AppResult<()> {
    DO_STATE
        .init(DoState {
            last: u64::MAX,
            tick_count: 0,
        })
        .map_err(|_| crate::error::AppError::Io("DO state busy during init".into()))?;
    let _ = _hal;
    log::info!(
        "[do] output task registered in main_loop (fallback={} ticks version {})",
        FALLBACK_TICKS,
        hw_version::NAME
    );
    Ok(())
}

/// main_loop 每 20ms 调用一次，dirty 标志提供事件驱动效果。
pub fn tick_do_output(hal: &Hal) {
    let _ = DO_STATE.with_mut(|state| {
        let dirty = DO_DIRTY.swap(false, Ordering::Acquire);
        state.tick_count = state.tick_count.wrapping_add(1);

        // LOOP13: 两路触发: dirty=事件驱动, tick_count 溢出=10s 兜底
        // dirty=true 立即写并重置计数器；否则达到 FALLBACK_TICKS 时强制同步。
        if !dirty && state.tick_count < FALLBACK_TICKS {
            return;
        }
        // 进入写路径: 重置计数器 (兜底或 dirty 二选一)
        state.tick_count = 0;

        let bits = crate::bus::IO.do_.load_bits();
        if bits != state.last {
            if let Err(e) = hal.dio().write_do_all(bits) {
                log::error!("[do] write_do_all failed: {}", e);
            }
            state.last = bits;
        }
    });
}
