//! AI 模拟输入采样任务
//!
//! - 周期 100ms 采样：F3/Default 四路，F4 八路 (12-bit)
//! - 滑动平均 (窗口 8)
//! - 反向线性归一化到 0..4095；不是 mA，电流量需按现场两点标定换算
//! - 写入 `BUS.ai.raw` (滑动平均后的 ADC 值)
//! - 写入 `BUS.ai.scaled` (与原 MCA `Get_Analog()` 一致的 0..4095 校准值)
//! - 写入 `BUS.ai.status` (与原 MCA `Analog_Status[]` 一致的状态位)

use std::sync::Arc;

use super::ai_transfer::{MovingAverage, effective_calibration, legacy_scaled_value};
use crate::error::AppResult;
use crate::hal::Hal;
use crate::sync::MainLoopCell;
use esp_idf_svc::timer::EspTaskTimerService;

/// 采样周期 (ms)
const SAMPLE_PERIOD_MS: u64 = 100;
/// AI 通道数。F3/Default 四路，F4 八路。
#[cfg(feature = "f4")]
pub const CHANNEL_COUNT: usize = 8;
#[cfg(not(feature = "f4"))]
pub const CHANNEL_COUNT: usize = 4;

// AI 已合并到 main_loop，状态用 MainLoopCell 保护。

type AiState = MovingAverage<CHANNEL_COUNT>;

static AI_STATE: MainLoopCell<AiState> = MainLoopCell::new();

pub fn start_sample_task(hal: Arc<Hal>, _timer_svc: EspTaskTimerService) -> AppResult<()> {
    // 不再创建独立 pthread. ADC driver 通过 hal.adc 访问, 状态由 main_loop 持有.
    AI_STATE
        .init(AiState::new())
        .map_err(|_| crate::error::AppError::Channel("AI state busy during init".into()))?;
    let _ = hal; // 抑制 unused 警告
    log::info!(
        "[ai] sample task registered in main_loop (period={}ms)",
        SAMPLE_PERIOD_MS
    );
    Ok(())
}

/// main_loop 每 100ms 调用一次
pub fn tick_ai_sample(hal: &crate::hal::Hal) {
    let _ = AI_STATE.with_mut(|state| {
        let raws: [u16; 8] = hal.adc.sample_all();

        // 从 holding_buf 读取本通道校准值 (由 calib::run_auto_calibration 写入)。
        // 已保存的单端标定也必须使用；缺失端点单独回退到默认值。
        let (sensor_min, sensor_max) = read_sensor_calibration();

        let averages = state.push(std::array::from_fn(|ch| raws[ch]));
        for ch in 0..CHANNEL_COUNT {
            let avg = averages[ch];
            let (cal_min, cal_max) = effective_calibration(sensor_min[ch], sensor_max[ch]);
            // 原 MCA: sensorValue = map(raw, max, min, 4095, 0),
            // gucAnalogData = 4095 - sensorValue。因此 raw 越接近 max，输出越接近 0。
            // 代数等价形式是直接把 [min,max] 映射到 [4095,0]。
            let scaled = legacy_scaled_value(avg, cal_min, cal_max);
            let status = if (cal_max as i32 - avg as i32) > 240 {
                0
            } else {
                1
            };
            crate::bus::IO.ai.set_raw(ch, avg);
            crate::bus::IO.ai.set_scaled(ch, scaled);
            crate::bus::IO.ai.set_status(ch, status);
        }
        crate::bus::send_event(crate::bus::IoEvent::AiSampled);
    });
}

/// 从同一存储快照读取物理通道的 SENSOR_MIN/MAX；零表示该端点未标定。
fn read_sensor_calibration() -> ([u16; CHANNEL_COUNT], [u16; CHANNEL_COUNT]) {
    use crate::config::regs;
    let mut minimum = [0u16; CHANNEL_COUNT];
    let mut maximum = [0u16; CHANNEL_COUNT];
    let min_base =
        (regs::HOLD_SENSOR_MIN_BASE as usize).saturating_sub(regs::HOLD_CFG_BASE as usize);
    let max_base =
        (regs::HOLD_SENSOR_MAX_BASE as usize).saturating_sub(regs::HOLD_CFG_BASE as usize);
    // One immutable storage image serves both calibration arrays.
    crate::bus::storage_state::storage_read_with(|snap| {
        for channel in 0..CHANNEL_COUNT {
            let min_index = min_base + channel;
            let max_index = max_base + channel;
            if min_index < snap.holding_buf.len() {
                minimum[channel] = snap.holding_buf[min_index];
            }
            if max_index < snap.holding_buf.len() {
                maximum[channel] = snap.holding_buf[max_index];
            }
        }
    });
    (minimum, maximum)
}
