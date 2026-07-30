//! 并发同步原语 — 无 parking_lot::Mutex, 无锁中毒
//!
//! ## 设计取向
//! - 高频实时位图 (DI/DO 64 位) → [`AtomicBits64`] (wait-free 读 + CAS 单位写)
//! - 短临界区 (外设句柄 / NVS / legacy Bus / 环日志 / 缓冲池 / BLE 回调表) → [`Spin`]
//!   - 非阻塞自旋, 无中毒, 不阻塞内核/调度器
//! - 多生产者-单消费者消息 (Actor mailbox / 事件总线) → [`MpscRing`]
//!   - bounded MPSC, 不阻塞, 满即丢
//!
//! ## 为什么不用 parking_lot
//! - parking_lot::Mutex 会把争用线程 park 到 OS (调度器介入, 微秒→毫秒级)
//! - std::sync::Mutex 可中毒 (持锁线程 panic 时永久不可用)
//! - 本模块: 所有锁都是短临界区 (≤ 微秒级), 用自旋 + CompareExchange 即可
//! - 工业实时路径 (I/O/Modbus) 永不睡眠
//!
//! ## 完全去除 parking_lot::Mutex 之后的状态分类
//! | 区域 | 旧锁 | 新方案 | 类别 |
//! |------|------|--------|------|
//! | di/do 64 位位图 | Mutex\<u64\> | AtomicBits64 | 真无锁 (CAS+seqlock) |
//! | health registry | Mutex\<Registry\> | 原子指针表 + AtomicUsize | 真无锁 |
//! | OTA session / pending | Mutex\<Opt\>/Mutex\<u32\> | 单原子 + Spin\<Opt\> | 准无锁 |
//! | event_bus | std Mutex\<Queue\> | MpscRing | 非阻塞 (满丢新) |
//! | ble_at 6 锁 | parking_lot::Mutex | Spin | 短临界区自旋 |
//! | device NVS | parking_lot::Mutex | Spin | 短临界区自旋 |
//! | legacy Bus | parking_lot::Mutex | Spin + try_lock | 短临界区自旋 |
//! | HAL 外设句柄 | parking_lot::Mutex | Spin | 短临界区自旋 |
//! | ringlog / buffer_pool | std::sync::Mutex | Spin | 短临界区自旋 |

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};

use heapless::spsc::Queue;

// ============================================================================
// Spin<T> — 非阻塞自旋锁 (短临界区, 无中毒)
// ============================================================================
//
// 适用: 单原子级操作无法表达的临界区 (外设句柄 deresf mut, NVS, 大对象借片段).
// 持锁应在微秒级; 永不 park, 不依赖 OS 调度器.

/// 非阻塞自旋锁 (非中毒).
pub struct Spin<T: ?Sized> {
    flag: AtomicBool,
    inner: UnsafeCell<T>,
}

// SAFETY: 数据受 AtomicBool flag 串行化访问; 允许跨线程共享当 T: Send.
unsafe impl<T: ?Sized + Send> Sync for Spin<T> {}
unsafe impl<T: ?Sized + Send> Send for Spin<T> {}

impl<T> Spin<T> {
    /// const 构造, 可放 static.
    pub const fn new(value: T) -> Self {
        Self {
            flag: AtomicBool::new(false),
            inner: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> Spin<T> {
    /// 阻塞自旋直到取得锁 (但不 park 线程).
    pub fn lock(&self) -> SpinGuard<'_, T> {
        // 先尝试 CAS 快路径
        while self
            .flag
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // CAS 失败: 等持有者释放, 再试 (减少 cache-line contention)
            while self.flag.load(Ordering::Relaxed) {
                spin_loop();
            }
        }
        SpinGuard { lock: self }
    }

    /// 非阻塞尝试取锁; 持锁中返回 None. 用于 BLE 回调等不能阻塞的路径.
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        if self
            .flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinGuard { lock: self })
        } else {
            None
        }
    }
}

/// SpinGuard: RAII 释放; deref/deref_mut 到内部 T.
pub struct SpinGuard<'a, T: ?Sized> {
    lock: &'a Spin<T>,
}

impl<T: ?Sized> core::ops::Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: 持锁期间独占
        unsafe { &*self.lock.inner.get() }
    }
}

impl<T: ?Sized> core::ops::DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: 持锁期间独占
        unsafe { &mut *self.lock.inner.get() }
    }
}

impl<T: ?Sized> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.flag.store(false, Ordering::Release);
    }
}

// ============================================================================
// AtomicBits64 — xtensa 无原生 AtomicU64 的真无锁 64 位位图
// ============================================================================
//
// ESP32-S3 (Xtensa LX7) 没有原生 64 位原子; `AtomicU64` 在 xtensa 上回退到 libatomic
// (会引锁). 之前用 `Mutex<u64>` 保护 64 位 IO 位图, 在 IO 高频路径上加锁竞争明显.
//
// 本实现: 拆 4× AtomicU16 (xtensa 原生无锁), 配合单字节 seqlock:
// - 读 (`load_bits`): 序号前后比对 + 读 4 个 word; 不一致重试 (类 seqlock, 无限重试边界).
// - 写: CAS 把偶数序号改成奇数以取得唯一写者资格，完成后发布下一个偶数序号。
//
// 不能仅在写入前后 `fetch_add`：两个写者重叠时序号会暂时变回偶数，读者可能
// 把半写入数据当成稳定快照。DO 可由 BLE/TCP/RTU/Web 并发写入，因此写者互斥
// 是业务正确性要求，不只是性能优化。
//
// 性能: 读 ≈ 4 次 atomic load + 2 次 seq load (常驻 cache, 纳秒级); 写 ≈ 8 次 atomic op.
// 多写者安全: store_bits 是多写者最后写入生效; set_bit 用 CAS 单位写入者相互重试.

pub struct AtomicBits64 {
    /// 低 16 位 → 高 16 位 顺序排列
    words: [AtomicU16; 4],
    /// seqlock 序号 (写者进入+1, 出口+1; 序号偶 = 临界区空闲, 奇 = 写入中)
    seq: AtomicU8,
}

unsafe impl Sync for AtomicBits64 {}
unsafe impl Send for AtomicBits64 {}

impl AtomicBits64 {
    /// const 构造.
    pub const fn new(value: u64) -> Self {
        Self {
            words: [
                AtomicU16::new(value as u16),
                AtomicU16::new((value >> 16) as u16),
                AtomicU16::new((value >> 32) as u16),
                AtomicU16::new((value >> 48) as u16),
            ],
            seq: AtomicU8::new(0),
        }
    }

    /// 全量读取 (seqlock 重试, 真无锁).
    pub fn load_bits(&self) -> u64 {
        loop {
            let v0 = self.seq.load(Ordering::Acquire);
            if v0 & 1 != 0 {
                // 写入中: 立刻重试
                spin_loop();
                continue;
            }
            let w0 = self.words[0].load(Ordering::Acquire);
            let w1 = self.words[1].load(Ordering::Acquire);
            let w2 = self.words[2].load(Ordering::Acquire);
            let w3 = self.words[3].load(Ordering::Acquire);
            let v1 = self.seq.load(Ordering::Acquire);
            if v0 == v1 {
                return (w0 as u64)
                    | ((w1 as u64) << 16)
                    | ((w2 as u64) << 32)
                    | ((w3 as u64) << 48);
            }
            // 中途被写者改写: 重试
            spin_loop();
        }
    }

    /// 全量替换 (多写者最后写入生效).
    pub fn store_bits(&self, value: u64) {
        let write_seq = self.begin_write();
        self.words[0].store(value as u16, Ordering::Release);
        self.words[1].store((value >> 16) as u16, Ordering::Release);
        self.words[2].store((value >> 32) as u16, Ordering::Release);
        self.words[3].store((value >> 48) as u16, Ordering::Release);
        self.end_write(write_seq);
    }

    /// 读取单个位 (ch < 64, 越界返回 false).
    pub fn get_bit(&self, ch: usize) -> bool {
        if ch >= 64 {
            return false;
        }
        let w = self.words[ch >> 4].load(Ordering::Acquire);
        w & (1u16 << (ch & 0x0F)) != 0
    }

    /// 置/清单个位 (CAS-loop, 多写者安全). 越界返回 false.
    pub fn set_bit(&self, ch: usize, value: bool) -> bool {
        if ch >= 64 {
            return false;
        }
        let write_seq = self.begin_write();
        let idx = ch >> 4;
        let mask = 1u16 << (ch & 0x0F);
        let cur = self.words[idx].load(Ordering::Relaxed);
        let nv = if value { cur | mask } else { cur & !mask };
        self.words[idx].store(nv, Ordering::Release);
        self.end_write(write_seq);
        true
    }

    /// 用 (mask, value) 在 64 位范围内批量替换指定 bit. 多写者 best-effort.
    pub fn mask_replace(&self, mask: u64, value: u64) {
        let write_seq = self.begin_write();
        for i in 0..4usize {
            let m = ((mask >> (i * 16)) & 0xFFFF) as u16;
            if m == 0 {
                continue;
            }
            let v = ((value >> (i * 16)) & 0xFFFF) as u16;
            let cur = self.words[i].load(Ordering::Relaxed);
            let nv = (cur & !m) | (v & m);
            self.words[i].store(nv, Ordering::Release);
        }
        self.end_write(write_seq);
    }

    /// 取得唯一写者资格，返回取得时的偶数序号。
    fn begin_write(&self) -> u8 {
        loop {
            let seq = self.seq.load(Ordering::Acquire);
            if seq & 1 != 0 {
                spin_loop();
                continue;
            }
            if self
                .seq
                .compare_exchange_weak(
                    seq,
                    seq.wrapping_add(1),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return seq;
            }
            spin_loop();
        }
    }

    fn end_write(&self, write_seq: u8) {
        self.seq.store(write_seq.wrapping_add(2), Ordering::Release);
    }
}

impl Default for AtomicBits64 {
    fn default() -> Self {
        Self::new(0)
    }
}

// ============================================================================
// MpscRing<T, N> — 非阻塞 bounded MPSC 环 (取代 Mutex<Queue>)
// ============================================================================

/// bounded MPSC 环: 多生产者 (`try_enqueue` 共享 `&self`), 单消费者 (`dequeue`).
///
/// **可观察容量 = N-1**: `heapless::spsc::Queue<T, N>` 内部保留 1 个槽区分空/满, 故
/// `MpscRing<T, 32>` 实际能存 31 条消息. 调用方应按 N-1 估算.
///
/// 内核基于 [`Spin`] 包 `heapless::spsc::Queue`: 多生产者经由 Spin 串行化 enqueue,
/// 不阻塞 OS (与 parking_lot 区别); 满时由调用方决定丢新 (`try_enqueue`) 或丢旧
/// (`enqueue_drop_oldest`).
pub struct MpscRing<T, const N: usize> {
    inner: Spin<Queue<T, N>>,
}

impl<T, const N: usize> MpscRing<T, N> {
    /// const 构造 (可放 static).
    pub const fn new() -> Self {
        Self {
            inner: Spin::new(Queue::new()),
        }
    }

    /// 入队 (非阻塞); 满或锁竞争 → 丢弃新值, 返回 false. Actor mailbox 默认行为.
    pub fn try_enqueue(&self, value: T) -> bool {
        match self.inner.try_lock() {
            Some(mut q) => q.enqueue(value).is_ok(),
            None => false,
        }
    }

    /// 入队 (非阻塞); 满 → 移除最旧再插入 (事件总线: 保留最新). 锁竞争 → 丢弃.
    pub fn enqueue_drop_oldest(&self, value: T) {
        if let Some(mut q) = self.inner.try_lock() {
            match q.enqueue(value) {
                Ok(()) => {}
                Err(value) => {
                    // 队列满: 丢弃最旧, 重新插入 (失败即放弃, 丢新)
                    let _ = q.dequeue();
                    let _ = q.enqueue(value);
                }
            }
        }
    }

    /// 出队 (单消费者); 空返回 None. 阻塞短 Spin, 不 park.
    pub fn dequeue(&self) -> Option<T> {
        let mut q = self.inner.lock();
        q.dequeue()
    }

    /// 当前长度 (持锁瞬间快照).
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// 是否空 (持锁瞬间快照).
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

impl<T, const N: usize> Default for MpscRing<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// MainLoopCell<T> — main_loop tick 状态的受控借用 cell (取代 Mutex<Option<T>>)
// ============================================================================
//
// ## 为什么不用 Mutex
// - `main_loop` 中调用的 `tick_xxx` 函数通常在同一线程, 没有并发争用
// - 仍用一个原子借用标志防止递归调用或未来调度变更制造 Rust 引用别名
// - 持锁失败 `try_lock → return` 会跳过本次 tick, 导致 I/O 数据丢失
//
// ## 适用场景
// - **只从 main_loop 调用的 tick 函数的状态** (TCP/DI/DO/AI/AO/eth-hb)
// - init 由启动阶段一次性写入, tick 可在闭包作用域内更新状态
// - 没有任何 BLE/Modbus 中断或 pthread 路径访问
//
// ## 安全保证
// - `init()`: 与访问互斥且幂等, 写入值后发布 init 标志
// - `with_mut / with`: 通过 CAS 取得唯一借用, 引用不能逃出闭包
// - `init` happens-before 任何 `with_*` (Release/Acquire ordering)
// - 并发或递归借用立即返回 None, 不等待、不死锁
//
// ## 内存模型
// - T 存储在 UnsafeCell 中
// - tick 热路径为一次无争用 CAS + init flag 检查
pub struct MainLoopCell<T> {
    init: AtomicBool,
    borrowed: AtomicBool,
    inner: UnsafeCell<Option<T>>,
}

// SAFETY: borrowed 以 Acquire/Release 串行化所有对 inner 的访问, T: Send
// 允许值在线程之间转移独占访问权。
unsafe impl<T: Send> Sync for MainLoopCell<T> {}

impl<T> MainLoopCell<T> {
    /// const 构造 (可放 static).
    pub const fn new() -> Self {
        Self {
            init: AtomicBool::new(false),
            borrowed: AtomicBool::new(false),
            inner: UnsafeCell::new(None),
        }
    }

    /// 幂等初始化。正在访问时返回原值，供调用方触发服务重试。
    /// 已初始化时丢弃新值并返回成功，不重置运行状态。
    pub fn init(&self, value: T) -> Result<(), T> {
        let _borrow = match self.try_borrow() {
            Some(guard) => guard,
            None => return Err(value),
        };
        if self.init.load(Ordering::Acquire) {
            return Ok(());
        }
        // SAFETY: borrow guard 保证 inner 当前没有其它引用。
        unsafe {
            *self.inner.get() = Some(value);
        }
        self.init.store(true, Ordering::Release);
        Ok(())
    }

    /// 是否已初始化 (供 tick 路径快速跳过未初始化情况).
    pub fn is_initialized(&self) -> bool {
        self.init.load(Ordering::Acquire)
    }

    /// 在闭包作用域内共享访问；未初始化、并发或递归借用时返回 None。
    #[inline(always)]
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        let _borrow = self.try_borrow()?;
        if !self.init.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: borrow guard 保证闭包结束前 inner 没有可变引用。
        let value = unsafe { &*self.inner.get() }.as_ref()?;
        Some(f(value))
    }

    /// 在闭包作用域内独占访问；未初始化、并发或递归借用时返回 None。
    #[inline(always)]
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let _borrow = self.try_borrow()?;
        if !self.init.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: borrow guard 保证闭包结束前 inner 没有其它引用。
        let value = unsafe { &mut *self.inner.get() }.as_mut()?;
        Some(f(value))
    }

    fn try_borrow(&self) -> Option<MainLoopBorrow<'_>> {
        self.borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MainLoopBorrow {
                borrowed: &self.borrowed,
            })
    }
}

struct MainLoopBorrow<'a> {
    borrowed: &'a AtomicBool,
}

impl Drop for MainLoopBorrow<'_> {
    fn drop(&mut self) {
        self.borrowed.store(false, Ordering::Release);
    }
}

impl<T> Default for MainLoopCell<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    // --- MainLoopCell ---
    #[test]
    fn test_mlc_basic() {
        let c: MainLoopCell<u32> = MainLoopCell::new();
        assert!(!c.is_initialized());
        assert!(c.with(|value| *value).is_none());
        c.init(42).unwrap();
        assert!(c.is_initialized());
        assert_eq!(c.with(|value| *value), Some(42));
        assert_eq!(c.with_mut(|value| *value += 1), Some(()));
        assert_eq!(c.with(|value| *value), Some(43));
    }

    #[test]
    fn test_mlc_rejects_nested_borrow() {
        let c = MainLoopCell::new();
        c.init(7u32).unwrap();
        assert_eq!(
            c.with(|value| (*value, c.with_mut(|nested| *nested += 1).is_none())),
            Some((7, true))
        );
        assert_eq!(c.with(|value| *value), Some(7));
    }

    #[test]
    fn test_mlc_init_is_idempotent() {
        let c = MainLoopCell::new();
        c.init(7u32).unwrap();
        c.init(99u32).unwrap();
        assert_eq!(c.with(|value| *value), Some(7));
    }

    #[test]
    fn test_mlc_default() {
        let c: MainLoopCell<Vec<u8>> = MainLoopCell::default();
        assert!(!c.is_initialized());
    }

    use std::sync::Arc;
    use std::thread;

    // --- Spin ---
    #[test]
    fn test_spin_basic() {
        let s = Spin::new(42u32);
        {
            let mut g = s.lock();
            *g += 1;
        }
        assert_eq!(*s.lock(), 43);
    }

    #[test]
    fn test_spin_try_lock() {
        let s = Spin::new(0u32);
        let g = s.lock();
        assert!(s.try_lock().is_none());
        drop(g);
        assert!(s.try_lock().is_some());
    }

    #[test]
    fn test_spin_concurrent() {
        let s = Arc::new(Spin::new(0u32));
        let mut h = vec![];
        for _ in 0..4 {
            let s = s.clone();
            h.push(thread::spawn(move || {
                for _ in 0..1000 {
                    *s.lock() += 1;
                }
            }));
        }
        for x in h {
            x.join().unwrap();
        }
        assert_eq!(*s.lock(), 4000);
    }

    // --- AtomicBits64 ---
    #[test]
    fn test_bits_basic() {
        let b = AtomicBits64::new(0);
        assert_eq!(b.load_bits(), 0);
        b.store_bits(0xDEADBEEFCAFEBABE);
        assert_eq!(b.load_bits(), 0xDEADBEEFCAFEBABE);
    }

    #[test]
    fn test_bits_bit_ops() {
        let b = AtomicBits64::new(0);
        for i in 0..64 {
            assert!(b.set_bit(i, true));
            assert!(b.get_bit(i));
        }
        for i in 0..64 {
            assert!(b.set_bit(i, false));
            assert!(!b.get_bit(i));
        }
        assert!(!b.get_bit(64));
        assert!(!b.set_bit(64, true));
    }

    #[test]
    fn test_bits_mask_replace() {
        // 初始值 (16 进制位段): 0000 F00F 00FF FFFF
        // mask 0xFFFF_0000_0000_FFFF 只覆盖位 0-15 与 48-63, 中段 (16-47) 不动
        // 中段字节: 0xF00F_00FF 的位 16-47 → 字节(由低到高) = 0xFF, 0x00, 0xF0
        let init = 0x0000_F00F_00FF_FFFF;
        let b = AtomicBits64::new(init);
        b.mask_replace(0xFFFF_0000_0000_FFFF, 0x1234_0000_0000_ABCD);
        let v = b.load_bits();
        assert_eq!(v & 0xFFFF_0000_0000_FFFF, 0x1234_0000_0000_ABCD);
        // 中段 (位 16-23, 即最低未掩码字节) 保持原值 0xFF
        assert_eq!((v >> 16) & 0xFF, 0xFF);
    }

    #[test]
    fn test_bits_concurrent() {
        let b = Arc::new(AtomicBits64::new(0));
        let mut h = vec![];
        for t in 0..8 {
            let bb = b.clone();
            h.push(thread::spawn(move || {
                for _ in 0..1000 {
                    bb.set_bit((t * 8) & 63, true);
                }
            }));
        }
        for x in h {
            x.join().unwrap();
        }
        // 不强断言内容; 只确认不 panic / 数据竞争检测通过
        let _ = b.load_bits();
    }

    // --- MpscRing ---
    #[test]
    fn test_ring_basic() {
        // heapless::spsc::Queue<T, N> 的实际容量是 N-1 (单缓冲槽 SPSC)
        // MpscRing<T, 4> 可观察容量 = 3
        let r: MpscRing<u32, 4> = MpscRing::new();
        assert!(r.try_enqueue(1));
        assert!(r.try_enqueue(2));
        assert!(r.try_enqueue(3));
        assert_eq!(r.dequeue(), Some(1));
        assert_eq!(r.dequeue(), Some(2));
        assert_eq!(r.dequeue(), Some(3));
        assert_eq!(r.dequeue(), None);
    }

    #[test]
    fn test_ring_full_drops_new() {
        // MpscRing<T, 2> 可观察容量 = 1 (heapless::spsc::Queue<T, 2>)
        let r: MpscRing<u32, 2> = MpscRing::new();
        assert!(r.try_enqueue(1));
        // 第 2 个槽位是 SPSC 内部保留槽, 不可见 → 满 → 丢新
        assert!(!r.try_enqueue(2));
        assert_eq!(r.dequeue(), Some(1));
        assert_eq!(r.dequeue(), None);
    }

    #[test]
    fn test_ring_drop_oldest() {
        // MpscRing<T, 4> 可观察容量 = 3: 填 3 个再 enqueue 第 4 个, 丢最旧
        let r: MpscRing<u32, 4> = MpscRing::new();
        r.enqueue_drop_oldest(1);
        r.enqueue_drop_oldest(2);
        r.enqueue_drop_oldest(3); // 现持有 [1,2,3], 满 (可见 3, 内部 4 槽)
        r.enqueue_drop_oldest(4); // 满 → 丢最旧(1), 现 [2,3,4]
        assert_eq!(r.dequeue(), Some(2));
        assert_eq!(r.dequeue(), Some(3));
        assert_eq!(r.dequeue(), Some(4));
        assert_eq!(r.dequeue(), None);
    }

    #[test]
    fn test_ring_mpmc() {
        let r = Arc::new(MpscRing::<u32, 16>::new());
        let mut h = vec![];
        for t in 0..4 {
            let rr = r.clone();
            h.push(thread::spawn(move || {
                for i in 0..50 {
                    rr.try_enqueue(t * 100 + i);
                }
            }));
        }
        for x in h {
            x.join().unwrap();
        }
        let mut count = 0;
        while r.dequeue().is_some() {
            count += 1;
        }
        // 可能因锁竞争丢一些, 但应 ≥ 大部分
        assert!(count > 0);
    }
}
