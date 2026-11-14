//! Pending complete BLE RX slots. Publishing must never fail due to a brief
//! consumer lock: ATT has already acknowledged the bytes at this point.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

pub(super) struct RequestSlots<const N: usize> {
    pending: AtomicU8,
    next_ticket: AtomicU32,
    tickets: [AtomicU32; N],
}

impl<const N: usize> RequestSlots<N> {
    pub(super) const fn new() -> Self {
        assert!(N > 0 && N <= 8);
        Self {
            pending: AtomicU8::new(0),
            next_ticket: AtomicU32::new(0),
            tickets: [const { AtomicU32::new(0) }; N],
        }
    }

    /// Only the GATT producer publishes new completed slots. A slot cannot be
    /// reused until the consumer has released its separate RX state to FREE.
    pub(super) fn enqueue_new(&self, index: u8) -> bool {
        let Some(ticket) = self.tickets.get(usize::from(index)) else {
            return false;
        };
        ticket.store(
            self.next_ticket.fetch_add(1, Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.try_enqueue(index)
    }

    /// Requeue a busy slot without changing its original FIFO ticket.
    pub(super) fn try_enqueue(&self, index: u8) -> bool {
        if usize::from(index) >= N {
            return false;
        }
        self.pending.fetch_or(1u8 << index, Ordering::Release);
        true
    }

    /// The main task is the single consumer. A snapshot costs at most N reads;
    /// producer publication never waits for the consumer or loses a slot.
    pub(super) fn dequeue(&self) -> Option<u8> {
        let pending = self.pending.load(Ordering::Acquire);
        let mut oldest: Option<(u8, u32)> = None;
        for index in 0..N {
            if pending & (1 << index) == 0 {
                continue;
            }
            let ticket = self.tickets[index].load(Ordering::Relaxed);
            if oldest.is_none_or(|(_, previous)| (ticket.wrapping_sub(previous) as i32) < 0) {
                oldest = Some((index as u8, ticket));
            }
        }
        let (index, _) = oldest?;
        self.pending.fetch_and(!(1u8 << index), Ordering::AcqRel);
        Some(index)
    }

    pub(super) fn len(&self) -> usize {
        self.pending.load(Ordering::Acquire).count_ones() as usize
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pending.load(Ordering::Acquire) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complete_slots_keep_fifo_order_across_retry() {
        let queue = RequestSlots::<4>::new();
        assert!(queue.enqueue_new(3));
        assert!(queue.enqueue_new(0));
        assert_eq!(queue.dequeue(), Some(3));
        assert!(queue.try_enqueue(3));
        assert_eq!(queue.dequeue(), Some(3));
        assert_eq!(queue.dequeue(), Some(0));
        assert!(queue.is_empty());
    }

    #[test]
    fn test_ticket_rollover_keeps_order() {
        let queue = RequestSlots::<4>::new();
        queue.next_ticket.store(u32::MAX - 1, Ordering::Relaxed);
        for index in [2, 1, 3, 0] {
            assert!(queue.enqueue_new(index));
        }
        for index in [2, 1, 3, 0] {
            assert_eq!(queue.dequeue(), Some(index));
        }
    }

    #[test]
    fn test_duplicate_retry_coalesces_and_invalid_slot_is_rejected() {
        let queue = RequestSlots::<4>::new();
        assert!(queue.enqueue_new(1));
        assert!(queue.try_enqueue(1));
        assert_eq!(queue.len(), 1);
        assert!(!queue.enqueue_new(4));
        assert!(!queue.try_enqueue(u8::MAX));
        assert_eq!(queue.dequeue(), Some(1));
        assert_eq!(queue.dequeue(), None);
    }

    #[test]
    fn test_concurrent_producer_consumer_never_drops_completed_slots() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let queue = Arc::new(RequestSlots::<4>::new());
        let ready = Arc::new([const { AtomicBool::new(false) }; 4]);
        let writer_queue = Arc::clone(&queue);
        let writer_ready = Arc::clone(&ready);
        let writer = std::thread::spawn(move || {
            for item in 0..20_000 {
                let index = item % 4;
                while writer_ready[index].load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                writer_ready[index].store(true, Ordering::Release);
                assert!(writer_queue.enqueue_new(index as u8));
            }
        });
        for item in 0..20_000 {
            let index = loop {
                if let Some(index) = queue.dequeue() {
                    break usize::from(index);
                }
                std::thread::yield_now();
            };
            assert_eq!(index, item % 4, "completed frame order at {item}");
            assert!(ready[index].swap(false, Ordering::AcqRel));
        }
        writer.join().unwrap();
        assert!(queue.is_empty());
    }
}
