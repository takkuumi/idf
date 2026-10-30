//! Serialize a pending save without acknowledging a concurrent newer mutation.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn commit_pending<E>(
    dirty: &AtomicBool,
    lock: &Mutex<()>,
    force: bool,
    save: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Consume only work that existed before taking the snapshot. A producer
    // marks a later write dirty independently; never clear it after Flash IO.
    let pending = dirty.swap(false, Ordering::AcqRel);
    if !pending && !force {
        return Ok(());
    }
    if let Err(error) = save() {
        dirty.store(true, Ordering::Release);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_save_is_skipped_unless_explicitly_forced() {
        let dirty = AtomicBool::new(false);
        let lock = Mutex::new(());
        let mut saves = 0;
        commit_pending(&dirty, &lock, false, || {
            saves += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(saves, 0);
        commit_pending(&dirty, &lock, true, || {
            saves += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(saves, 1);
    }

    #[test]
    fn test_success_consumes_only_original_pending_work() {
        let dirty = AtomicBool::new(true);
        commit_pending(&dirty, &Mutex::new(()), false, || Ok::<_, ()>(())).unwrap();
        assert!(!dirty.load(Ordering::Acquire));
        dirty.store(true, Ordering::Release);
        commit_pending(&dirty, &Mutex::new(()), false, || {
            dirty.store(true, Ordering::Release);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(
            dirty.load(Ordering::Acquire),
            "a write during Flash IO must remain pending"
        );
    }

    #[test]
    fn test_failure_keeps_retry_pending_even_for_forced_save() {
        for original in [false, true] {
            let dirty = AtomicBool::new(original);
            assert_eq!(
                commit_pending(&dirty, &Mutex::new(()), true, || Err("Flash unavailable")),
                Err("Flash unavailable")
            );
            assert!(dirty.load(Ordering::Acquire));
        }
    }
}
