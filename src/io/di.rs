//! DI 数字输入扫描任务
//!
//! - 默认版本: 周期 1ms 读取 8 路 GPIO DI (光耦隔离输入)
//! - F3/F4 版本: 周期 1ms 读取 16/48 路 I2C MCP23017 扩展 DI
//! - 软件去抖：连续 3 次相同采样值才确认更新
//! - 检测上升/下降沿 (debug 级别日志)
//!
//! 与总线交互：写入 `BUS.di.bits` (u64, bit i 对应 DI i)

use std::sync::Arc;
use std::time::Duration;

use crate::config::hw_version;
use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// 扫描周期 (ms)
/// DI 扫描周期:
/// - F16 (PCA9555 软件 I2C): 5ms — 留足 I2C 时间, 避免阻塞网络中断
///   (读 2 端口 + 写 2 LED ≈ 4×100μs @250kHz, < 5% CPU)
/// - F3/F4 (I2C MCP23017): 5ms 同理
const SCAN_PERIOD_MS: u64 = 5;
/// 去抖需要的连续相同采样次数
const DEBOUNCE_COUNT: u8 = 3;
/// 心跳节流: 每 100ms 上报一次, 避免原子操作过载
/// 默认版本 (1ms 周期): HB_DIV=100 → 100ms
/// F3/F4 版本 (5ms 周期): HB_DIV=20 → 100ms
#[cfg(not(any(feature = "f3", feature = "f4")))]
const HB_DIV: u32 = 100;
#[cfg(any(feature = "f3", feature = "f4"))]
const HB_DIV: u32 = 20;

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("di-scan");

/// 启动 DI 扫描任务
#[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
pub fn start_scan_task(hal: Arc<Hal>) -> AppResult<()> {
    health::register(&TASK_HB);
    health::set_next_thread_core(health::CORE_RT);
    let result = std::thread::Builder::new()
        .name("di-scan".into())
        .spawn(move || {
            log::info!(
                "[di] scan task started, period={}ms, channels={} (version {})",
                SCAN_PERIOD_MS, hw_version::DI_COUNT, hw_version::NAME
            );

            // 上一次已确认稳定的 DI 状态
            let mut stable: u64 = 0;
            // 待确认的候选值 (去抖窗口内的最新采样)
            let mut candidate: u64 = 0;
            let mut candidate_count: u8 = 0;
            let mut tick_div: u32 = 0;

            loop {
                tick_div = tick_div.wrapping_add(1);
                if tick_div % HB_DIV == 0 {
                    TASK_HB.tick();
                }

                // 1. 采样所有 DI 通道 (统一通过 DigitalIo trait 访问)
                let bits: u64 = match hal.dio().read_di_all() {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("[di] read_di_all failed: {}", e);
                        // 读失败保持上一次值, 避免误报变化
                        candidate
                    }
                };

                // 2. 去抖：连续 DEBOUNCE_COUNT 次相同才确认
                if bits == candidate {
                    candidate_count = candidate_count.saturating_add(1);
                } else {
                    candidate = bits;
                    candidate_count = 1;
                }

                // 3. 稳定值变化时更新总线并打边沿日志
                if candidate_count >= DEBOUNCE_COUNT && candidate != stable {
                    let changed = candidate ^ stable;
                    let rising = candidate & changed;
                    let falling = stable & changed;
                    if rising != 0 {
                        log::debug!("[di] rising  : 0x{:016X}", rising);
                    }
                    if falling != 0 {
                        log::debug!("[di] falling : 0x{:016X}", falling);
                    }
                    stable = candidate;
                    // 阶段 A: 无锁写入 bus::IO.di (AtomicBits64)
                    crate::bus::IO.di.store_bits(stable);
                    crate::bus::send_event(crate::bus::IoEvent::DiChanged);
                }

                std::thread::sleep(Duration::from_millis(SCAN_PERIOD_MS));
            }
        });
    health::reset_thread_core();
    result.map_err(|e| AppError::Io(format!("spawn di-scan: {e}")))?;

    Ok(())
}
