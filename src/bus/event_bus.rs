//! 事件总线：关键状态与普通采样事件分离。
//!
//! 普通 DI/AI/AO/DO 事件只表达“当前状态需要处理”，使用原子 bit 合并，避免
//! 高频采样把队列塞满。IP、链路和复位等关键状态使用独立单槽，保留最新状态，
//! 不会被普通采样事件覆盖。所有生产者仍为非阻塞路径。

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use crate::sync::Spin;

/// IO 事件类型
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoEvent {
    DiChanged,
    DoChanged,
    AiSampled,
    AoUpdated,
    ResetRequested,
    IpAssigned([u8; 4], [u8; 4], [u8; 4], u32),
    EthernetLink(bool, u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CriticalEvent {
    ResetRequested,
    IpAssigned([u8; 4], [u8; 4], [u8; 4], u32),
    EthernetLink(bool, u32),
}

const FLAG_DI_CHANGED: u8 = 1 << 0;
const FLAG_DO_CHANGED: u8 = 1 << 1;
const FLAG_AI_SAMPLED: u8 = 1 << 2;
const FLAG_AO_UPDATED: u8 = 1 << 3;

static COALESCED_FLAGS: AtomicU8 = AtomicU8::new(0);
static CRITICAL_EVENT: LazyLock<Spin<Option<CriticalEvent>>> = LazyLock::new(|| Spin::new(None));
static CRITICAL_OVERWRITES: AtomicU32 = AtomicU32::new(0);

/// 发送事件：非阻塞；普通事件合并，关键事件保留最新状态。
pub fn send_event(event: IoEvent) {
    match event {
        IoEvent::DiChanged => {
            COALESCED_FLAGS.fetch_or(FLAG_DI_CHANGED, Ordering::AcqRel);
        }
        IoEvent::DoChanged => {
            COALESCED_FLAGS.fetch_or(FLAG_DO_CHANGED, Ordering::AcqRel);
        }
        IoEvent::AiSampled => {
            COALESCED_FLAGS.fetch_or(FLAG_AI_SAMPLED, Ordering::AcqRel);
        }
        IoEvent::AoUpdated => {
            COALESCED_FLAGS.fetch_or(FLAG_AO_UPDATED, Ordering::AcqRel);
        }
        critical => {
            let critical = match critical {
                IoEvent::ResetRequested => CriticalEvent::ResetRequested,
                IoEvent::IpAssigned(ip, mask, gw, event_ms) => {
                    CriticalEvent::IpAssigned(ip, mask, gw, event_ms)
                }
                IoEvent::EthernetLink(up, event_ms) => CriticalEvent::EthernetLink(up, event_ms),
                IoEvent::DiChanged
                | IoEvent::DoChanged
                | IoEvent::AiSampled
                | IoEvent::AoUpdated => unreachable!(),
            };
            let mut slot = CRITICAL_EVENT.lock();
            if slot.replace(critical).is_some() {
                CRITICAL_OVERWRITES.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// 接收关键事件优先，其次消费每类普通事件各一次。
pub fn recv_event() -> Option<IoEvent> {
    if let Some(event) = CRITICAL_EVENT.lock().take() {
        return Some(match event {
            CriticalEvent::ResetRequested => IoEvent::ResetRequested,
            CriticalEvent::IpAssigned(ip, mask, gw, event_ms) => {
                IoEvent::IpAssigned(ip, mask, gw, event_ms)
            }
            CriticalEvent::EthernetLink(up, event_ms) => IoEvent::EthernetLink(up, event_ms),
        });
    }

    let flags = COALESCED_FLAGS.load(Ordering::Acquire);
    for (flag, event) in [
        (FLAG_DI_CHANGED, IoEvent::DiChanged),
        (FLAG_DO_CHANGED, IoEvent::DoChanged),
        (FLAG_AI_SAMPLED, IoEvent::AiSampled),
        (FLAG_AO_UPDATED, IoEvent::AoUpdated),
    ] {
        if flags & flag != 0
            && COALESCED_FLAGS
                .compare_exchange(flags, flags & !flag, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            return Some(event);
        }
    }
    None
}

pub fn has_event() -> bool {
    COALESCED_FLAGS.load(Ordering::Acquire) != 0 || CRITICAL_EVENT.lock().is_some()
}

pub fn event_count() -> usize {
    let flags = COALESCED_FLAGS.load(Ordering::Acquire).count_ones() as usize;
    flags + usize::from(CRITICAL_EVENT.lock().is_some())
}

pub fn critical_overwrite_count() -> u32 {
    CRITICAL_OVERWRITES.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain() {
        while recv_event().is_some() {}
    }

    #[test]
    fn test_event_basic() {
        drain();
        send_event(IoEvent::DiChanged);
        assert_eq!(recv_event(), Some(IoEvent::DiChanged));
    }

    #[test]
    fn test_sample_events_coalesce_without_queue_growth() {
        drain();
        for _ in 0..40 {
            send_event(IoEvent::AiSampled);
        }
        assert_eq!(event_count(), 1);
        assert_eq!(recv_event(), Some(IoEvent::AiSampled));
        assert!(!has_event());
    }

    #[test]
    fn test_critical_event_survives_sample_flood() {
        drain();
        send_event(IoEvent::EthernetLink(false, 11));
        for _ in 0..100 {
            send_event(IoEvent::AiSampled);
            send_event(IoEvent::AoUpdated);
        }
        assert_eq!(recv_event(), Some(IoEvent::EthernetLink(false, 11)));
        assert!(recv_event().is_some());
    }

    #[test]
    fn test_latest_critical_state_replaces_older_state() {
        drain();
        let before = critical_overwrite_count();
        send_event(IoEvent::EthernetLink(false, 10));
        send_event(IoEvent::EthernetLink(true, 20));
        assert_eq!(recv_event(), Some(IoEvent::EthernetLink(true, 20)));
        assert!(critical_overwrite_count() >= before + 1);
    }

    #[test]
    fn test_ip_assignment_remains_distinct_from_sample_events() {
        drain();
        send_event(IoEvent::AiSampled);
        send_event(IoEvent::IpAssigned(
            [192, 168, 1, 10],
            [255, 255, 255, 0],
            [192, 168, 1, 1],
            42,
        ));
        assert!(matches!(recv_event(), Some(IoEvent::IpAssigned(..))));
        assert_eq!(recv_event(), Some(IoEvent::AiSampled));
    }
}
