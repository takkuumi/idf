//! 可确定性测试的校准时窗；每次 tick 最多采一个样本，不补跑错过的采样。
pub struct CalibrationWindow {
    start_ms: u32,
    last_sample_ms: Option<u32>,
    pub raw_min: u16,
    pub raw_max: u16,
    pub samples: u32,
}

impl CalibrationWindow {
    pub fn new(start_ms: u32) -> Self {
        Self {
            start_ms,
            last_sample_ms: None,
            raw_min: u16::MAX,
            raw_max: 0,
            samples: 0,
        }
    }
    pub fn done(&self, now_ms: u32, window_ms: u32) -> bool {
        now_ms.wrapping_sub(self.start_ms) >= window_ms
    }
    pub fn sample_due(&self, now_ms: u32, interval_ms: u32) -> bool {
        self.last_sample_ms
            .is_none_or(|last| now_ms.wrapping_sub(last) >= interval_ms)
    }
    pub fn observe(&mut self, now_ms: u32, raw: u16) {
        self.raw_min = self.raw_min.min(raw);
        self.raw_max = self.raw_max.max(raw);
        self.samples = self.samples.saturating_add(1);
        self.last_sample_ms = Some(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_calibration_ticks_do_not_block_or_catch_up() {
        let mut window = CalibrationWindow::new(0);
        assert!(window.sample_due(0, 50));
        window.observe(0, 605);
        assert!(!window.sample_due(49, 50));
        assert!(window.sample_due(1000, 50));
        window.observe(1000, 3016);
        assert!(!window.sample_due(1001, 50));
        assert_eq!(window.samples, 2);
        assert_eq!((window.raw_min, window.raw_max), (605, 3016));
        assert!(!window.done(7999, 8000));
        assert!(window.done(8000, 8000));
    }
    #[test]
    fn test_calibration_window_survives_timer_wrap() {
        let mut window = CalibrationWindow::new(u32::MAX - 99);
        window.observe(u32::MAX - 49, 10);
        assert!(window.sample_due(0, 50));
        assert!(!window.done(7899, 8000));
        assert!(window.done(7900, 8000));
    }
}
