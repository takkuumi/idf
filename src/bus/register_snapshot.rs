//! Fixed-size runtime register image for one Modbus FC03/FC04 response.
//!
//! Capture under the producer's short scheduler mutex, then encode outside it.
//! A lower-priority producer cannot make the reader exhaust seqlock retries or
//! publish half of a multi-word value. No allocation, UART or Flash in capture.

const CAPACITY: usize = 125;
const VALID_WORDS: usize = CAPACITY.div_ceil(32);

pub(crate) struct RegisterSnapshot {
    start: u16,
    values: [u16; CAPACITY],
    valid: [u32; VALID_WORDS],
}

impl RegisterSnapshot {
    pub(crate) fn empty(start: u16) -> Self {
        Self {
            start,
            values: [0; CAPACITY],
            valid: [0; VALID_WORDS],
        }
    }

    pub(crate) fn capture(
        start: u16,
        count: u16,
        mut read: impl FnMut(u16) -> Option<u16>,
    ) -> Self {
        let mut snapshot = Self::empty(start);
        for offset in 0..usize::from(count).min(CAPACITY) {
            let Some(address) = start.checked_add(offset as u16) else {
                break;
            };
            if let Some(value) = read(address) {
                snapshot.values[offset] = value;
                snapshot.valid[offset / 32] |= 1 << (offset % 32);
            }
        }
        snapshot
    }

    pub(crate) fn get(&self, address: u16) -> Option<u16> {
        let offset = usize::from(address.checked_sub(self.start)?);
        if offset >= CAPACITY || self.valid[offset / 32] & (1 << (offset % 32)) == 0 {
            return None;
        }
        Some(self.values[offset])
    }
}

/// Runtime images occupy two disjoint windows. Unrelated configuration reads
/// must not wait for or depend on a runtime image being updated.
pub(crate) fn overlaps_results(
    start: u16,
    count: u16,
    monitor_count: u16,
    user_start: u16,
    user_count: u16,
) -> bool {
    if count == 0 {
        return false;
    }
    let start = u32::from(start);
    let end = start + u32::from(count);
    start < u32::from(monitor_count)
        || (start < u32::from(user_start) + u32::from(user_count) && end > u32::from(user_start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_preserves_values_and_unconfigured_gaps() {
        let snapshot =
            RegisterSnapshot::capture(4000, 125, |address| (address != 4032).then_some(address));
        assert_eq!(snapshot.get(4000), Some(4000));
        assert_eq!(snapshot.get(4031), Some(4031));
        assert_eq!(snapshot.get(4032), None);
        assert_eq!(snapshot.get(4124), Some(4124));
        assert_eq!(snapshot.get(3999), None);
        assert_eq!(snapshot.get(4125), None);
    }

    #[test]
    fn test_snapshot_handles_zero_capacity_and_address_boundary() {
        let empty = RegisterSnapshot::capture(0, 0, |_| Some(9));
        assert_eq!(empty.get(0), None);
        let last = RegisterSnapshot::capture(u16::MAX, 125, Some);
        assert_eq!(last.get(u16::MAX), Some(u16::MAX));
        assert_eq!(last.get(0), None);
    }

    #[test]
    fn test_runtime_overlap_boundaries() {
        for (start, count, expected) in [
            (0, 0, false),
            (0, 125, true),
            (127, 1, true),
            (128, 125, false),
            (2280, 16, false),
            (3999, 1, false),
            (3999, 2, true),
            (4000, 125, true),
            (4223, 1, true),
            (4224, 1, false),
            (u16::MAX, 125, false),
        ] {
            assert_eq!(
                overlaps_results(start, count, 128, 4000, 224),
                expected,
                "start={start} count={count}"
            );
        }
    }
}
