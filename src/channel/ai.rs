//! AI 模拟输入采样任务
//!
//! - 周期 100ms 采样 6 路 ADC1 (12-bit)
//! - 滑动平均 (窗口 8)
//! - 4-20mA 转换：假设 ADC 0-3.3V 对应 4-20mA，可标定
//! - 写入 `BUS.ai.raw` (滑动平均后的原始值) 和 `BUS.ai.scaled` (mA*1000)

use std::sync::Arc;
use std::time::Duration;

use esp_idf_svc::timer::EspTaskTimerService;

use crate::bus;
use crate::config::ai_calib;
use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// 采样周期 (ms)
const SAMPLE_PERIOD_MS: u64 = 100;
/// 滑动平均窗口大小 (必须为 2 的幂, 用位移替代除法)
const AVG_WINDOW: usize = 8;
/// 位移数 (log2(AVG_WINDOW))
const AVG_SHIFT: u32 = 3;
/// AI 通道数
const CHANNEL_COUNT: usize = 6;

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("ai-sample");

/// 启动 AI 采样任务
///
/// TODO: 假设 `hal.adc.sample(idx: usize) -> u16`，由 hal/adc 模块实现后接入。
/// TODO: `_timer_svc` 可用于更精确的定时采样，当前用 std::thread::sleep。
pub fn start_sample_task(hal: Arc<Hal>, _timer_svc: EspTaskTimerService) -> AppResult<()> {
    health::register(&TASK_HB);
    health::set_next_thread_core(health::CORE_RT);
    let result = std::thread::Builder::new()
        .name("ai-sample".into())
        .spawn(move || {
            log::info!("[ai] sample task started, period={}ms", SAMPLE_PERIOD_MS);

            // 每通道一个环形缓冲区
            let mut buf = [[0u16; AVG_WINDOW]; CHANNEL_COUNT];
            // 当前写入位置 (所有通道共享，因为同步采样)
            let mut pos: usize = 0;
            // 已填充的样本数 (启动初期小于 AVG_WINDOW)
            let mut filled: usize = 0;

            loop {
                // 心跳: 每次循环 (100ms) 上报一次
                TASK_HB.tick();

                // 批量采样 6 通道 (一次 Mutex 锁, 替代 6 次 sample)
                let raws: [u16; 6] = hal.adc.sample_all();

                // 持续累加的 sum, 减少 iter().sum() 重复遍历
                for ch in 0..CHANNEL_COUNT {
                    let raw: u16 = raws[ch];

                    // 写入环形缓冲区
                    buf[ch][pos % AVG_WINDOW] = raw;

                    // 计算滑动平均 (位移优化: 窗口满后用 >> AVG_SHIFT 替代除法)
                    let sum: u32 = buf[ch].iter().map(|&v| v as u32).sum();
                    let avg = if filled >= AVG_WINDOW {
                        // 稳态: 窗口满, 用位移 (更快)
                        (sum >> AVG_SHIFT) as u16
                    } else {
                        // 启动初期: 窗口未满, 用除法
                        let count = filled.max(1) as u32;
                        (sum / count) as u16
                    };

                    // 4-20mA 转换: scaled = MA_MIN + avg * (MA_MAX - MA_MIN) / ADC_MAX
                    let scaled = ai_calib::MA_MIN
                        + (avg as u32) * (ai_calib::MA_MAX - ai_calib::MA_MIN) / ai_calib::ADC_MAX;

                    if let Some(mut b) = bus::lock_timeout() {
                        b.ai.raw[ch] = avg;
                        b.ai.scaled[ch] = scaled as u16;
                    } else {
                        log::error!("[ai] bus lock timeout (ch={})", ch);
                    }
                }

                pos = (pos + 1) % AVG_WINDOW;
                filled = (filled + 1).min(AVG_WINDOW);

                std::thread::sleep(Duration::from_millis(SAMPLE_PERIOD_MS));
            }
        });
    health::reset_thread_core();
    result.map_err(|e| AppError::Channel(format!("spawn ai-sample: {e}")))?;

    Ok(())
}
