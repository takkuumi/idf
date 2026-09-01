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
//! - 任务表用固定数组 (最多 24 个任务)
//! - 不依赖 panic=unwind, 不用 catch_unwind

use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

/// 将 ESP-IDF 复位原因转换为稳定的诊断名称。
/// 数字值直接来自 `esp_reset_reason_t`，未知值保留十六进制由调用方输出。
pub fn reset_reason_name(reason: u8) -> &'static str {
    match reason {
        0 => "UNKNOWN",
        1 => "POWERON",
        2 => "EXTERNAL",
        3 => "SOFTWARE",
        4 => "PANIC",
        5 => "INT_WDT",
        6 => "TASK_WDT",
        7 => "WDT",
        8 => "DEEPSLEEP",
        9 => "BROWNOUT",
        10 => "SDIO",
        11 => "USB",
        12 => "JTAG",
        13 => "RTC_WDT_SYS",
        14 => "RTC_WDT_CPU",
        15 => "RTC_WDT_RTC",
        _ => "UNKNOWN",
    }
}

/// 最大监控任务数
const MAX_TASKS: usize = 24;

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
    /// 一次性任务完成后设为 true, check_all 跳过 (避免误报停滞)
    completed: core::sync::atomic::AtomicBool,
    /// 配置的任务栈字节数；0 表示该心跳不代表独立任务。
    stack_size: AtomicU32,
    /// 任务首次 tick 时捕获的 FreeRTOS TaskHandle_t。
    task_handle: AtomicUsize,
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
            completed: core::sync::atomic::AtomicBool::new(false),
            stack_size: AtomicU32::new(0),
            task_handle: AtomicUsize::new(0),
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
            completed: core::sync::atomic::AtomicBool::new(false),
            stack_size: AtomicU32::new(0),
            task_handle: AtomicUsize::new(0),
        }
    }

    /// 任务 loop 中调用, 递增心跳计数
    pub fn tick(&self) {
        if self.stack_size.load(Ordering::Relaxed) != 0
            && self.task_handle.load(Ordering::Relaxed) == 0
        {
            let handle = unsafe { esp_idf_sys::xTaskGetCurrentTaskHandle() } as usize;
            if handle != 0 {
                self.task_handle.store(handle, Ordering::Release);
            }
        }
        self.counter.fetch_add(1, Ordering::Relaxed);
    }

    /// 一次性任务完成后调用, 标记该任务为已完成.
    /// 已完成任务不再被 check_all 检查 (避免误报停滞).
    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::Release);
        // 最后再 tick 一次, 保证 check_all 的 last_check 更新
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

/// 注册真实常驻任务并绑定其配置栈大小，供运行时高水位采样。
pub fn register_with_stack(task: &'static TaskHb, stack_size: usize) {
    task.stack_size.store(stack_size as u32, Ordering::Release);
    register(task);
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
        // LOOP9: 一次性任务完成后跳过, 避免误报停滞触发强制重启
        if t.completed.load(Ordering::Acquire) {
            continue;
        }
        let cur = t.counter.load(Ordering::Relaxed);
        let last = t.last_check.load(Ordering::Relaxed);
        let threshold = t.max_stall.load(Ordering::Relaxed);
        let threshold = if threshold == 0 {
            DEFAULT_STALL_THRESHOLD
        } else {
            threshold
        };

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
/// LOOP9: 幂等 — 重复订阅同一任务返回 ESP_ERR_INVALID_STATE, 视为成功 (已订阅).
pub fn subscribe_wdt() {
    // esp_task_wdt_add(NULL) 表示添加当前任务
    let r = unsafe { esp_idf_sys::esp_task_wdt_add(std::ptr::null_mut()) };
    if r == 0 || r == esp_idf_sys::ESP_ERR_INVALID_STATE {
        // 0 = 新订阅成功; INVALID_STATE = 已订阅 (幂等)
        log::debug!("[health] current task subscribed to WDT (r=0x{:08X})", r);
    } else {
        log::warn!("[health] esp_task_wdt_add failed: 0x{:08X}", r);
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
//   Core 0 — 网络/协议栈 (LwIP/W5500/mb-rtu/ble-mesh/main-loop)
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
#[deprecated(
    since = "0.2.0",
    note = "use set_next_thread_core() before thread::spawn instead"
)]
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
            i,
            t.name,
            t.max_stall.load(Ordering::Relaxed)
        );
    }
}

/// 汇总所有真实用户任务的历史最小剩余栈。
///
/// ESP-IDF 的 `uxTaskGetStackHighWaterMark2` 返回字节，不是标准 FreeRTOS 文档中的 word。
/// 健康时只输出一行汇总，避免串口逐任务打印阻塞 main-loop；低水位任务仍逐项告警。
/// 只告警和记录，绝不因低水位主动重启设备。
pub fn print_stack_watermarks() {
    let n = REGISTRY.count.load(Ordering::Acquire);
    let mut sampled = 0u32;
    let mut low = 0u32;
    let mut min_free = u32::MAX;
    let mut min_free_task = "-";
    let mut max_used_pct = 0u32;
    let mut max_used_task = "-";

    for i in 0..n {
        let ptr = REGISTRY.tasks[i].load(Ordering::Acquire);
        if ptr.is_null() {
            continue;
        }
        let task: &'static TaskHb = unsafe { &*ptr };
        let size = task.stack_size.load(Ordering::Acquire);
        let handle = task.task_handle.load(Ordering::Acquire);
        if size == 0 || handle == 0 {
            continue;
        }
        let free = unsafe {
            esp_idf_sys::uxTaskGetStackHighWaterMark2(handle as esp_idf_sys::TaskHandle_t) as u32
        };
        let used_pct = size.saturating_sub(free).saturating_mul(100) / size;
        #[cfg(debug_assertions)]
        log::info!(
            "[stack-detail] task={} size={}B min_free={}B used={}%",
            task.name,
            size,
            free,
            used_pct
        );
        sampled += 1;
        if free < min_free {
            min_free = free;
            min_free_task = task.name;
        }
        if used_pct > max_used_pct {
            max_used_pct = used_pct;
            max_used_task = task.name;
        }
        if free < 1024 || used_pct >= 90 {
            low += 1;
            log::error!(
                "[stack] LOW task={} size={}B min_free={}B used={}%",
                task.name,
                size,
                free,
                used_pct
            );
        }
    }

    if sampled == 0 {
        log::warn!("[stack] no task watermark available");
    } else {
        log::info!(
            "[stack] tasks={} low={} min_free={}B min_task={} max_used={}% max_task={}",
            sampled,
            low,
            min_free,
            min_free_task,
            max_used_pct,
            max_used_task
        );
    }
}

#[cfg(test)]
mod tests {
    use super::reset_reason_name;

    #[test]
    fn reset_reason_names_match_esp_idf_values() {
        assert_eq!(reset_reason_name(1), "POWERON");
        assert_eq!(reset_reason_name(4), "PANIC");
        assert_eq!(reset_reason_name(6), "TASK_WDT");
        assert_eq!(reset_reason_name(9), "BROWNOUT");
        assert_eq!(reset_reason_name(0xFF), "UNKNOWN");
    }
}
