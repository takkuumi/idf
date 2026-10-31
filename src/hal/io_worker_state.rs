//! 固定容量 IO 命令队列及采样有效性；不依赖 ESP-IDF，供 host 故障测试复用。

pub const OUTPUT_QUEUE_CAPACITY: usize = 32;
pub const INPUT_MAX_AGE_MS: u32 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputCommand {
    pub generation: u32,
    pub bits: u64,
}

/// 只在短调度器互斥区内访问。发送失败时不消费队首；满时显式拒绝新命令。
pub struct OutputQueue {
    commands: [OutputCommand; OUTPUT_QUEUE_CAPACITY],
    head: usize,
    len: usize,
    latest: OutputCommand,
    pub high_water: usize,
    pub rejected: u32,
    pub canceled_after_fault: u32,
}

impl OutputQueue {
    pub const fn new() -> Self {
        Self {
            commands: [OutputCommand {
                generation: 0,
                bits: 0,
            }; OUTPUT_QUEUE_CAPACITY],
            head: 0,
            len: 0,
            latest: OutputCommand {
                generation: 0,
                bits: 0,
            },
            high_water: 0,
            rejected: 0,
            canceled_after_fault: 0,
        }
    }

    pub fn submit(&mut self, bits: u64) -> bool {
        if bits == self.latest.bits {
            return true;
        }
        if self.len == OUTPUT_QUEUE_CAPACITY {
            self.rejected = self.rejected.saturating_add(1);
            return false;
        }
        let command = OutputCommand {
            generation: self.latest.generation.wrapping_add(1),
            bits,
        };
        self.commands[(self.head + self.len) % OUTPUT_QUEUE_CAPACITY] = command;
        self.len += 1;
        self.high_water = self.high_water.max(self.len);
        self.latest = command;
        true
    }

    /// 硬件故障期间只保留最新目标，避免恢复时重放已经过期的 ON/OFF。
    /// 正常状态仍保持 FIFO；此计数向诊断明确报告被取消的边沿。
    pub fn retain_latest_after_fault(&mut self) {
        if self.len > 1 {
            self.canceled_after_fault = self
                .canceled_after_fault
                .saturating_add((self.len - 1) as u32);
            self.commands[0] = self.latest;
            self.head = 0;
            self.len = 1;
        }
    }

    pub fn front(&self) -> Option<OutputCommand> {
        (self.len != 0).then(|| self.commands[self.head])
    }

    pub fn latest(&self) -> OutputCommand {
        self.latest
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn confirm(&mut self, command: OutputCommand) -> bool {
        if self.front() != Some(command) {
            return false;
        }
        self.head = (self.head + 1) % OUTPUT_QUEUE_CAPACITY;
        self.len -= 1;
        true
    }
}

pub fn sample_is_fresh(initialized: bool, now_ms: u32, sampled_ms: u32, failures: u32) -> bool {
    initialized && failures == 0 && now_ms.wrapping_sub(sampled_ms) <= INPUT_MAX_AGE_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_keeps_on_off_edges_and_retries_failed_front() {
        let mut queue = OutputQueue::new();
        assert!(queue.submit(1));
        assert!(queue.submit(0));
        let on = queue.front().unwrap();
        assert_eq!(on.bits, 1);
        assert_eq!(
            queue.front(),
            Some(on),
            "failed hardware attempt must retain ON"
        );
        assert!(queue.confirm(on));
        assert_eq!(queue.front().unwrap().bits, 0);
        assert!(
            !queue.confirm(on),
            "stale confirmation must not consume OFF"
        );
    }

    #[test]
    fn test_full_queue_rejects_without_overwriting_pending_off() {
        let mut queue = OutputQueue::new();
        for i in 0..OUTPUT_QUEUE_CAPACITY {
            assert!(queue.submit((i as u64 + 1) % 2));
        }
        assert!(!queue.submit(1));
        assert_eq!(queue.latest().bits, 0);
        assert_eq!(queue.rejected, 1);
        assert_eq!(queue.high_water, OUTPUT_QUEUE_CAPACITY);
        for i in 0..OUTPUT_QUEUE_CAPACITY {
            let command = queue.front().unwrap();
            assert_eq!(command.bits, (i as u64 + 1) % 2);
            assert!(queue.confirm(command));
        }
        assert_eq!(queue.len(), 0);
        assert!(queue.submit(1));
    }

    #[test]
    fn test_fault_recovery_does_not_replay_superseded_pulse() {
        let mut queue = OutputQueue::new();
        assert!(queue.submit(1));
        assert!(queue.submit(0));
        queue.retain_latest_after_fault();
        assert_eq!(queue.front().unwrap().bits, 0);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.canceled_after_fault, 1);
    }

    #[test]
    fn test_duplicate_commands_do_not_consume_queue_capacity() {
        let mut queue = OutputQueue::new();
        assert!(queue.submit(1));
        for _ in 0..100 {
            assert!(queue.submit(1));
        }
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn test_sample_validity_handles_startup_errors_staleness_and_clock_wrap() {
        assert!(!sample_is_fresh(false, 0, 0, 0));
        assert!(sample_is_fresh(true, 0, 0, 0));
        assert!(sample_is_fresh(true, 250, 0, 0));
        assert!(!sample_is_fresh(true, 251, 0, 0));
        assert!(!sample_is_fresh(true, 1, 0, 1));
        assert!(sample_is_fresh(true, 5, u32::MAX - 4, 0));
    }
}
