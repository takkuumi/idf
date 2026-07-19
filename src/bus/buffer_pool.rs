//! 缓冲区池 - 减少 Modbus TCP 响应堆分配
//!
//! ## 简化设计
//! 用 RefCell 包装, 不需要 Sync (单线程访问池, 内部数据可由 Mutex 保护)
//! 注: 此模块主要用于未来优化 Modbus TCP, 当前作为 API 占位符

use crate::sync::Spin;

use std::sync::LazyLock;

const POOL_SIZE: usize = 4;
const BUF_SIZE: usize = 256;

/// 全局缓冲池 (Mutex 保护, 完全线程安全)
pub struct GlobalBufferPool {
    inner: Spin<[heapless::Vec<u8, BUF_SIZE>; POOL_SIZE]>,
    used: Spin<[bool; POOL_SIZE]>,
}

impl GlobalBufferPool {
    pub const fn new() -> Self {
        Self {
            inner: Spin::new([const { heapless::Vec::new() }; POOL_SIZE]),
            used: Spin::new([false; POOL_SIZE]),
        }
    }

    /// 获取缓冲区
    /// 注: 由于 Rust 借用规则, 池缓冲区的 RAII 模式较难实现
    /// 这里返回 Option<usize> (缓冲区索引), 由调用方在使用完后归还
    pub fn acquire(&self) -> Option<usize> {
        let mut used = self.used.lock();
        for i in 0..POOL_SIZE {
            if !used[i] {
                used[i] = true;
                return Some(i);
            }
        }
        None
    }

    /// 释放缓冲区
    pub fn release(&self, idx: usize) {
        let mut used = self.used.lock();
        used[idx] = false;
    }

    /// 写入缓冲区
    /// SAFETY: 调用方需保证 idx 是 acquire 返回的合法值
    pub fn write<F>(&self, idx: usize, f: F)
    where F: FnOnce(&mut heapless::Vec<u8, BUF_SIZE>)
    {
        let mut pool = self.inner.lock();
        pool[idx].clear();
        f(&mut pool[idx]);
    }

    /// 读取缓冲区
    pub fn read(&self, idx: usize) -> Option<heapless::Vec<u8, BUF_SIZE>> {
        Some(self.inner.lock()[idx].clone())
    }
}

/// 全局池 (Lazy 初始化)
pub static POOL: LazyLock<GlobalBufferPool> = LazyLock::new(GlobalBufferPool::new);

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
        assert_eq!(i1, i2);  // 释放后复用同一索引
        POOL.release(i2);
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;
    
    #[test]
    fn test_pool_with_buffer() {
        // 测试 acquire 然后写入, 再 read 出来
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
}
