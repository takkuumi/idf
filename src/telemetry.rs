//! 主循环固定容量时延直方图；只在低频发布时加锁，热路径不分配、不取锁。
use std::sync::Mutex;

const PHASES: usize = 10;
const LIMITS_US: [u32; 16] = [
    50,
    100,
    200,
    500,
    1000,
    2000,
    3000,
    4000,
    5000,
    7500,
    10000,
    20000,
    50000,
    100000,
    1000000,
    u32::MAX,
];

#[derive(Clone, Copy)]
pub struct Histogram {
    bins: [u32; 16],
    count: u32,
    peak_us: u32,
}
impl Histogram {
    pub const fn new() -> Self {
        Self {
            bins: [0; 16],
            count: 0,
            peak_us: 0,
        }
    }
    pub fn record(&mut self, us: u64) {
        let us = us.min(u64::from(u32::MAX)) as u32;
        let index = LIMITS_US
            .iter()
            .position(|&upper| us <= upper)
            .unwrap_or(15);
        self.bins[index] = self.bins[index].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.peak_us = self.peak_us.max(us);
    }
    /// 返回所在桶的上界，绝不标成精确分位数。
    pub fn percentile_upper_us(&self, percent: u32) -> u32 {
        if self.count == 0 {
            return 0;
        }
        let target = u64::from(self.count)
            .saturating_mul(u64::from(percent.min(100)))
            .div_ceil(100)
            .max(1);
        let mut sum = 0u64;
        for (i, &count) in self.bins.iter().enumerate() {
            sum += u64::from(count);
            if sum >= target {
                return LIMITS_US[i];
            }
        }
        u32::MAX
    }
    fn summary(&self) -> Summary {
        Summary {
            count: self.count,
            max_us: self.peak_us,
            p95_upper_us: self.percentile_upper_us(95),
            p99_upper_us: self.percentile_upper_us(99),
        }
    }
}

#[derive(Clone, Copy)]
struct Summary {
    count: u32,
    max_us: u32,
    p95_upper_us: u32,
    p99_upper_us: u32,
}
impl Summary {
    fn json(&self) -> String {
        format!(
            r#"{{"count":{},"max_us":{},"p95_upper_us":{},"p99_upper_us":{}}}"#,
            self.count, self.max_us, self.p95_upper_us, self.p99_upper_us
        )
    }
}
#[derive(Clone, Copy)]
struct Published {
    phases: [Summary; PHASES],
    work: Summary,
}

#[derive(Clone, Copy)]
struct Snapshot {
    phases: [Histogram; PHASES],
    work: Histogram,
}
static LAST: Mutex<Option<Published>> = Mutex::new(None);

pub fn json() -> String {
    let snapshot = LAST.try_lock().ok().and_then(|guard| *guard);
    let Some(snapshot) = snapshot else {
        return "null".into();
    };
    let mut phases = String::new();
    for (i, histogram) in snapshot.phases.iter().enumerate() {
        if i != 0 {
            phases.push(',');
        }
        phases.push_str(&histogram.json());
    }
    format!(
        r#"{{"window":"last_completed_60s_ticks","main_work":{},"phases":[{}]}}"#,
        snapshot.work.json(),
        phases
    )
}

pub struct MainLoopPerf {
    snapshot: Snapshot,
}
impl MainLoopPerf {
    pub const fn new() -> Self {
        Self {
            snapshot: Snapshot {
                phases: [Histogram::new(); PHASES],
                work: Histogram::new(),
            },
        }
    }
    #[inline]
    pub fn measure(&mut self, phase: usize, operation: impl FnOnce()) {
        let started = std::time::Instant::now();
        operation();
        self.record_elapsed(phase, started);
    }
    pub fn record_elapsed(&mut self, phase: usize, started: std::time::Instant) {
        if let Some(histogram) = self.snapshot.phases.get_mut(phase) {
            histogram.record(started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
        }
    }
    pub fn record_work(&mut self, us: u64) {
        self.snapshot.work.record(us);
    }
    pub fn log_and_reset(&mut self) {
        if let Ok(mut last) = LAST.try_lock() {
            *last = Some(Published {
                phases: self.snapshot.phases.map(|h| h.summary()),
                work: self.snapshot.work.summary(),
            });
        }
        let work = self.snapshot.work;
        let p = self.snapshot.phases.map(|h| h.peak_us);
        log::info!(
            "[perf] work_samples={} p95_upper_us={} p99_upper_us={} phase_max_us={:?}",
            work.count,
            work.percentile_upper_us(95),
            work.percentile_upper_us(99),
            p
        );
        self.snapshot = Snapshot {
            phases: [Histogram::new(); PHASES],
            work: Histogram::new(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_histogram_empty_boundaries_and_upper_percentiles() {
        let mut h = Histogram::new();
        assert_eq!(h.percentile_upper_us(99), 0);
        for _ in 0..95 {
            h.record(51);
        }
        for _ in 0..4 {
            h.record(1001);
        }
        h.record(9000);
        assert_eq!(h.percentile_upper_us(95), 100);
        assert_eq!(h.percentile_upper_us(99), 2000);
        assert_eq!(h.peak_us, 9000);
        assert_eq!(h.percentile_upper_us(100), 10000);
    }
}
