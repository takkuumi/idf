//! Bounded advertising renewal, independent of the last GAP success flag.
//!
//! Legacy BLE has no public read-advertising-enable command. A successful
//! callback is therefore a lease, not proof that advertising stays on forever.
//! Renew only while disconnected; the SDK restart has a brief on-air gap.

pub(super) const RENEW_INTERVAL_MS: u32 = 60_000;
pub(super) const RETRY_INTERVAL_MS: u32 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Name,
    Configure,
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    None,
    Retry,
    Renew,
}

pub(super) struct AdvertisingHealth {
    active: bool,
    pending: Option<Operation>,
    last_attempt_ms: u32,
    last_confirmed_ms: u32,
    retry_backoff: bool,
}

impl AdvertisingHealth {
    pub(super) const fn new() -> Self {
        Self {
            active: false,
            pending: None,
            last_attempt_ms: 0,
            last_confirmed_ms: 0,
            retry_backoff: false,
        }
    }

    pub(super) fn active(&self) -> bool {
        self.active
    }

    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn can_update_name(&self, now_ms: u32) -> bool {
        !self.busy()
            && (!self.retry_backoff
                || now_ms.wrapping_sub(self.last_attempt_ms) >= RETRY_INTERVAL_MS)
    }

    /// Reserve before calling an asynchronous SDK API. Never hold the state
    /// lock during the SDK call: a callback may run before that call returns.
    pub(super) fn begin(&mut self, operation: Operation, now_ms: u32) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.pending = Some(operation);
        self.last_attempt_ms = now_ms;
        self.retry_backoff = false;
        true
    }

    /// Ignore callbacks from a phase cancelled by connection or timeout.
    /// The SDK does not supply a request ID for these legacy GAP events.
    pub(super) fn complete(&mut self, operation: Operation, success: bool, now_ms: u32) -> bool {
        if self.pending != Some(operation) {
            return false;
        }
        self.pending = None;
        self.last_attempt_ms = now_ms;
        self.active = operation == Operation::Start && success;
        self.retry_backoff = !success;
        if self.active {
            self.last_confirmed_ms = now_ms;
        }
        true
    }

    pub(super) fn connected(&mut self, now_ms: u32) {
        self.active = false;
        self.pending = None;
        self.last_attempt_ms = now_ms;
        self.retry_backoff = false;
    }

    pub(super) fn recovery(
        &mut self,
        now_ms: u32,
        service_ready: bool,
        connected: bool,
        rename_pending: bool,
    ) -> Recovery {
        if !service_ready || connected {
            return Recovery::None;
        }
        if self.pending.is_some() {
            if now_ms.wrapping_sub(self.last_attempt_ms) < RETRY_INTERVAL_MS {
                return Recovery::None;
            }
            // A lost configure/start/stop callback must not freeze recovery.
            self.pending = None;
            self.active = false;
            return Recovery::Retry;
        }
        // Name changes get the next idle phase; do not overlap maintenance.
        if rename_pending {
            return Recovery::None;
        }
        if !self.active {
            if now_ms.wrapping_sub(self.last_attempt_ms) >= RETRY_INTERVAL_MS {
                return Recovery::Retry;
            }
        } else if now_ms.wrapping_sub(self.last_confirmed_ms) >= RENEW_INTERVAL_MS {
            return Recovery::Renew;
        }
        Recovery::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(at: u32) -> AdvertisingHealth {
        let mut health = AdvertisingHealth::new();
        assert!(health.begin(Operation::Start, at));
        assert!(health.complete(Operation::Start, true, at));
        health
    }

    #[test]
    fn test_stale_active_flag_is_renewed_at_sixty_seconds() {
        let mut health = running(1_600);
        assert_eq!(health.recovery(61_599, true, false, false), Recovery::None);
        // Simulate a silent controller stop: no callback changes active=true.
        assert!(health.active());
        assert_eq!(health.recovery(61_600, true, false, false), Recovery::Renew);
        assert!(health.begin(Operation::Start, 61_600));
        assert_eq!(health.recovery(61_610, true, false, false), Recovery::None);
        assert!(health.complete(Operation::Start, true, 61_620));
        assert_eq!(health.recovery(121_619, true, false, false), Recovery::None);
    }

    #[test]
    fn test_service_not_ready_or_connected_never_renews() {
        let mut health = running(100);
        assert_eq!(health.recovery(90_000, false, false, false), Recovery::None);
        assert_eq!(health.recovery(90_000, true, true, false), Recovery::None);
        health.connected(90_000);
        assert!(!health.active());
        assert!(!health.complete(Operation::Start, true, 90_001));
    }

    #[test]
    fn test_each_missing_callback_times_out_without_overlapping_requests() {
        for operation in [Operation::Configure, Operation::Start, Operation::Stop] {
            let mut health = running(100);
            assert!(health.begin(operation, 200));
            assert!(!health.begin(Operation::Start, 201));
            assert_eq!(health.recovery(5_199, true, false, false), Recovery::None);
            assert_eq!(health.recovery(5_200, true, false, false), Recovery::Retry);
            assert!(!health.active());
            assert!(!health.busy());
            assert!(!health.complete(operation, true, 5_201));
        }
    }

    #[test]
    fn test_failure_keeps_five_second_backoff() {
        let mut health = running(100);
        assert!(health.begin(Operation::Start, 60_100));
        assert!(health.complete(Operation::Start, false, 60_120));
        assert_eq!(health.recovery(65_119, true, false, false), Recovery::None);
        assert_eq!(health.recovery(65_120, true, false, false), Recovery::Retry);
    }

    #[test]
    fn test_name_updates_wait_for_pending_phases_and_error_backoff() {
        let mut health = running(100);
        assert!(health.can_update_name(101));
        assert!(health.begin(Operation::Name, 101));
        assert!(!health.can_update_name(102));
        assert!(!health.complete(Operation::Configure, true, 102));
        assert!(health.complete(Operation::Name, false, 103));
        assert!(!health.can_update_name(5_102));
        assert!(health.can_update_name(5_103));
    }

    #[test]
    fn test_rename_has_priority_but_cannot_hide_a_lost_callback() {
        let mut health = running(100);
        assert_eq!(health.recovery(60_100, true, false, true), Recovery::None);
        assert!(health.begin(Operation::Stop, 60_100));
        assert_eq!(health.recovery(65_099, true, false, true), Recovery::None);
        assert_eq!(health.recovery(65_100, true, false, true), Recovery::Retry);
    }

    #[test]
    fn test_connect_cancels_every_pending_phase() {
        for operation in [Operation::Configure, Operation::Start, Operation::Stop] {
            let mut health = running(100);
            assert!(health.begin(operation, 200));
            health.connected(201);
            assert!(!health.complete(operation, true, 202));
            assert_eq!(health.recovery(90_000, true, true, true), Recovery::None);
            assert!(health.begin(Operation::Configure, 90_001));
        }
    }

    #[test]
    fn test_timers_survive_millisecond_wrap() {
        let mut health = running(u32::MAX - 1_000);
        assert_eq!(health.recovery(58_998, true, false, false), Recovery::None);
        assert_eq!(health.recovery(58_999, true, false, false), Recovery::Renew);
        assert!(health.begin(Operation::Configure, u32::MAX - 100));
        assert_eq!(health.recovery(4_898, true, false, false), Recovery::None);
        assert_eq!(health.recovery(4_899, true, false, false), Recovery::Retry);
    }

    #[test]
    fn test_only_successful_start_confirms_advertising() {
        let mut health = AdvertisingHealth::new();
        for operation in [Operation::Configure, Operation::Stop] {
            assert!(health.begin(operation, 100));
            assert!(health.complete(operation, true, 101));
            assert!(!health.active());
        }
        assert!(health.begin(Operation::Start, 200));
        assert!(!health.complete(Operation::Configure, true, 201));
        assert!(health.busy());
        assert!(health.complete(Operation::Start, true, 202));
        assert!(health.active());
    }
}
