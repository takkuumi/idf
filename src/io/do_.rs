//! DO 数字输出
//!
//! - main_loop 每 5ms 检查 `BUS.do_.bits`，变化时更新 PCA9555 输出
//! - Modbus/业务写入后调用 `notify()` 置 dirty，下一周期刷新
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::hw_version;
use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::sync::MainLoopCell;

/// 10s 兜底轮询: 即使 dirty 未触发, 也每 10s 检查一次硬件同步
const FALLBACK_TICKS: u32 = 10_000 / crate::config::MAIN_LOOP_PERIOD_MS as u32;
/// 硬件写失败后 100ms 重试，避免 5ms 忙重试占满 I2C，同时不等待 10s 兜底。
const RETRY_TICKS: u32 = 100 / crate::config::MAIN_LOOP_PERIOD_MS as u32;

/// Modbus/其他模块写入 DO 后置位, 通知 DO 任务立即刷新
static DO_DIRTY: AtomicBool = AtomicBool::new(false);

/// 通知 DO 任务有新数据需要立即刷新 (事件驱动, 非阻塞)
pub fn notify() {
    DO_DIRTY.store(true, Ordering::Release);
}

// 架构改造 Phase 2: DO 输出合并到 main_loop
// 原始: 独立 pthread 1ms tick + notify 事件驱动
// 改造后: main_loop 5ms poll + dirty 标志 (Modbus 写入时置位)

struct DoState {
    last: u64,
    /// 主循环 tick 计数器, 用于 10s 兜底定时
    tick_count: u32,
    consecutive_failures: u32,
    retry_ticks: u32,
}

impl DoState {
    /// 返回是否仍需提交；只有成功才能推进 last，短暂争用不进入硬件退避。
    fn submission_completed(&mut self, bits: u64, result: AppResult<()>) -> bool {
        match result {
            Ok(()) => {
                self.last = bits;
                self.consecutive_failures = 0;
                self.retry_ticks = 0;
                false
            }
            Err(AppError::Busy(_)) => {
                self.retry_ticks = 0;
                true
            }
            Err(error) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if self.consecutive_failures == 1
                    || self.consecutive_failures == 3
                    || self.consecutive_failures.is_multiple_of(100)
                {
                    log::error!(
                        "[do] write_do_all failed (attempt={}): {}",
                        self.consecutive_failures,
                        error
                    );
                }
                self.retry_ticks = RETRY_TICKS;
                true
            }
        }
    }
}

static DO_STATE: MainLoopCell<DoState> = MainLoopCell::new();

#[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
pub fn start_output_task(_hal: Arc<Hal>) -> AppResult<()> {
    DO_STATE
        .init(DoState {
            last: u64::MAX,
            tick_count: 0,
            consecutive_failures: 0,
            retry_ticks: 0,
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

/// main_loop 每 5ms 调用一次，dirty 标志提供事件驱动效果。
pub fn tick_do_output(hal: &Hal) {
    let _ = DO_STATE.with_mut(|state| {
        if retry_pending(&mut state.retry_ticks) {
            return;
        }
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
        if bits != state.last && state.submission_completed(bits, hal.dio().write_do_all(bits)) {
            // Busy 下一 tick 重试；硬件故障保留 100ms 退避，均不推进 last。
            DO_DIRTY.store(true, Ordering::Release);
        }
    });
}

fn retry_pending(ticks: &mut u32) -> bool {
    *ticks = ticks.saturating_sub(1);
    *ticks != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_do_failure_waits_full_retry_interval_despite_dirty_flag() {
        let mut ticks = RETRY_TICKS;
        for tick in 1..RETRY_TICKS {
            assert!(retry_pending(&mut ticks), "premature retry at tick {tick}");
        }
        assert!(!retry_pending(&mut ticks));
        assert!(!retry_pending(&mut ticks));
    }

    #[test]
    fn test_do_queue_contention_retries_next_tick_without_false_hardware_failure() {
        let mut state = DoState {
            last: 0xD011,
            tick_count: 0,
            consecutive_failures: 0,
            retry_ticks: 0,
        };
        for _ in 0..3 {
            assert!(state.submission_completed(0xD010, Err(AppError::Busy("DO queue contended"))));
            assert_eq!(state.last, 0xD011);
            assert_eq!(state.consecutive_failures, 0);
            assert!(!retry_pending(&mut state.retry_ticks));
        }
        assert!(!state.submission_completed(0xD010, Ok(())));
        assert_eq!(state.last, 0xD010);
    }

    #[test]
    fn test_do_hardware_failure_keeps_backoff_and_latest_target_after_busy() {
        let mut state = DoState {
            last: 1,
            tick_count: 0,
            consecutive_failures: 0,
            retry_ticks: 0,
        };
        assert!(state.submission_completed(2, Err(AppError::Io("injected failure".into()))));
        assert_eq!(state.last, 1);
        assert_eq!(state.consecutive_failures, 1);
        for _ in 1..RETRY_TICKS {
            assert!(retry_pending(&mut state.retry_ticks));
        }
        assert!(!retry_pending(&mut state.retry_ticks));
        assert!(state.submission_completed(3, Err(AppError::Busy("DO queue full"))));
        assert_eq!(state.last, 1);
        assert_eq!(state.consecutive_failures, 1);
        assert!(!retry_pending(&mut state.retry_ticks));
        assert!(!state.submission_completed(4, Ok(())));
        assert_eq!(state.last, 4);
        assert_eq!(state.consecutive_failures, 0);
    }
}
