//! DO 数字输出任务
//!
//! - F16 版本: 周期 ~1ms 检查 `BUS.do_.bits`, 变化时更新 PCA9555 输出
//! - 支持事件驱动: Modbus 写入后调用 `notify()` 立即触发刷新
//! - 仅在状态变化时写入硬件, 避免不必要的 I2C 操作

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::bus;
use crate::config::hw_version;
use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// 输出刷新间隔 (ms) — 细粒度检查, 配合 notify 实现亚 ms 级响应
const TICK_MS: u64 = 1;
/// 最大轮询间隔 (ms) — notify 未触发时也在此时间内刷新
const MAX_POLL_MS: u64 = 10;
/// 心跳节流: 每 100ms 上报一次
const HB_DIV: u32 = 100;

static TASK_HB: TaskHb = TaskHb::new("do-output");

/// Modbus/其他模块写入 DO 后置位, 通知 DO 任务立即刷新
static DO_DIRTY: AtomicBool = AtomicBool::new(false);

/// 通知 DO 任务有新数据需要立即刷新 (事件驱动, 非阻塞)
pub fn notify() {
    DO_DIRTY.store(true, Ordering::Release);
}

#[cfg(any(feature_io_di_do, feature_f3, feature_f4))]
pub fn start_output_task(hal: Arc<Hal>) -> AppResult<()> {
    health::register(&TASK_HB);
    health::set_next_thread_core(health::CORE_RT);
    let result = std::thread::Builder::new()
        .name("do-output".into())
        .spawn(move || {
            log::info!(
                "[do] output task started, tick={}ms, max_poll={}ms, channels={} (version {})",
                TICK_MS, MAX_POLL_MS, hw_version::DO_COUNT, hw_version::NAME
            );

            let mut last: u64 = u64::MAX;
            let mut tick_div: u32 = 0;
            let mut since_last_write: u64 = 0;

            loop {
                tick_div = tick_div.wrapping_add(1);
                if tick_div % HB_DIV == 0 {
                    TASK_HB.tick();
                }

                // 检查是否需要刷新: notify 触发 或 达到最大轮询间隔
                let dirty = DO_DIRTY.swap(false, Ordering::Acquire);
                since_last_write += TICK_MS;

                if dirty || since_last_write >= MAX_POLL_MS {
                    since_last_write = 0;
                    let bits = match bus::lock_timeout() {
                        Some(b) => b.do_.bits,
                        None => {
                            log::error!("[do] bus lock timeout, keep last");
                            last
                        }
                    };
                    if bits != last {
                        if let Err(e) = hal.dio().write_do_all(bits) {
                            log::error!("[do] write_do_all failed: {}", e);
                        }
                        last = bits;
                    }
                }

                std::thread::sleep(Duration::from_millis(TICK_MS));
            }
        });
    health::reset_thread_core();
    result.map_err(|e| AppError::Io(format!("spawn do-output: {e}")))?;

    Ok(())
}
