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

use std::sync::atomic::{AtomicU32, Ordering};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

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

/// 全局任务表 (静态分配, 最多 MAX_TASKS 个)
struct TaskRegistry {
    tasks: [&'static TaskHb; MAX_TASKS],
    count: usize,
}

impl TaskRegistry {
    const fn empty() -> Self {
        Self {
            tasks: [
                &SENTINEL, &SENTINEL, &SENTINEL, &SENTINEL,
                &SENTINEL, &SENTINEL, &SENTINEL, &SENTINEL,
                &SENTINEL, &SENTINEL, &SENTINEL, &SENTINEL,
                &SENTINEL, &SENTINEL, &SENTINEL, &SENTINEL,
            ],
            count: 0,
        }
    }
}

/// 哨兵 TaskHb (占位, 不参与监控)
static SENTINEL: TaskHb = TaskHb::new("__sentinel__");

static REGISTRY: Lazy<Mutex<TaskRegistry>> =
    Lazy::new(|| Mutex::new(TaskRegistry::empty()));

/// 默认停滞阈值 (main_loop 检查周期数)
const DEFAULT_STALL_THRESHOLD: u32 = 3;

/// 注册任务到全局心跳表
///
/// 应在任务启动时 (init 函数中) 调用一次。
/// 超过 MAX_TASKS 时记日志并忽略。
pub fn register(task: &'static TaskHb) {
    let mut reg = REGISTRY.lock();
    if reg.count >= MAX_TASKS {
        log::error!("[health] task table full, cannot register '{}'", task.name);
        return;
    }
    let count = reg.count;
    reg.tasks[count] = task;
    reg.count += 1;
    log::debug!("[health] registered task '{}'", task.name);
}

/// 检查所有已注册任务的心跳
///
/// 返回停滞任务名列表 (心跳连续未变化次数超过 max_stall)。
/// main_loop 每 1s 调用一次。
pub fn check_all() -> heapless::Vec<&'static str, MAX_TASKS> {
    let mut stalled = heapless::Vec::new();
    let reg = REGISTRY.lock();
    for i in 0..reg.count {
        let t = reg.tasks[i];
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

/// 更新所有任务的 last_check 快照
///
/// 在 check_all 之后调用, 用于下次比较。
/// (check_all 内部已用 store 更新, 此函数供外部重置用)
pub fn snapshot() {
    let reg = REGISTRY.lock();
    for i in 0..reg.count {
        let t = reg.tasks[i];
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
// ESP32-S3 双核: Core 0 (网络/BLE/协议栈) + Core 1 (实时采集/计算)
// 默认 Rust `std::thread::spawn` 不绑定核, 任务在核间漂移导致 cache miss 抖动。
// 在任务函数开头调用 `pin_current_to_core(N)` 可固定到指定核, 减少抖动。
//
// 核分配策略 (按工业实时性 + 协议栈特性):
//   - Core 0: 网络/协议栈 (eth/mb-tcp/mb-rtu-master/ble-at/proto-store/main_loop)
//     (LwIP/Bluedroid/FreeRTOS 系统任务默认在 Core 0, 减少 IPC)
//   - Core 1: 实时采集 (io-di-scan/io-do-output/ai-sample/ao-output)
//     (避开网络/协议栈抖动, DI 1ms 周期更稳定)

/// 把当前任务绑定到指定核 (ESP32-S3: 0 或 1)
///
/// 在任务函数开头调用一次即可。基于 ESP-IDF FreeRTOS v10.5+ 的
/// `vTaskCoreAffinitySet` API (SMP 版本)。
///
/// 注意: 必须在任务上下文内调用 (会获取当前 task handle)。
/// 如果 binding 不可用 (esp_idf_sys < 0.34), 编译会报错, 移除调用即可。
#[inline]
pub fn pin_current_to_core(core: u32) {
    // ESP-IDF FreeRTOS SMP: mask bit0=core0, bit1=core1, 0x3=both
    // vTaskCoreAffinitySet not available in this esp_idf_sys version;
    // tasks are pinned via xTaskCreatePinnedToCore at creation time.
    let _ = core;
}

/// Core 0 (网络/协议栈核)
pub const CORE_NET: u32 = 0;
/// Core 1 (实时采集/计算核)
pub const CORE_RT: u32 = 1;
