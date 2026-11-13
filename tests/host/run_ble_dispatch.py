#!/usr/bin/env python3
"""Exercise the production BLE slot dispatcher under NFC lock contention."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def item(source: str, marker: str) -> str:
    start = source.index(marker)
    opening = source.index('{', start)
    end, depth = opening + 1, 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


HARNESS = r'''
use std::sync::{Mutex, MutexGuard};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};
extern crate self as log;
#[macro_export] macro_rules! debug { ($($args:tt)*) => {{ let _ = format_args!($($args)*); }}; }
struct Spin<T>(Mutex<T>);
impl<T> Spin<T> {
    const fn new(value: T) -> Self { Self(Mutex::new(value)) }
    fn lock(&self) -> MutexGuard<'_,T> { self.0.lock().unwrap() }
    fn try_lock(&self) -> Option<MutexGuard<'_,T>> { self.0.try_lock().ok() }
}
const RX_FREE: u8 = 0;
const RX_READY: u8 = 2;
const RX_PROCESSING: u8 = 3;
struct Slot { state: AtomicU8, frame: Spin<[u8;2]>, conn_id: AtomicU16, epoch: AtomicU32 }
static BINARY_RX_SLOTS: [Slot;1] = [Slot { state: AtomicU8::new(RX_FREE), frame: Spin::new([1,2]),
    conn_id: AtomicU16::new(7), epoch: AtomicU32::new(4) }];
static CONNECTION_EPOCH: AtomicU32 = AtomicU32::new(4);
static PROTOCOL_DISPATCH_LOCK: Spin<()> = Spin::new(());
static CONNECTION_BUFFERS_DIRTY: AtomicBool = AtomicBool::new(false);
static HANDLED: AtomicU32 = AtomicU32::new(0);
struct Queue { entries: Mutex<Vec<u8>>, reject: AtomicBool }
impl Queue {
    fn try_enqueue(&self, value: u8) -> bool {
        if self.reject.load(Ordering::Acquire) { return false; }
        self.entries.lock().unwrap().push(value); true
    }
}
static BINARY_REQUESTS: Queue = Queue { entries: Mutex::new(Vec::new()), reject: AtomicBool::new(false) };
fn try_handle_binary_protocol(frame: &[u8;2], conn_id: u16, session_epoch: u32) -> bool {
    assert_eq!(frame, &[1,2]); assert_eq!(conn_id,7); assert_eq!(session_epoch,4);
    assert!(PROTOCOL_DISPATCH_LOCK.try_lock().is_none());
    HANDLED.fetch_add(1,Ordering::Relaxed); true
}
fn release_binary_rx_slot(index: u8) -> bool {
    let slot = &BINARY_RX_SLOTS[index as usize];
    let Some(_frame) = slot.frame.try_lock() else { return false; };
    slot.state.store(RX_FREE,Ordering::Release); true
}
fn reset() {
    BINARY_RX_SLOTS[0].state.store(RX_READY,Ordering::Release);
    BINARY_RX_SLOTS[0].epoch.store(4,Ordering::Release);
    CONNECTION_EPOCH.store(4,Ordering::Release);
    CONNECTION_BUFFERS_DIRTY.store(false,Ordering::Release);
    BINARY_REQUESTS.reject.store(false,Ordering::Release);
    BINARY_REQUESTS.entries.lock().unwrap().clear();
    HANDLED.store(0,Ordering::Release);
}
#[test] fn test_nfc_lock_busy_requeues_without_waiting_or_dispatching() {
    reset();
    let _nfc = PROTOCOL_DISPATCH_LOCK.lock();
    process_binary_request_slot(0,7);
    assert_eq!(HANDLED.load(Ordering::Acquire),0);
    assert_eq!(BINARY_RX_SLOTS[0].state.load(Ordering::Acquire),RX_READY);
    assert_eq!(*BINARY_REQUESTS.entries.lock().unwrap(),vec![0]);
    assert!(BINARY_RX_SLOTS[0].frame.try_lock().is_some(), "frame lock must be released before retry");
    assert!(!CONNECTION_BUFFERS_DIRTY.load(Ordering::Acquire));
    drop(_nfc);
    process_binary_request_slot(0,7);
    assert_eq!(HANDLED.load(Ordering::Acquire),1);
    assert_eq!(BINARY_RX_SLOTS[0].state.load(Ordering::Acquire),RX_FREE);
}
#[test] fn test_full_retry_queue_requests_existing_cleanup() {
    reset(); BINARY_REQUESTS.reject.store(true,Ordering::Release);
    let _nfc = PROTOCOL_DISPATCH_LOCK.lock();
    process_binary_request_slot(0,7);
    assert!(CONNECTION_BUFFERS_DIRTY.load(Ordering::Acquire));
    assert_eq!(BINARY_RX_SLOTS[0].state.load(Ordering::Acquire),RX_READY);
    assert_eq!(HANDLED.load(Ordering::Acquire),0);
}
#[test] fn test_frame_contention_keeps_request_for_retry() {
    reset(); let _frame = BINARY_RX_SLOTS[0].frame.lock();
    process_binary_request_slot(0,7);
    assert_eq!(*BINARY_REQUESTS.entries.lock().unwrap(),vec![0]);
    assert_eq!(BINARY_RX_SLOTS[0].state.load(Ordering::Acquire),RX_READY);
    assert_eq!(HANDLED.load(Ordering::Acquire),0);
}
#[test] fn test_old_connection_epoch_is_dropped_even_while_nfc_busy() {
    reset(); CONNECTION_EPOCH.store(5,Ordering::Release);
    let _nfc = PROTOCOL_DISPATCH_LOCK.lock();
    process_binary_request_slot(0,7);
    assert_eq!(BINARY_RX_SLOTS[0].state.load(Ordering::Acquire),RX_FREE);
    assert_eq!(HANDLED.load(Ordering::Acquire),0);
    assert!(BINARY_REQUESTS.entries.lock().unwrap().is_empty());
}
#[test] fn test_invalid_or_not_ready_slot_is_ignored() {
    reset(); BINARY_RX_SLOTS[0].state.store(RX_FREE,Ordering::Release);
    process_binary_request_slot(0,7); process_binary_request_slot(255,7);
    assert_eq!(HANDLED.load(Ordering::Acquire),0);
    assert!(BINARY_REQUESTS.entries.lock().unwrap().is_empty());
}
'''


def main() -> None:
    production = (ROOT / 'src/ble_at/mod.rs').read_text()
    source = HARNESS + '\n' + item(production, 'fn process_binary_request_slot(')
    with tempfile.TemporaryDirectory(prefix='imc-ble-dispatch-') as directory:
        root = Path(directory)
        (root / 'tests.rs').write_text(source)
        subprocess.run(['rustc', '+stable', '--edition=2024', '--test', str(root / 'tests.rs'), '-o', str(root / 'tests')], check=True)
        subprocess.run([str(root / 'tests'), '--test-threads=1'], check=True, timeout=15)


if __name__ == '__main__':
    main()
