//! DO 数字输出任务
//!
//! - 默认版本: 周期 10ms 读取 `BUS.do_.bits`, 更新到 8 路 GPIO 输出
//! - F3/F4 版本: 周期 10ms 读取 `BUS.do_.bits`, 更新到 16 路 I2C MCP23017 扩展输出
//! - 仅在状态变化时调用写入，避免抖动

use std::sync::Arc;
use std::time::Duration;

use crate::bus;
use crate::config::hw_version;
use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// 输出周期 (ms)
const OUTPUT_PERIOD_MS: u64 = 10;
/// 心跳节流: 每 10 次循环 (≈100ms) 上报一次
const HB_DIV: u32 = 10;

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("do-output");

/// 启动 DO 输出任务
pub fn start_output_task(hal: Arc<Hal>) -> AppResult<()> {
    health::register(&TASK_HB);
    std::thread::Builder::new()
        .name("do-output".into())
        .spawn(move || {
            health::pin_current_to_core(health::CORE_RT);
            log::info!(
                "[do] output task started, period={}ms, channels={} (version {})",
                OUTPUT_PERIOD_MS, hw_version::DO_COUNT, hw_version::NAME
            );

            // 上一次输出的 DO 状态, 初值 0xFFFF_FFFF_FFFF_FFFF 使首次必然全量刷新
            let mut last: u64 = u64::MAX;
            let mut tick_div: u32 = 0;

            loop {
                tick_div = tick_div.wrapping_add(1);
                if tick_div % HB_DIV == 0 {
                    TASK_HB.tick();
                }
                let bits = match bus::lock_timeout() {
                    Some(b) => b.do_.bits,
                    None => {
                        log::error!("[do] bus lock timeout, keep last");
                        last
                    }
                };

                // 仅在变化时写硬件 (统一通过 DigitalIo trait 访问)
                if bits != last {
                    if let Err(e) = hal.dio().write_do_all(bits) {
                        log::error!("[do] write_do_all failed: {}", e);
                    }
                    last = bits;
                }

                std::thread::sleep(Duration::from_millis(OUTPUT_PERIOD_MS));
            }
        })
        .map_err(|e| AppError::Io(format!("spawn do-output: {e}")))?;

    Ok(())
}
