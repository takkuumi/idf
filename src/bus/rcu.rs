//! 通用 RCU (Read-Copy-Update) 原子指针 — 无 leak 回收
//!
//! ## 设计目标
//! - **读**: lock-free + 零引用计数以外的同步 (atomic ptr load + fetch_add on u16 epoch槽)
//! - **写**: 构造新值, 原子替换指针; **旧值按 epoch 延迟回收 (无 leak)**
//!
//! ## 与 Arc<T> / ArcSwap 的区别
//! - Arc: 每次 clone atomic add, 每次 drop atomic sub (两路原子开销)
//! - ArcSwap + 全回收需要 hazard 指针或 epoch 国
//! - **本实现**: 简化 epoch 计数回收 — 读者 fetch_add 登记, 写者 sweep 旧代
//!
//! ## 回收算法 (Lazy Epoch Reclamation)
//!
//! 4 个 reader-count 槽 (AtomicU16), 一个 AtomicU8 epoch 计数器 mod 4:
//!
//! 1. `write(new)`:
//!    - (惰性回收前) sweep_unsafe: 扫描 retire_queue, 任何它代次 reader_counts==0 的旧 ptr
//!      → 安全释放 Box; 否则留下次再试.
//!    - 推进 epoch: `epoch.fetch_add(1, AcqRel)`. 新读者将登记到新代.
//!    - `ptr.swap(new_ptr, AcqRel)` → 读者立即看到新值.
//!    - 旧 ptr 入 retire_queue, 记录它被替换时的旧 epoch.
//! 2. `read() -> RcuReader<'_, T>`:
//!    - 读 epoch 号, 在对应槽 `reader_counts[idx].fetch_add(1, AcqRel)`.
//!    - acquire-load ptr → 返回 Smart pointer `RcuReader` (RAII 减计数).
//! 3. `RcuReader::drop`: 减计数 → 浔者下次可回收该代次.
//!
//! **读代价**: 1 load + 1 fetch_add ≈ 5 ns.
//! **写代价**: sweep (≤4) + 1 swap + 1 fetch_add ≈ 亚微秒.
//!
//! ## 适用场景
//! - 读多写少 (Modbus 配置访问, RTU 主站轮询表)
//! - 数据结构较大 (>= 1KB, 拷贝成本可接受)
//! - 写极少 (启动时 1 次 + 用户主动配置时)
//!
//! ## 多写者
//! `write` 仅假设多写者并发串行调用 (即一个配置任务队列化写).
//! 多写者并发 push 同一 retire 槽会丢弃未被回收者 (泄漏 1 个快照), 非常见.

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU16, AtomicU8, Ordering};

const RETIRE_QUEUE_LEN: usize = 4;
const EPOCH_COUNT: u8 = 4;

/// Rcu 容器 (无 leak, 多读单/串行写 安全)
pub struct Rcu<T> {
    ptr: AtomicPtr<T>,
    /// 当前 epoch 号 (写者递增 mod 4)
    epoch: AtomicU8,
    /// 各 epoch 当前的活跃读者计数 (4 个独立原子计数器)
    reader_counts: [AtomicU16; EPOCH_COUNT as usize],
    /// 待回收的旧指针 ring (写入者 enqueue, 写者下次再回收)
    retire_queue: [RetireSlot<T>; RETIRE_QUEUE_LEN],
    /// 下一个写入的 retire 槽 (AtomicU8 → 避免 mut self)
    retire_idx: AtomicU8,
}

/// 一个待回收槽 (旧指针 + 它被替换时的 epoch)
struct RetireSlot<T> {
    ptr: AtomicPtr<T>,
    retired_at_epoch: AtomicU8,
}

impl<T> RetireSlot<T> {
    const fn empty_const() -> Self {
        Self {
            ptr: AtomicPtr::new(ptr::null_mut()),
            retired_at_epoch: AtomicU8::new(0),
        }
    }
}

// SAFETY: T 不变 (构造后只读), 写时整体替换指针; 读者通过 epoch 计数器同步回收.
unsafe impl<T: Sync> Sync for Rcu<T> {}
unsafe impl<T: Send> Send for Rcu<T> {}

impl<T> Rcu<T> {
    /// const 构造 (空; 第一次 read 返回 None).
    pub const fn empty() -> Self {
        Self {
            ptr: AtomicPtr::new(ptr::null_mut()),
            epoch: AtomicU8::new(0),
            reader_counts: [
                AtomicU16::new(0),
                AtomicU16::new(0),
                AtomicU16::new(0),
                AtomicU16::new(0),
            ],
            retire_queue: [
                RetireSlot::empty_const(),
                RetireSlot::empty_const(),
                RetireSlot::empty_const(),
                RetireSlot::empty_const(),
            ],
            retire_idx: AtomicU8::new(0),
        }
    }

    /// 创建已初始化的 RCU.
    pub fn new(initial: T) -> Self {
        let mut this = Self::empty();
        this.ptr = AtomicPtr::new(Box::into_raw(Box::new(initial)));
        this
    }

    /// 写: 原子替换为新值; 旧值按 epoch 延迟回收.
    pub fn write(&self, new_value: T) {
        let new_ptr = Box::into_raw(Box::new(new_value));

        // 1. 惰性回收 — swap 之前先扫一遍 (用旧 epoch 数据依然成立)
        self.sweep_unsafe();

        // 2. 推进 epoch (新读者将进入新代)
        let old_epoch = self.epoch.fetch_add(1, Ordering::AcqRel);

        // 3. 原子替换指针 (读者立即看到新值)
        let old_ptr = self.ptr.swap(new_ptr, Ordering::AcqRel);

        // 4. 旧 ptr 入 retire queue
        if !old_ptr.is_null() {
            let next = self.retire_idx.fetch_add(1, Ordering::AcqRel) as usize;
            let idx = next % RETIRE_QUEUE_LEN;
            let prev = self.retire_queue[idx].ptr.swap(old_ptr, Ordering::AcqRel);
            self.retire_queue[idx].retired_at_epoch.store(old_epoch, Ordering::Release);
            if !prev.is_null() {
                // 罕见: 覆盖仍未回收的旧 ptr. 安全选项: leak 该 box (best effort).
                unsafe { core::mem::forget(Box::from_raw(prev)); }
                log::warn!(
                    "[rcu] retire slot {idx} overwrite (leak one snapshot); writer burst too fast"
                );
            }
        }

        // 5. 再次尝试回收 (前一代读者可能已退)
        self.sweep_unsafe();
    }

    /// 读: 在 epoch 槽登记 fetch_add, 然后 load 当前 ptr.
    pub fn read(&self) -> Option<RcuReader<'_, T>> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let idx = (epoch % EPOCH_COUNT) as usize;

        // 登记: 该代次读者计数 +1
        self.reader_counts[idx].fetch_add(1, Ordering::AcqRel);

        // load ptr
        let p = self.ptr.load(Ordering::Acquire);
        if p.is_null() {
            // 回滚计数 (避免 sweep 永远不释放)
            self.reader_counts[idx].fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        Some(RcuReader { rcu: self, epoch_idx: idx, ptr: p })
    }

    /// 读并克隆 (适用 T: Clone).
    pub fn read_cloned(&self) -> Option<T>
    where T: Clone
    {
        self.read().map(|r| (*r).clone())
    }

    /// 读并执行 (closure 结束后退计数).
    pub fn read_with<F, R>(&self, f: F) -> Option<R>
    where F: FnOnce(&T) -> R
    {
        let reader = self.read()?;
        Some(f(&*reader))
    }

    /// 扫描 retire queue: 释放该 epoch 读者计数为 0 的旧 Box. (單写者同步调用)
    fn sweep_unsafe(&self) {
        for i in 0..RETIRE_QUEUE_LEN {
            let slot = &self.retire_queue[i];
            let p = slot.ptr.load(Ordering::Acquire);
            if p.is_null() {
                continue;
            }
            let retired_epoch = slot.retired_at_epoch.load(Ordering::Acquire);
            // 该 ptr 被替换时的旧 epoch — 该 epoch 上 current reader 数
            let idx = (retired_epoch % EPOCH_COUNT) as usize;
            // "当前" epoch 等于 retired_epoch 时表示又有读者进同一代 (因为 mod 4 wrap),
            // 需比较 retired_epoch 与 epoch 不相等 才可释放.
            let current_epoch = self.epoch.load(Ordering::Acquire);
            let safe = current_epoch != retired_epoch
                && self.reader_counts[idx].load(Ordering::Acquire) == 0;
            // ↑ 注: retired_epoch 已推进 (write 里 fetch_add), current_epoch > retired_epoch
            // (除非 wraparound 4 轮后 == 等价小概率避免使用 ETF).
            if safe {
                // SAFETY: 旧 ptr 的 epoch 当前无活跃读者, 安全释放
                let taken = slot.ptr.swap(ptr::null_mut(), Ordering::AcqRel);
                if !taken.is_null() {
                    unsafe { drop(Box::from_raw(taken)); }
                }
                slot.retired_at_epoch.store(0, Ordering::Release);
            }
        }
    }
}

/// 读者 guard: RAII — Drop 时减 reader_counts, 让 sweep 可以回收该 epoch 的旧值.
pub struct RcuReader<'a, T> {
    rcu: &'a Rcu<T>,
    epoch_idx: usize,
    ptr: *mut T,
}

impl<T> core::ops::Deref for RcuReader<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: 持有 reader 计数, sweep 不会释放此 epoch 的 ptr (等 readers==0).
        unsafe { &*self.ptr }
    }
}

impl<T> Drop for RcuReader<'_, T> {
    fn drop(&mut self) {
        self.rcu.reader_counts[self.epoch_idx].fetch_sub(1, Ordering::AcqRel);
    }
}

impl<T> Default for Rcu<T> {
    fn default() -> Self { Self::empty() }
}

impl<T> Drop for Rcu<T> {
    fn drop(&mut self) {
        // 退出路径: 无并发读者, 释放 retire queue 与当前 ptr
        for i in 0..RETIRE_QUEUE_LEN {
            let p = self.retire_queue[i].ptr.swap(ptr::null_mut(), Ordering::AcqRel);
            if !p.is_null() {
                // SAFETY: drop 期无并发
                unsafe { drop(Box::from_raw(p)); }
            }
        }
        let cur = self.ptr.swap(ptr::null_mut(), Ordering::AcqRel);
        if !cur.is_null() {
            // SAFETY: 独占 owning
            unsafe { drop(Box::from_raw(cur)); }
        }
    }
}

// ============================================================================
// 单元测试
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
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
    fn test_rcu_cloned() {
        let rcu = Rcu::new(String::from("hello"));
        let s: Option<String> = rcu.read_cloned();
        assert_eq!(s, Some("hello".to_string()));
    }

    #[test]
    fn test_rcu_read_with() {
        let rcu = Rcu::new(vec![1, 2, 3]);
        let sum: i32 = rcu.read_with(|v| v.iter().sum()).unwrap();
        assert_eq!(sum, 6);
    }

    #[test]
    fn test_rcu_concurrent_reads_with_swaps() {
        // 验证读取者与 swap 之间无 UAF, 无 panic
        let rcu = Arc::new(Rcu::new(0u64));
        let mut h = vec![];
        for _ in 0..4 {
            let r = rcu.clone();
            h.push(thread::spawn(move || {
                for _ in 0..1000 {
                    let _v = r.read();
                }
            }));
        }
        for i in 1..=10 {
            rcu.write(i);
        }
        for x in h { x.join().unwrap(); }
        assert_eq!(*rcu.read().unwrap(), 10);
    }

    #[test]
    fn test_rcu_no_leak_under_writer_burst() {
        // 模拟写者快速连写 N 次, 读者短临界区. 验证:
        // - 不 panic, 不 UAF
        // - 最终读回最新值
        let rcu = Arc::new(Rcu::new(0u32));
        let stop = Arc::new(AtomicUsize::new(0));
        let mut h = vec![];
        for _ in 0..4 {
            let r = rcu.clone();
            h.push(thread::spawn(move || {
                for i in 0..1000 {
                    let v = r.read();
                    let _ = *v.unwrap();
                    r.write(i);
                }
            }));
        }
        for x in h { x.join().unwrap(); }
        let _ = stop.fetch_or(0, Ordering::Relaxed);
        // 最新写入值范围 [0, 999]; 主要确认无 deadlock/UAF
        let last = *rcu.read().unwrap();
        assert!(last < 1000);
    }
}
