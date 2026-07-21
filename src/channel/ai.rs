//! AI 模拟输入采样任务
//!
//! - 周期 100ms 采样 6 路 ADC1 (12-bit)
//! - 滑动平均 (窗口 8)
//! - 4-20mA 转换：假设 ADC 0-3.3V 对应 4-20mA，可标定
//! - 写入 `BUS.ai.raw` (滑动平均后的原始值) 和 `BUS.ai.scaled` (mA*1000)

use std::sync::Arc;


use crate::config::ai_calib;
use crate::error::AppResult;
use crate::hal::Hal;
use esp_idf_svc::timer::EspTaskTimerService;
use crate::health::{self, TaskHb};
use crate::sync::MainLoopCell;

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
// 架构改造 Phase 2: AI 合并到 main_loop
// 状态用 MainLoopCell 保护 (单线程访问, 无 atomic CAS 开销)


struct AiState {
    buf: [[u16; AVG_WINDOW]; CHANNEL_COUNT],
    pos: usize,
    filled: usize,
}

static AI_STATE: MainLoopCell<AiState> = MainLoopCell::new();

pub fn start_sample_task(hal: Arc<Hal>, _timer_svc: EspTaskTimerService) -> AppResult<()> {
    health::register(&TASK_HB);
    // 不再创建独立 pthread. ADC driver 通过 hal.adc 访问, 状态由 main_loop 持有.
    AI_STATE.init(AiState {
        buf: [[0u16; AVG_WINDOW]; CHANNEL_COUNT],
        pos: 0,
        filled: 0,
    });
    let _ = hal; // 抑制 unused 警告
    log::info!("[ai] sample task registered in main_loop (period={}ms)", SAMPLE_PERIOD_MS);
    Ok(())
}

/// main_loop 每 100ms 调用一次
pub fn tick_ai_sample(hal: &crate::hal::Hal) {
    let state = match AI_STATE.get_mut() {
        Some(s) => s,
        None => return,
    };

    TASK_HB.tick();
    let raws: [u16; 6] = hal.adc.sample_all();
    for ch in 0..CHANNEL_COUNT {
        let raw: u16 = raws[ch];
        state.buf[ch][state.pos % AVG_WINDOW] = raw;
        let sum: u32 = state.buf[ch].iter().map(|&v| v as u32).sum();
        let avg = if state.filled >= AVG_WINDOW {
            (sum >> AVG_SHIFT) as u16
        } else {
            (sum / state.filled.max(1) as u32) as u16
        };
        let scaled = ai_calib::MA_MIN
            + (avg as u32) * (ai_calib::MA_MAX - ai_calib::MA_MIN) / ai_calib::ADC_MAX;
        crate::bus::IO.ai.set_raw(ch, avg);
        crate::bus::IO.ai.set_scaled(ch, scaled as u16);
    }
    state.pos = (state.pos + 1) % AVG_WINDOW;
    state.filled = (state.filled + 1).min(AVG_WINDOW);
    crate::bus::send_event(crate::bus::IoEvent::AiSampled);
}
