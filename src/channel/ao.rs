//! AO 模拟输出任务
//!
//! - 周期 100ms 读取 `BUS.ao.scaled`，转换为 duty 写入 `BUS.ao.duty`
//! - 调用 `hal.ledc.set_duty(idx, duty)` 输出 PWM
//! - 0-10000 (0-10V) 对应 duty 0-4095 (12-bit)
//!
//! 工程量尺度：scaled = 工程量 * 1000 (0..=10000 表示 0.000-10.000 V)

use std::sync::Arc;

use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::TaskHb;
use crate::sync::MainLoopCell;

/// 输出周期 (ms)
const OUTPUT_PERIOD_MS: u64 = 100;
/// AO 通道数
const CHANNEL_COUNT: usize = 4;

/// scaled 上限 (0-10V 对应 0-10000)
const SCALED_MAX: u32 = 10000;
/// LEDC duty 上限 (12-bit, 由 config::pins::AO_RESOLUTION_BITS 推导)
const DUTY_MAX: u32 = (1u32 << (crate::config::pins::AO_RESOLUTION_BITS as u32)) - 1;

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("ao-output");

/// 启动 AO 输出任务
///
/// TODO: 假设 `hal.ledc.set_duty(idx: usize, duty: u32) -> ()`，由 hal/ledc 模块实现后接入。
// 架构改造 Phase 2: AO 合并到 main_loop
// 状态用 MainLoopCell 保护 (单线程访问, 零开销)

struct AoState {
    last_duty: [u32; CHANNEL_COUNT],
}

static AO_STATE: MainLoopCell<AoState> = MainLoopCell::new();

pub fn start_output_task(_hal: Arc<Hal>) -> AppResult<()> {
    AO_STATE.init(AoState {
        last_duty: [u32::MAX; CHANNEL_COUNT],
    });
    log::info!("[ao] output task registered in main_loop (period={}ms)", OUTPUT_PERIOD_MS);
    Ok(())
}

/// main_loop 每 100ms 调用一次
pub fn tick_ao_output(hal: &crate::hal::Hal) {
    let state = match AO_STATE.get_mut() {
        Some(s) => s,
        None => return,
    };
    TASK_HB.tick();

    let mut duties = [0u32; CHANNEL_COUNT];
    for ch in 0..CHANNEL_COUNT {
        let scaled = crate::bus::IO.ao.get_scaled(ch);
        duties[ch] = (scaled as u32) * DUTY_MAX / SCALED_MAX;
        crate::bus::IO.ao.set_duty(ch, duties[ch]);
    }
    crate::bus::send_event(crate::bus::IoEvent::AoUpdated);

    // 仅在 duty 变化时调用 LEDC (复用上面已取的 &mut)
    for ch in 0..CHANNEL_COUNT {
        if duties[ch] != state.last_duty[ch] {
            hal.ledc.set_duty(ch, duties[ch]);
            state.last_duty[ch] = duties[ch];
        }
    }
}
