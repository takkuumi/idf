//! 通用不可变快照容器。
//!
//! 读取路径使用原子指针 + reader guard，不获取 OS Mutex；写入路径先交换新快照，
//! 再等待已经开始的读者退出后释放旧快照。读者在增加 guard 计数后才加载指针，
//! 因此旧对象在 `Arc::increment_strong_count` 前始终由写者持有，避免 use-after-free。
//!
//! 这是一个适合“读多写少、读闭包短”的 RCU。写者会在读者长期占用快照时让出调度器，
//! 不在主循环中持有大锁；不用于需要实时写入确认的外设事务。

use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

pub struct Rcu<T> {
    ptr: AtomicPtr<T>,
    readers: AtomicUsize,
}

impl<T> Rcu<T> {
    pub const fn empty() -> Self {
        Self {
            ptr: AtomicPtr::new(std::ptr::null_mut()),
            readers: AtomicUsize::new(0),
        }
    }

    pub fn new(initial: T) -> Self {
        Self {
            ptr: AtomicPtr::new(Arc::into_raw(Arc::new(initial)) as *mut T),
            readers: AtomicUsize::new(0),
        }
    }

    /// 发布新快照。旧快照只在所有已经开始的读者退出后释放。
    pub fn write(&self, new_value: T) {
        let new_ptr = Arc::into_raw(Arc::new(new_value)) as *mut T;
        let old_ptr = self.ptr.swap(new_ptr, Ordering::AcqRel);
        while self.readers.load(Ordering::Acquire) != 0 {
            std::thread::yield_now();
        }
        if !old_ptr.is_null() {
            // SAFETY: old_ptr came from Arc::into_raw and no reader can still
            // be between pointer load and Arc::increment_strong_count after the
            // reader counter reached zero.
            unsafe {
                drop(Arc::from_raw(old_ptr));
            }
        }
    }

    /// 读取当前快照并转移一个安全的所有权句柄。
    pub fn read(&self) -> Option<Arc<T>> {
        self.readers.fetch_add(1, Ordering::Acquire);
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            self.readers.fetch_sub(1, Ordering::Release);
            return None;
        }
        // SAFETY: a writer cannot free the pointer while readers is non-zero;
        // the writer that swapped this pointer still owns one Arc reference.
        unsafe {
            Arc::increment_strong_count(ptr);
        }
        self.readers.fetch_sub(1, Ordering::Release);
        // SAFETY: the increment above created the owned Arc reference.
        Some(unsafe { Arc::from_raw(ptr) })
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
        while self.readers.load(Ordering::Acquire) != 0 {
            std::thread::yield_now();
        }
        let ptr = self.ptr.load(Ordering::Acquire);
        if !ptr.is_null() {
            // SAFETY: Drop has exclusive access to self and all readers exited.
            unsafe {
                drop(Arc::from_raw(ptr));
            }
        }
    }
}

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
}
