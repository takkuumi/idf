//! 不可变 Arc 快照：读端不获取 OS Mutex，写端串行发布并回收旧代。
//!
//! 所有 epoch / reader / pointer 操作用 SeqCst 建立共同的总序。读者必须先登记
//! 并验证 epoch，再解引用指针；写者先交换指针、推进 epoch，之后等待旧代登记退出。
//! 计数只覆盖 Arc 引用获取，业务闭包及外部 Arc 持有时间不在回收等待区间内。
//! 新代读者不延长旧代回收。被暂停的旧代读者仍可能延迟写者，不能宣称 wait-free；
//! 写者争用回收时按 RTOS tick 休眠，避免同核抢占造成永久自旋。

use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct Rcu<T> {
    ptr: AtomicPtr<T>,
    epoch: AtomicUsize,
    readers: [AtomicUsize; 2],
    writer: Mutex<()>,
    max_reclaim_wait_us: AtomicU32,
}

impl<T> Rcu<T> {
    pub const fn empty() -> Self {
        Self {
            ptr: AtomicPtr::new(std::ptr::null_mut()),
            epoch: AtomicUsize::new(0),
            readers: [AtomicUsize::new(0), AtomicUsize::new(0)],
            writer: Mutex::new(()),
            max_reclaim_wait_us: AtomicU32::new(0),
        }
    }

    pub fn new(initial: T) -> Self {
        let value = Self::empty();
        value
            .ptr
            .store(Arc::into_raw(Arc::new(initial)) as *mut T, Ordering::SeqCst);
        value
    }

    pub fn write(&self, new_value: T) {
        let new_value = Arc::new(new_value);
        let _writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let old = self
            .ptr
            .swap(Arc::into_raw(new_value) as *mut T, Ordering::SeqCst);
        let old_epoch = self.epoch.fetch_add(1, Ordering::SeqCst);
        let started = std::time::Instant::now();
        while self.readers[old_epoch & 1].load(Ordering::SeqCst) != 0 {
            // yield_now alone need not schedule a lower-priority owner on RTOS.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // writer Mutex already serializes this high-water update. Avoid
        // AtomicU32::fetch_max: Xtensa LLVM 1.90 emits undefined branch labels
        // for this RMW lowering. Readers still use an atomic load.
        let elapsed = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        let maximum = self
            .max_reclaim_wait_us
            .load(Ordering::Relaxed)
            .max(elapsed);
        self.max_reclaim_wait_us.store(maximum, Ordering::Relaxed);
        if !old.is_null() {
            // SAFETY: writer serialization owns exactly this retired Arc. Any
            // reader that could have observed old registered in old_epoch and
            // has now acquired its own Arc (or rejected its stale registration).
            unsafe {
                drop(Arc::from_raw(old));
            }
        }
    }

    pub fn read(&self) -> Option<Arc<T>> {
        loop {
            let epoch = self.epoch.load(Ordering::SeqCst);
            let readers = &self.readers[epoch & 1];
            readers.fetch_add(1, Ordering::SeqCst);
            if epoch != self.epoch.load(Ordering::SeqCst) {
                readers.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let ptr = self.ptr.load(Ordering::SeqCst);
            if ptr.is_null() {
                readers.fetch_sub(1, Ordering::SeqCst);
                return None;
            }
            // SAFETY: validated reader registration prevents the owning writer
            // from releasing this pointer until the increment has completed.
            unsafe {
                Arc::increment_strong_count(ptr);
            }
            readers.fetch_sub(1, Ordering::SeqCst);
            // SAFETY: increment_strong_count created this owned reference.
            return Some(unsafe { Arc::from_raw(ptr) });
        }
    }

    pub fn max_reclaim_wait_us(&self) -> u32 {
        self.max_reclaim_wait_us.load(Ordering::Relaxed)
    }

    pub fn read_cloned(&self) -> Option<T>
    where
        T: Clone,
    {
        self.read().map(|value| value.as_ref().clone())
    }

    pub fn read_with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&T) -> R,
    {
        let value = self.read()?;
        Some(f(value.as_ref()))
    }
}

impl<T> Default for Rcu<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T> Drop for Rcu<T> {
    fn drop(&mut self) {
        let ptr = *self.ptr.get_mut();
        if !ptr.is_null() {
            // SAFETY: exclusive access to the container excludes acquisitions;
            // Arc handles already returned own their independent references.
            unsafe {
                drop(Arc::from_raw(ptr));
            }
        }
    }
}

// SAFETY: immutable T is shared through Arc; mutations only publish owned values.
unsafe impl<T: Send + Sync> Send for Rcu<T> {}
unsafe impl<T: Send + Sync> Sync for Rcu<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;

    #[test]
    fn test_rcu_basic() {
        let rcu = Rcu::new(42u32);
        assert_eq!(*rcu.read().unwrap(), 42);
        rcu.write(100);
        assert_eq!(*rcu.read().unwrap(), 100);
    }

    #[test]
    fn test_rcu_empty() {
        let rcu: Rcu<u32> = Rcu::empty();
        assert!(rcu.read().is_none());
        rcu.write(99);
        assert_eq!(*rcu.read().unwrap(), 99);
    }

    #[test]
    fn test_rcu_read_with_and_cloned() {
        let rcu = Rcu::new(String::from("hello"));
        assert_eq!(rcu.read_with(String::len), Some(5));
        assert_eq!(rcu.read_cloned(), Some("hello".to_string()));
    }

    #[test]
    fn test_reader_keeps_replaced_snapshot_alive() {
        struct Tracked<'a>(&'a AtomicUsize);
        impl Drop for Tracked<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }
        let drops = AtomicUsize::new(0);
        let rcu = Rcu::new(Tracked(&drops));
        let old = rcu.read().unwrap();
        rcu.write(Tracked(&drops));
        assert_eq!(drops.load(Ordering::Acquire), 0);
        drop(old);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        drop(rcu);
        assert_eq!(drops.load(Ordering::Acquire), 2);
    }

    #[test]
    fn test_concurrent_readers_and_writers() {
        let rcu = Arc::new(Rcu::new(0u64));
        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for _ in 0..4 {
            let rcu = Arc::clone(&rcu);
            let stop = Arc::clone(&stop);
            readers.push(thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let value = rcu.read().expect("snapshot initialized");
                    assert!(*value <= 10_000);
                    thread::yield_now();
                    assert!(*value <= 10_000);
                }
            }));
        }
        for value in 1..=10_000 {
            rcu.write(value);
        }
        stop.store(true, Ordering::Release);
        for reader in readers {
            reader.join().expect("reader thread");
        }
        assert_eq!(*rcu.read().unwrap(), 10_000);
    }
    #[test]
    fn test_multiple_writers_reclaim_every_published_value() {
        struct Tracked(Arc<AtomicUsize>);
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let rcu = Arc::new(Rcu::new(Tracked(drops.clone())));
        thread::scope(|scope| {
            for _ in 0..3 {
                scope.spawn(|| {
                    for _ in 0..500 {
                        rcu.write(Tracked(drops.clone()));
                    }
                });
            }
            for _ in 0..3 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        drop(rcu.read());
                    }
                });
            }
        });
        drop(rcu);
        assert_eq!(drops.load(Ordering::SeqCst), 1501);
    }

    #[test]
    fn test_read_closure_can_publish_without_deadlock() {
        let rcu = Rcu::new(1);
        assert_eq!(
            rcu.read_with(|old| {
                rcu.write(2);
                *old
            }),
            Some(1)
        );
        assert_eq!(rcu.read_cloned(), Some(2));
    }
}
