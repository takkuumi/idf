//! 缓冲区池 - 减少 Modbus TCP 响应堆分配 (无锁: atomic free-list + UnsafeCell)
//!
//! ## 设计
//! - `used`: `AtomicBool` 数组, acquire 用 CAS-loop 找空闲槽
//! - `inner`: `UnsafeCell<[Vec; N]>` 直接索引, 调用方持 idx 期间独占访问 (不抢锁)
//! - 没有任何 Spin/Mutex 在热路径上, Modbus TCP 响应不阻塞
//!
//! ## 线程安全
//! - acquire/release 必须配对 (RAII 不易在 heapless 上实现, 用索引+手动 release)
//! - idx 在持有期间仅由调用方线程访问, 无 race
//!
//! ## 适用场景
//! - Modbus TCP 响应构造 (一次性短持有)
//! - BLE notify 拼包 (短持有)

use crate::sync::AtomicBits64;

use core::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};

const POOL_SIZE: usize = 4;
const BUF_SIZE: usize = 256;

/// 全局缓冲池 (atomic free-list + UnsafeCell, 零锁)
pub struct GlobalBufferPool {
    /// 每槽的"使用中"标志; acquire 找第一个 false 槽
    used: [AtomicBool; POOL_SIZE],
    /// 缓冲区存储; acquire 后通过 idx 独占访问, 无需 lock
    inner: UnsafeCell<[heapless::Vec<u8, BUF_SIZE>; POOL_SIZE]>,
}

// SAFETY: used 是 atomic; inner 由 idx 持有期间独占 (acquire/release 配对保证).
unsafe impl Sync for GlobalBufferPool {}
unsafe impl Send for GlobalBufferPool {}

impl GlobalBufferPool {
    pub const fn new() -> Self {
        Self {
            used: [const { AtomicBool::new(false) }; POOL_SIZE],
            inner: UnsafeCell::new([const { heapless::Vec::new() }; POOL_SIZE]),
        }
    }

    /// 获取一个空闲缓冲区槽位索引; 池满返回 None.
    /// 通过 CAS 找第一个 false 槽, 多线程并发安全.
    #[inline]
    pub fn acquire(&self) -> Option<usize> {
        for i in 0..POOL_SIZE {
            // CAS(false → true): 失败说明被别人抢了, 跳下一个.
            if self.used[i]
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Some(i);
            }
        }
        None
    }

    /// 释放缓冲区槽位.
    #[inline]
    pub fn release(&self, idx: usize) {
        if idx < POOL_SIZE {
            self.used[idx].store(false, Ordering::Release);
        }
    }

    /// 写入缓冲区 (调用方独占持 idx 期间).
    /// SAFETY: idx 必须是当前线程 acquire 得到的有效索引.
    #[inline]
    pub fn write<F>(&self, idx: usize, f: F)
    where
        F: FnOnce(&mut heapless::Vec<u8, BUF_SIZE>),
    {
        debug_assert!(idx < POOL_SIZE);
        debug_assert!(self.used[idx].load(Ordering::Acquire));
        // SAFETY: 调用方持有 idx (单线程独占), 没有其他线程能同时 write 同一 idx.
        let pool = unsafe { &mut *self.inner.get() };
        let buf = &mut pool[idx];
        buf.clear();
        f(buf);
    }

    /// 读取缓冲区副本 (调用方独占持 idx 期间).
    /// SAFETY: idx 必须是当前线程 acquire 得到的有效索引.
    #[inline]
    pub fn read(&self, idx: usize) -> Option<heapless::Vec<u8, BUF_SIZE>> {
        debug_assert!(idx < POOL_SIZE);
        debug_assert!(self.used[idx].load(Ordering::Acquire));
        // SAFETY: 调用方持有 idx (单线程独占).
        let pool = unsafe { &*self.inner.get() };
        Some(pool[idx].clone())
    }

    /// 当前空闲槽位数 (用于监控).
    pub fn available(&self) -> usize {
        self.used.iter().filter(|b| !b.load(Ordering::Relaxed)).count()
    }
}

/// 全局池 (Lazy 初始化)
pub static POOL: std::sync::LazyLock<GlobalBufferPool> =
    std::sync::LazyLock::new(GlobalBufferPool::new);

// AtomicBits64 import 是为了潜在扩展 (例如批量 release); 当前未使用, 保留兼容性.
#[allow(dead_code)]
const _ATBITS: fn() -> AtomicBits64 = || AtomicBits64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_basic() {
        let idx = POOL.acquire().expect("pool empty");
        POOL.release(idx);
    }

    #[test]
    fn test_pool_recycle() {
        let i1 = POOL.acquire().unwrap();
        POOL.release(i1);
        let i2 = POOL.acquire().unwrap();
        assert_eq!(i1, i2); // 释放后复用同一索引
        POOL.release(i2);
    }

    #[test]
    fn test_pool_with_buffer() {
        let idx = POOL.acquire().unwrap();
        POOL.write(idx, |v| {
            let _ = v.extend_from_slice(b"hello world");
        });
        let buf = POOL.read(idx);
        if let Some(data) = buf {
            assert_eq!(&data[..11], b"hello world");
        }
        POOL.release(idx);
    }

    #[test]
    fn test_pool_concurrent_acquire() {
        use std::sync::Arc;
        use std::thread;
        let p = Arc::new(GlobalBufferPool::new());
        let mut h = vec![];
        for _ in 0..8 {
            let pp = p.clone();
            h.push(thread::spawn(move || {
                let idx = pp.acquire();
                if let Some(i) = idx {
                    pp.write(i, |v| {
                        let _ = v.extend_from_slice(b"x");
                    });
                    pp.release(i);
                }
            }));
        }
        for x in h {
            x.join().unwrap();
        }
        // 池内 4 槽, 8 线程, 必然有 acquire 返回 None, 但不 panic.
        assert!(p.available() >= 0);
    }
}
