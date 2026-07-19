//! 系统健康监控
//!
//! 提供两层保护:
//!
//! 1. **任务看门狗** — 通过 ESP-IDF Task Watchdog, 关键任务超时触发系统复位
//!    - `subscribe_wdt()` 把当前任务加入看门狗监控
//!    - `feed_wdt()` 喂狗 (必须在 wdt_timeout 内调用)
//!    - sdkconfig: CONFIG_ESP_TASK_WDT_INIT=y, TIMEOUT_S=10
//!
//! 2. **任务心跳** — 软件心跳, main_loop 周期检查各任务是否存活
//!    - `register(&'static TaskHb)` 启动时注册
//!    - `tick(&'static TaskHb)` 任务 loop 中递增
//!    - `check_all()` 返回停滞任务名列表 (心跳未变化)
//!
//! # 设计原则
//!
//! - 静态分配, 无堆分配 (适合工业实时场景)
//! - 心跳用 AtomicU32, 无锁
//! - 任务表用固定数组 (最多 16 个任务)
//! - 不依赖 panic=unwind, 不用 catch_unwind

use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

/// 最大监控任务数
const MAX_TASKS: usize = 16;

/// 单个任务的心跳记录
///
/// 使用 `const fn` 构造, 可作为 `static` 全局变量。
pub struct TaskHb {
    /// 任务名 (用于日志)
    pub name: &'static str,
    /// 心跳计数 (任务 loop 中递增)
    counter: AtomicU32,
    /// 上次检查时的快照 (main_loop 更新)
    last_check: AtomicU32,
    /// 允许的最大停滞次数 (main_loop 周期数), 超过即判定停滞
    /// 0 = 使用默认阈值 (3, 即 ~3s @ 1s 检查周期)
    pub max_stall: AtomicU32,
    /// 连续停滞次数累积 (check_all 维护, 心跳变化时清零)
    stall_count: AtomicU32,
}

impl TaskHb {
    /// 构造一个静态心跳记录 (默认停滞阈值 3)
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            counter: AtomicU32::new(0),
            last_check: AtomicU32::new(0),
            max_stall: AtomicU32::new(3),
            stall_count: AtomicU32::new(0),
        }
    }

    /// 构造一个静态心跳记录, 指定最大停滞阈值
    /// (用于阻塞等待型任务, 如 TCP 监听/RTU 从站, 允许较长时间无活动)
    pub const fn new_with_stall(name: &'static str, stall: u32) -> Self {
        Self {
            name,
            counter: AtomicU32::new(0),
            last_check: AtomicU32::new(0),
            max_stall: AtomicU32::new(stall),
            stall_count: AtomicU32::new(0),
        }
    }

    /// 任务 loop 中调用, 递增心跳计数
    pub fn tick(&self) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// 全局任务表 (无锁原子指针表)
///
/// 单写者 (启动期 register) + 多读者 (main_loop check_all);
/// 用 [`AtomicPtr`] 持有 `&'static TaskHb`, [`AtomicUsize`] 计数,
/// 完全无锁, 无自旋, 无中毒.
struct Registry {
    /// 任务指针表 (空槽 = null)
    tasks: [AtomicPtr<TaskHb>; MAX_TASKS],
    /// 已注册任务数 (单写者启动期 fetch_add)
    count: AtomicUsize,
}

static REGISTRY: Registry = Registry {
    tasks: [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_TASKS],
    count: AtomicUsize::new(0),
};

/// 默认停滞阈值 (main_loop 检查周期数)
const DEFAULT_STALL_THRESHOLD: u32 = 3;

/// 注册任务到全局心跳表 (无锁)
///
/// 应在任务启动时 (init 函数中) 调用一次.
/// 超过 MAX_TASKS 时回退计数并记日志忽略.
pub fn register(task: &'static TaskHb) {
    let idx = REGISTRY.count.fetch_add(1, Ordering::AcqRel);
    if idx >= MAX_TASKS {
        // 越界: 回退计数, 拒绝注册
        REGISTRY.count.fetch_sub(1, Ordering::AcqRel);
        log::error!("[health] task table full, cannot register '{}'", task.name);
        return;
    }
    // SAFETY: task 为 &'static TaskHb, 存为 AtomicPtr 长期有效.
    REGISTRY.tasks[idx].store(task as *const TaskHb as *mut TaskHb, Ordering::Release);
    log::debug!("[health] registered task '{}' (slot {})", task.name, idx);
}

/// 检查所有已注册任务的心跳
///
/// 返回停滞任务名列表 (心跳连续未变化次数超过 max_stall)。
/// main_loop 每 1s 调用一次。
pub fn check_all() -> heapless::Vec<&'static str, MAX_TASKS> {
    let mut stalled = heapless::Vec::new();
    let n = REGISTRY.count.load(Ordering::Acquire);
    for i in 0..n {
        let ptr = REGISTRY.tasks[i].load(Ordering::Acquire);
        if ptr.is_null() {
            continue;
        }
        // SAFETY: 指针由 register() 的 &'static TaskHb 存入, 生命周期 = 程序.
        let t: &'static TaskHb = unsafe { &*ptr };
        let cur = t.counter.load(Ordering::Relaxed);
        let last = t.last_check.load(Ordering::Relaxed);
        let threshold = t.max_stall.load(Ordering::Relaxed);
        let threshold = if threshold == 0 { DEFAULT_STALL_THRESHOLD } else { threshold };

        if cur == last {
            // 心跳停滞, 累积计数
            let sc = t.stall_count.fetch_add(1, Ordering::Relaxed) + 1;
            if sc >= threshold {
                let _ = stalled.push(t.name);
                // 达到阈值后不再累积, 避免溢出; 由 main_loop 决定是否复位
            }
        } else {
            // 心跳变化, 清零停滞计数
            t.stall_count.store(0, Ordering::Relaxed);
        }
        // 更新快照
        t.last_check.store(cur, Ordering::Relaxed);
    }
    stalled
}

/// 更新所有任务的 last_check 快照 (无锁)
///
/// 在 check_all 之后调用, 用于下次比较.
/// (check_all 内部已用 store 更新, 此函数供外部重置用)
pub fn snapshot() {
    let n = REGISTRY.count.load(Ordering::Acquire);
    for i in 0..n {
        let ptr = REGISTRY.tasks[i].load(Ordering::Acquire);
        if ptr.is_null() {
            continue;
        }
        // SAFETY: register 存入的 &'static TaskHb, 生命周期 = 程序.
        let t: &'static TaskHb = unsafe { &*ptr };
        let cur = t.counter.load(Ordering::Relaxed);
        t.last_check.store(cur, Ordering::Relaxed);
    }
}

// ----------------------------------------------------------------------------
// ESP-IDF Task Watchdog 封装
// ----------------------------------------------------------------------------

/// 把当前任务加入 ESP-IDF Task Watchdog 监控
///
/// 必须在任务上下文内调用 (会绑定当前 task handle)。
/// 加入后必须在 CONFIG_ESP_TASK_WDT_TIMEOUT_S (10s) 内调用 `feed_wdt()`。
pub fn subscribe_wdt() {
    // esp_task_wdt_add(NULL) 表示添加当前任务
    let r = unsafe { esp_idf_sys::esp_task_wdt_add(std::ptr::null_mut()) };
    if r != 0 {
        log::warn!("[health] esp_task_wdt_add failed: 0x{:08X}", r);
    } else {
        log::debug!("[health] current task subscribed to WDT");
    }
}

/// 喂狗 (重置当前任务的看门狗计时器)
///
/// 必须先 `subscribe_wdt()` 才能调用。
pub fn feed_wdt() {
    let r = unsafe { esp_idf_sys::esp_task_wdt_reset() };
    if r != 0 {
        // 不打日志, 避免高频失败刷屏
    }
}

/// 从看门狗移除当前任务 (任务退出前调用)
pub fn unsubscribe_wdt() {
    let _ = unsafe { esp_idf_sys::esp_task_wdt_delete(std::ptr::null_mut()) };
}

// ----------------------------------------------------------------------------
// 任务核间绑定 (ESP32-S3 双核优化)
// ----------------------------------------------------------------------------
//
// ESP32-S3 双核: Core 0 (网络/BLE/协议栈) + Core 1 (实时采集/IO)
// Rust std::thread::spawn 通过 pthread_create 创建线程。
//
// 方案: 在 spawn 之前调用 `set_next_thread_core(N)` 配置下一次
// pthread_create 的目标核心。ESP-IDF 的 esp_pthread_set_cfg 影响
// 随后创建的线程（非当前线程）。
//
// 核分配策略:
//   Core 0 — 网络/协议栈 (LwIP/W5500/mb-tcp/mb-rtu/ble-mesh/main-loop)
//   Core 1 — 实时 IO (di-scan/do-output/ai-sample/ao-output)

/// 设置**下一次** `std::thread::spawn` 创建线程的目标核心。
///
/// 调用后立即 spawn 的线程会被固定到指定核心。
/// 注意: 这是全局状态, 影响所有后续线程创建, 直到再次调用。
/// 建议在 spawn 前调用, spawn 后恢复为默认值 (tskNO_AFFINITY = -1)。
#[inline]
pub fn set_next_thread_core(core: u32) {
    let mut cfg = unsafe { esp_idf_sys::esp_pthread_get_default_config() };
    cfg.pin_to_core = core as i32;
    cfg.inherit_cfg = false; // 不让子线程继承此配置
    unsafe { esp_idf_sys::esp_pthread_set_cfg(&cfg) };
}

/// 恢复线程创建到默认行为 (不绑定核心)
#[inline]
pub fn reset_thread_core() {
    let mut cfg = unsafe { esp_idf_sys::esp_pthread_get_default_config() };
    cfg.pin_to_core = -1; // tskNO_AFFINITY
    cfg.inherit_cfg = false;
    unsafe { esp_idf_sys::esp_pthread_set_cfg(&cfg) };
}

/// Core 0 — 网络/协议栈
pub const CORE_NET: u32 = 0;
/// Core 1 — 实时采集
pub const CORE_RT: u32 = 1;

/// 已废弃: ESP-IDF 不支持在任务内动态修改核心绑定。
/// 请改用 `set_next_thread_core()` 在 spawn 之前调用。
#[deprecated(since = "0.2.0", note = "use set_next_thread_core() before thread::spawn instead")]
#[inline]
pub fn pin_current_to_core(_core: u32) {}

// ----------------------------------------------------------------------------
// 启动时打印任务-核心分配表 (便于调试)
// ----------------------------------------------------------------------------

/// 任务-核心分配信息
pub struct TaskCoreInfo {
    pub name: &'static str,
    pub core: u32,
}

/// 启动时调用, 打印所有已注册任务的核分配 (无锁读注册表)
/// 建议在 main loop 启动后调用, 此时所有任务已注册
pub fn print_core_assignment() {
    use core::sync::atomic::Ordering;
    let n = REGISTRY.count.load(Ordering::Acquire);
    log::info!("=== Task-Core Assignment ({n} tasks) ===");
    for i in 0..n {
        let ptr = REGISTRY.tasks[i].load(Ordering::Acquire);
        if ptr.is_null() {
            continue;
        }
        // SAFETY: register 存入的 &'static TaskHb
        let t: &'static TaskHb = unsafe { &*ptr };
        log::info!(
            "  [{:2}] {} (max_stall={})",
            i, t.name, t.max_stall.load(Ordering::Relaxed)
        );
    }
}
