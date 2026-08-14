//! AI 模拟输入采样任务
//!
//! - 周期 100ms 采样 6 路 ADC1 (12-bit)
//! - 滑动平均 (窗口 8)
//! - 4-20mA 转换：假设 ADC 0-3.3V 对应 4-20mA，可标定
//! - 写入 `BUS.ai.raw` (滑动平均后的 ADC 值)
//! - 写入 `BUS.ai.scaled` (与原 MCA `Get_Analog()` 一致的 0..4095 校准值)
//! - 写入 `BUS.ai.status` (与原 MCA `Analog_Status[]` 一致的状态位)

use std::sync::Arc;

use crate::config::ai_calib;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::sync::MainLoopCell;
use esp_idf_svc::timer::EspTaskTimerService;

/// 采样周期 (ms)
const SAMPLE_PERIOD_MS: u64 = 100;
/// 滑动平均窗口大小 (必须为 2 的幂, 用位移替代除法)
const AVG_WINDOW: usize = 8;
/// 位移数 (log2(AVG_WINDOW))
const AVG_SHIFT: u32 = 3;
/// AI 通道数 (LOOP12: F4/F48 设备 8 通道, F16/F3 保持 6 通道).
/// 物理 ADC 仍只采集 6 路 (ESP32-S3 ADC1_CH0..5), F4 模式下 ch[6]/ch[7] 由软件填 0.
/// 未来硬件扩展 (MCP3424 ADC2) 真正补齐 8 路硬件采样时, 只需替换 hal/adc.rs 的 DMA buffer.
#[cfg(feature = "f4")]
pub const CHANNEL_COUNT: usize = 8;
#[cfg(not(feature = "f4"))]
pub const CHANNEL_COUNT: usize = 6;

// AI 已合并到 main_loop，状态用 MainLoopCell 保护。

struct AiState {
    buf: [[u16; AVG_WINDOW]; CHANNEL_COUNT],
    pos: usize,
    filled: usize,
}

static AI_STATE: MainLoopCell<AiState> = MainLoopCell::new();

pub fn start_sample_task(hal: Arc<Hal>, _timer_svc: EspTaskTimerService) -> AppResult<()> {
    // 不再创建独立 pthread. ADC driver 通过 hal.adc 访问, 状态由 main_loop 持有.
    AI_STATE
        .init(AiState {
            buf: [[0u16; AVG_WINDOW]; CHANNEL_COUNT],
            pos: 0,
            filled: 0,
        })
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
        let raws: [u16; 6] = hal.adc.sample_all();

        // 从 holding_buf 读取本通道校准值 (由 calib::run_auto_calibration 写入)。
        // 原 MCA 只有在 min/max 都非零时才启用现场校准，否则使用 605/3016 默认值。
        let sensor_min = read_sensor_calib(true);
        let sensor_max = read_sensor_calib(false);

        for ch in 0..CHANNEL_COUNT {
            let raw: u16 = if ch < raws.len() {
                raws[ch]
            } else {
                0 // F4/F48 设备 ch6/ch7: 硬件暂未接线, 返回 0 (后续扩展 HAL 驱动时替换)
            };
            state.buf[ch][state.pos % AVG_WINDOW] = raw;
            let sum: u32 = state.buf[ch].iter().map(|&v| v as u32).sum();
            let avg = if state.filled >= AVG_WINDOW {
                (sum >> AVG_SHIFT) as u16
            } else {
                (sum / state.filled.max(1) as u32) as u16
            };
            let (cal_min, cal_max) = effective_calibration(sensor_min[ch], sensor_max[ch]);
            // 原 MCA: sensorValue = map(raw, max, min, 4095, 0),
            // gucAnalogData = 4095 - sensorValue。因此 raw 越接近 max，输出越接近 0。
            // 代数等价形式是直接把 [min,max] 映射到 [4095,0]。
            let scaled = map_range(avg, cal_min, cal_max, 4095, 0);
            let status = if (cal_max as i32 - avg as i32) > 240 {
                0
            } else {
                1
            };
            crate::bus::IO.ai.set_raw(ch, avg);
            crate::bus::IO.ai.set_scaled(ch, scaled);
            crate::bus::IO.ai.set_status(ch, status);
        }
        state.pos = (state.pos + 1) % AVG_WINDOW;
        state.filled = (state.filled + 1).min(AVG_WINDOW);
        crate::bus::send_event(crate::bus::IoEvent::AiSampled);
    });
}

/// 选择与原 MCA `Get_Analog()` 一致的校准区间。
/// min/max 任一为 0 都表示尚未完成有效校准，必须回退到默认 605/3016。
fn effective_calibration(min: u16, max: u16) -> (u16, u16) {
    if min != 0 && max != 0 && min != max {
        (min, max)
    } else {
        (ai_calib::SENSOR_MIN, ai_calib::SENSOR_MAX)
    }
}

/// 读取 SENSOR_MIN/MAX 校准值数组 (LOOP12: 返回 [u16; CHANNEL_COUNT] 同步 cfg 切换).
///
/// F4 设备返回 8 个值, F16/F3 设备返回 6 个值, 对齐 HOLD_SENSOR_MIN/MAX_BASE 区长度.
/// LOOP9: 校准值由 calib::run_auto_calibration 写入 holding_buf (2280..2288 / 2288..2296).
fn read_sensor_calib(is_min: bool) -> [u16; CHANNEL_COUNT] {
    use crate::config::regs;
    let base = if is_min {
        regs::HOLD_SENSOR_MIN_BASE
    } else {
        regs::HOLD_SENSOR_MAX_BASE
    };
    // LOOP11: 零拷贝 (避免 AI 高频采样时 storage_read() 触发 Box alloc)
    let mut out = [0u16; CHANNEL_COUNT];
    let idx_base = (base as usize).saturating_sub(regs::HOLD_CFG_BASE as usize);
    crate::bus::storage_state::storage_read_with(|snap| {
        for (ch, value) in out.iter_mut().enumerate() {
            let i = idx_base + ch;
            if i < snap.holding_buf.len() {
                *value = snap.holding_buf[i];
            }
        }
    });
    out
}

/// 线性映射 (对齐 Arduino map / 参考固件): 把 x 从 [in_min, in_max] 映射到 [out_min, out_max].
/// 若 in_min == in_max 返回 out_min (避免除零).
fn map_range(x: u16, in_min: u16, in_max: u16, out_min: u16, out_max: u16) -> u16 {
    if in_min == in_max {
        return out_min;
    }
    // 用 i32 避免 u16 减法下溢
    let x = x as i32;
    let in_min = in_min as i32;
    let in_max = in_max as i32;
    let out_min = out_min as i32;
    let out_max = out_max as i32;
    let num = (x - in_min) as i64 * (out_max - out_min) as i64;
    let den = (in_max - in_min) as i64;
    let v = out_min as i64 + num / den;
    let lower = i64::from(out_min.min(out_max));
    let upper = i64::from(out_min.max(out_max));
    v.clamp(lower, upper) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_calibration_requires_both_values() {
        assert_eq!(effective_calibration(0, 3064), (605, 3016));
        assert_eq!(effective_calibration(605, 0), (605, 3016));
        assert_eq!(effective_calibration(605, 3016), (605, 3016));
    }

    /// 回归测试: 对齐原固件 `gucAnalogData = 4095 - map(raw, max, min, 4095, 0)`。
    #[test]
    fn test_map_range_basic() {
        // 中间 map(raw, max, min, 4095, 0)：raw=max → 4095，raw=min → 0。
        assert_eq!(map_range(3016, 3016, 605, 4095, 0), 4095);
        assert_eq!(map_range(605, 3016, 605, 4095, 0), 0);
        // x 中点 → out 中点
        let mid_in = (605 + 3016) / 2;
        let mid_out = map_range(mid_in, 3016, 605, 4095, 0);
        assert!(
            mid_out > 1900 && mid_out < 2100,
            "mid_out={mid_out} 应接近 2047"
        );
    }

    #[test]
    fn test_map_range_clamp() {
        assert_eq!(map_range(4000, 3016, 605, 4095, 0), 4095);
        assert_eq!(map_range(0, 3016, 605, 4095, 0), 0);
    }

    #[test]
    fn test_map_range_div_zero() {
        // in_min == in_max → 返回 out_min (避免除零)
        assert_eq!(map_range(100, 50, 50, 4095, 0), 4095);
        assert_eq!(map_range(100, 50, 50, 0, 4095), 0);
    }

    #[test]
    fn test_map_range_inverted_output() {
        // 最终输出使用 [min,max] -> [4095,0]，与原固件取反后的结果一致。
        assert_eq!(map_range(3016, 605, 3016, 4095, 0), 0);
        assert_eq!(map_range(605, 605, 3016, 4095, 0), 4095);
    }
}
