//! AI normalisation and sample filter. Values are ADC counts (0..4095), not mA.
use crate::config::ai_calib;

const AVG_WINDOW: usize = 8;

pub struct MovingAverage<const N: usize> {
    buf: [[u16; AVG_WINDOW]; N],
    pos: usize,
    filled: usize,
}

impl<const N: usize> MovingAverage<N> {
    pub const fn new() -> Self {
        Self {
            buf: [[0; AVG_WINDOW]; N],
            pos: 0,
            filled: 0,
        }
    }

    pub fn push(&mut self, raw: [u16; N]) -> [u16; N] {
        // The current sample has already entered the sum; include it in the divisor.
        let count = (self.filled + 1).min(AVG_WINDOW);
        let result = std::array::from_fn(|ch| {
            self.buf[ch][self.pos] = raw[ch];
            let sum: u32 = self.buf[ch].iter().map(|&v| u32::from(v)).sum();
            (sum / count as u32) as u16
        });
        self.pos = (self.pos + 1) % AVG_WINDOW;
        self.filled = count;
        result
    }
}

/// Zero means this endpoint is absent. Keep a recorded endpoint and use the
/// default only for the missing one. Reject reversed/out-of-ADC-range pairs.
/// This neither infers zero from the current input nor widens the noise deadband.
pub fn effective_calibration(min: u16, max: u16) -> (u16, u16) {
    let min = if min == 0 { ai_calib::SENSOR_MIN } else { min };
    let max = if max == 0 { ai_calib::SENSOR_MAX } else { max };
    if min < max && max <= ai_calib::ADC_MAX as u16 {
        (min, max)
    } else {
        (ai_calib::SENSOR_MIN, ai_calib::SENSOR_MAX)
    }
}

/// Whether the stored endpoints contain enough information to publish an AI
/// engineering value. A completely empty pair is deliberately invalid: the
/// default reference span is only a conversion fallback for diagnostics, not
/// evidence that an unconnected input carries a real signal.
pub fn calibration_is_usable(min: u16, max: u16) -> bool {
    // `max` is the raw zero-current endpoint in this inverted transfer. A
    // span-only calibration cannot establish what zero input looks like and
    // would turn ADC offset into a plausible nonzero process value.
    if max == 0 {
        return false;
    }
    let (effective_min, effective_max) = effective_calibration(min, max);
    effective_min < effective_max
        && effective_max <= ai_calib::ADC_MAX as u16
        && (min == 0 || min == effective_min)
        && (max == 0 || max == effective_max)
}

/// Return a converted ADC value only when at least one valid calibration
/// endpoint exists. `None` means the raw ADC remains diagnostic-only.
pub fn calibrated_scaled_value(raw: u16, min: u16, max: u16) -> Option<u16> {
    if !calibration_is_usable(min, max) {
        return None;
    }
    let (min, max) = effective_calibration(min, max);
    Some(legacy_scaled_value(raw, min, max))
}

/// 对齐旧 MCA `Get_Analog()` 的最终输出：先把 ADC 值映射到 0..4095，
/// 然后清除小于 20 的零点噪声。映射直接写成等价的正向表达式，
/// 避免旧实现 `4095 - map(raw, max, min, 4095, 0)` 的双重反向造成误读。
pub fn legacy_scaled_value(raw: u16, cal_min: u16, cal_max: u16) -> u16 {
    let scaled = map_range(raw, cal_min, cal_max, 4095, 0);
    if scaled < 20 { 0 } else { scaled }
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
    fn test_partial_calibration_keeps_available_endpoint() {
        assert_eq!(effective_calibration(0, 2942), (605, 2942));
        assert_eq!(effective_calibration(600, 0), (600, 3016));
        assert_eq!(effective_calibration(600, 2942), (600, 2942));
        assert_eq!(effective_calibration(0, 0), (605, 3016));
        // The recorded zero endpoint must actually be used, including small signals.
        assert_eq!(legacy_scaled_value(2942, 605, 2942), 0);
        assert!(legacy_scaled_value(2920, 605, 2942) > 20);
    }

    #[test]
    fn test_invalid_calibration_falls_back_safely() {
        for (min, max) in [
            (3016, 605),
            (605, 605),
            (4095, 0),
            (0, 600),
            (0, 4096),
            (65535, 65535),
        ] {
            assert_eq!(
                effective_calibration(min, max),
                (605, 3016),
                "invalid pair {min}/{max}"
            );
        }
    }

    #[test]
    fn test_empty_or_invalid_calibration_is_not_published_as_signal() {
        assert!(!calibration_is_usable(0, 0));
        assert!(calibration_is_usable(0, 2942));
        assert!(!calibration_is_usable(605, 0));
        assert!(calibration_is_usable(605, 3016));
        assert!(!calibration_is_usable(3016, 605));
        assert!(!calibration_is_usable(605, 605));
        assert!(!calibration_is_usable(4095, 0));
        assert_eq!(calibrated_scaled_value(2897, 0, 0), None);
        assert_eq!(calibrated_scaled_value(2897, 605, 0), None);
        // One stored endpoint remains usable and is preserved, as in the 3.2.0 fix.
        assert_eq!(calibrated_scaled_value(2942, 0, 2942), Some(0));
        assert!(calibrated_scaled_value(2897, 0, 2942).unwrap() > 0);
    }

    #[test]
    fn test_average_startup_and_steady_state_for_four_and_eight_channels() {
        fn exercise<const N: usize>() {
            let mut average = MovingAverage::<N>::new();
            for _ in 0..24 {
                assert_eq!(average.push([2942; N]), [2942; N]);
            }
            for n in 1..=8 {
                assert_eq!(average.push([0; N]), [(2942 * (8 - n) / 8) as u16; N]);
            }
            assert_eq!(average.push([0; N]), [0; N]);
        }
        exercise::<4>();
        exercise::<8>();
    }

    #[test]
    fn test_average_includes_current_sample_and_keeps_channels_independent() {
        let mut average = MovingAverage::<4>::new();
        for n in 1..=8u16 {
            assert_eq!(average.push([n * 2, 4095, 0, 100]), [n + 1, 4095, 0, 100]);
        }
    }

    #[test]
    fn test_transfer_is_bounded_and_monotonic_for_every_adc_code() {
        for (min, max) in [(0, 0), (0, 2942), (600, 0), (600, 2942)] {
            let (lo, hi) = effective_calibration(min, max);
            let mut previous = 4095;
            for raw in 0..=4095 {
                let value = legacy_scaled_value(raw, lo, hi);
                assert!(
                    value <= previous && value <= 4095,
                    "raw={raw}, endpoints={lo}/{hi}"
                );
                previous = value;
            }
        }
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

    #[test]
    fn test_normalisation_endpoints_and_existing_deadband() {
        assert_eq!(legacy_scaled_value(605, 605, 3016), 4095);
        assert_eq!(legacy_scaled_value(3016, 605, 3016), 0);
        // 与旧固件的 gucAnalogData < 20 清零行为一致。
        assert_eq!(legacy_scaled_value(3005, 605, 3016), 0);
        // Direct reverse mapping rounds this point to 21 (Arduino double
        // reverse differs by one LSB); retain the existing IMC transfer.
        assert_eq!(legacy_scaled_value(3004, 605, 3016), 21);
        // 输入越界时沿旧固件映射结果饱和到 0..4095。
        assert_eq!(legacy_scaled_value(0, 605, 3016), 4095);
        assert_eq!(legacy_scaled_value(4095, 605, 3016), 0);
    }
}
