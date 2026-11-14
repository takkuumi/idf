//! Bounded per-UART telemetry. Each UART has exactly one owning RTU worker.
//! The writer never allocates or locks; HTTP readers make at most three attempts.
use std::sync::atomic::{AtomicU32, Ordering};

const FIELDS: usize = 19;
struct Trace {
    sequence: AtomicU32,
    values: [AtomicU32; FIELDS],
}
impl Trace {
    const fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            values: [const { AtomicU32::new(0) }; FIELDS],
        }
    }
    fn update(&self, change: impl FnOnce(&Self)) {
        // Only the driver-owning worker writes this UART's record.
        self.sequence.fetch_add(1, Ordering::AcqRel);
        change(self);
        self.sequence.fetch_add(1, Ordering::Release);
    }
    fn get(&self, index: usize) -> u32 {
        self.values[index].load(Ordering::Relaxed)
    }
    fn set(&self, index: usize, value: u32) {
        self.values[index].store(value, Ordering::Relaxed);
    }
    fn increment(&self, index: usize) {
        self.set(index, self.get(index).saturating_add(1));
    }
    fn peak(&self, index: usize, value: u32) {
        self.set(index, self.get(index).max(value));
    }
    fn snapshot(&self) -> Option<[u32; FIELDS]> {
        for _ in 0..3 {
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue;
            }
            let values = std::array::from_fn(|i| self.get(i));
            std::sync::atomic::fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return Some(values);
            }
        }
        None
    }
    fn rx(&self, length: usize, us: u32) {
        self.update(|s| {
            s.increment(0);
            s.set(1, length as u32);
            s.set(2, us);
            s.peak(3, us);
        });
    }
    fn tx(&self, length: usize, tail: u16, us: u32, ok: bool) {
        self.update(|s| {
            s.increment(4);
            s.set(5, length as u32);
            s.set(6, us);
            s.peak(7, us);
            if !ok {
                s.increment(8);
            }
            s.set(9, u32::from(tail));
        });
    }
    fn service(&self, build_us: u32, total_us: u32) {
        self.update(|s| {
            s.set(10, build_us);
            s.set(11, total_us);
            s.peak(14, total_us);
            s.increment(16);
        });
    }
}
static PORTS: [Trace; 3] = [const { Trace::new() }; 3];
fn micros(value: std::time::Duration) -> u32 {
    value.as_micros().min(u128::from(u32::MAX)) as u32
}
pub(crate) fn opened(uart: u8, baud: u32, threshold: u32, tx_bytes: u32) {
    if let Some(t) = PORTS.get(usize::from(uart)) {
        t.update(|s| {
            s.set(12, baud);
            s.set(13, 1);
            s.increment(15);
            s.set(17, threshold);
            s.set(18, tx_bytes);
        });
    }
}
pub(crate) fn closed(uart: u8) {
    if let Some(t) = PORTS.get(usize::from(uart)) {
        t.update(|s| s.set(13, 0));
    }
}
pub(crate) fn rx(uart: u8, length: usize, elapsed: std::time::Duration) {
    if let Some(t) = PORTS.get(usize::from(uart)) {
        t.rx(length, micros(elapsed));
    }
}
pub(crate) fn tx(uart: u8, bytes: &[u8], elapsed: std::time::Duration, ok: bool) {
    let tail = if bytes.len() >= 2 {
        u16::from_le_bytes([bytes[bytes.len() - 2], bytes[bytes.len() - 1]])
    } else {
        0
    };
    if let Some(t) = PORTS.get(usize::from(uart)) {
        t.tx(bytes.len(), tail, micros(elapsed), ok);
    }
}
pub(crate) fn service(uart: u8, build: std::time::Duration, total: std::time::Duration) {
    if let Some(t) = PORTS.get(usize::from(uart)) {
        t.service(micros(build), micros(total));
    }
}
pub fn json() -> String {
    use std::fmt::Write;
    let mut out = String::from("[");
    for (uart, trace) in PORTS.iter().enumerate() {
        if uart != 0 {
            out.push(',');
        }
        if let Some(v) = trace.snapshot() {
            let _ = write!(
                out,
                "{{\"uart\":{},\"rx_frames\":{},\"last_rx_bytes\":{},\"last_rx_assembly_us\":{},\"max_rx_assembly_us\":{},\"tx_frames\":{},\"last_tx_bytes\":{},\"last_tx_us\":{},\"max_tx_us\":{},\"tx_errors\":{},\"last_tx_tail16\":{},\"last_build_us\":{},\"last_service_us\":{},\"driver_baud\":{},\"driver_open\":{},\"max_service_us\":{},\"driver_opens\":{},\"service_frames\":{}}}",
                uart,
                v[0],
                v[1],
                v[2],
                v[3],
                v[4],
                v[5],
                v[6],
                v[7],
                v[8],
                v[9],
                v[10],
                v[11],
                v[12],
                v[13] != 0,
                v[14],
                v[15],
                v[16]
            );
        } else {
            out.push_str("null");
        }
    }
    out.push(']');
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_counts_peaks_and_errors_do_not_change_transport() {
        let t = Trace::new();
        t.rx(8, 5000);
        t.tx(245, 0x5dda, 260000, true);
        t.service(120, 260120);
        t.tx(9, 0, 10000, false);
        let v = t.snapshot().unwrap();
        assert_eq!(&v[..10], &[1, 8, 5000, 5000, 2, 9, 10000, 260000, 1, 0]);
        assert_eq!(v[11], 260120);
        assert_eq!(v[16], 1);
    }
    #[test]
    fn test_snapshot_remains_coherent_under_writer() {
        let t = std::sync::Arc::new(Trace::new());
        let writer = std::sync::Arc::clone(&t);
        let task = std::thread::spawn(move || {
            for n in 1..=20000 {
                writer.tx(n % 256, n as u16, n as u32, true);
            }
        });
        for _ in 0..20000 {
            if let Some(v) = t.snapshot() {
                assert_eq!(v[5], v[4] % 256);
            }
        }
        task.join().unwrap();
        assert_eq!(t.snapshot().unwrap()[4], 20000);
    }
    #[test]
    fn test_snapshot_retries_are_bounded_and_sequence_wraps() {
        let t = Trace::new();
        t.sequence.store(1, Ordering::Relaxed);
        assert!(t.snapshot().is_none());
        t.sequence.store(u32::MAX - 1, Ordering::Relaxed);
        t.rx(8, 1);
        assert_eq!(t.sequence.load(Ordering::Relaxed), 0);
        assert_eq!(t.snapshot().unwrap()[0], 1);
    }
}
