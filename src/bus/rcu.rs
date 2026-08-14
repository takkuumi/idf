//! 通用不可变快照容器。
//!
//! 读写锁内只克隆或替换一个 `Arc`，实际读取、业务闭包和旧快照析构均在锁外。
//! 这里不使用 `arc-swap`：其 debt/TLS 回收路径在 ESP-IDF pthread 环境中会破坏
//! 相邻 BSS 状态。这个实现牺牲“理论 lock-free”，换取可验证的内存安全和确定性。

use std::sync::Arc;

use crate::sync::Spin;

/// 多读多写安全的不可变快照容器。
pub struct Rcu<T> {
    value: Spin<Option<Arc<T>>>,
}

impl<T> Rcu<T> {
    /// 构造空容器，第一次读取返回 `None`。
    pub const fn empty() -> Self {
        Self {
            value: Spin::new(None),
        }
    }

    /// 创建已初始化容器。
    pub fn new(initial: T) -> Self {
        Self {
            value: Spin::new(Some(Arc::new(initial))),
        }
    }

    /// 发布新快照。分配在锁前完成，旧快照在锁外释放。
    pub fn write(&self, new_value: T) {
        let new_value = Arc::new(new_value);
        let old_value = {
            let mut current = self.value.lock();
            current.replace(new_value)
        };
        drop(old_value);
    }

    /// 读取当前快照并转移一个安全的所有权句柄。
    pub fn read(&self) -> Option<Arc<T>> {
        self.value.lock().clone()
    }

    /// 克隆当前快照值。
    pub fn read_cloned(&self) -> Option<T>
    where
        T: Clone,
    {
        self.read().map(|value| value.as_ref().clone())
    }

    /// 在锁外执行读取闭包；锁内只增加一次 `Arc` 引用计数。
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
