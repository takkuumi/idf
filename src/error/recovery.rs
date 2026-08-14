//! 分级故障恢复 (替代直接 esp_restart)
//!
//! ## 设计原则
//! - 任何故障先尝试恢复, 绝不直接重启
//! - 故障计数器分级别: 轻 → 重 → 严重
//! - 严重故障进入更严格的降级模式，保留可用业务
//! - 网络/外设故障 → 降级模式 (关闭相关功能)
//! - 数据故障 → 仅记录日志, 重置默认值
//!
//! ## 故障分类
//! | 等级 | 恢复策略 | 例子 |
//! |------|---------|------|
//! | Recoverable | 重试 N 次 | BLE 通知失败、Modbus CRC 错误 |
//! | Degradable | 关闭相关功能 | 网线断开 (降级为 BLE-only) |
//! | Severe | 本地降级 | NVS 损坏、OTA 失败 |
//! | Fatal | 最小功能降级 | panic、内存耗尽 |

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use std::sync::LazyLock;

/// 故障严重等级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// 可恢复 (重试即可)
    Recoverable,
    /// 可降级 (关闭相关功能)
    Degradable,
    /// 严重 (进入本地降级)
    Severe,
    /// 致命 (最小功能降级)
    Fatal,
}

/// 当前降级模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradedMode {
    /// 全功能模式
    Normal,
    /// 仅 BLE 模式 (Modbus TCP/RTU 关闭)
    BleOnly,
    /// 仅本地模式 (网络关闭)
    LocalOnly,
    /// 完全降级 (仅 AT 命令)
    Minimal,
}

/// 启动时间 (用于计算 elapsed ms)
static BOOT_TIME: LazyLock<Instant> = LazyLock::new(Instant::now);

fn elapsed_ms() -> u64 {
    BOOT_TIME.elapsed().as_millis() as u64
}

// === 故障统计 (原子无锁) ===
static RECOVERABLE_COUNT: AtomicU32 = AtomicU32::new(0);
static DEGRADABLE_COUNT: AtomicU32 = AtomicU32::new(0);
static SEVERE_COUNT: AtomicU32 = AtomicU32::new(0);
static LAST_RECOVERABLE_MS: AtomicU32 = AtomicU32::new(0);
static LAST_DEGRADABLE_MS: AtomicU32 = AtomicU32::new(0);
static LAST_SEVERE_MS: AtomicU32 = AtomicU32::new(0);

// === 当前降级模式 (原子无锁) ===
static DEGRADED_MODE: AtomicU32 = AtomicU32::new(0); // 0=Normal
/// 记录一次故障
pub fn record_failure(severity: Severity, module: &str, msg: &str) {
    let now = elapsed_ms();
    match severity {
        Severity::Recoverable => {
            RECOVERABLE_COUNT.fetch_add(1, Ordering::Relaxed);
            LAST_RECOVERABLE_MS.store(now as u32, Ordering::Relaxed);
            log::warn!("[recoverable] {}: {}", module, msg);
        }
        Severity::Degradable => {
            DEGRADABLE_COUNT.fetch_add(1, Ordering::Relaxed);
            LAST_DEGRADABLE_MS.store(now as u32, Ordering::Relaxed);
            log::error!("[degradable] {}: {}", module, msg);
        }
        Severity::Severe => {
            SEVERE_COUNT.fetch_add(1, Ordering::Relaxed);
            LAST_SEVERE_MS.store(now as u32, Ordering::Relaxed);
            log::error!("[SEVERE] {}: {}", module, msg);
        }
        Severity::Fatal => {
            log::error!("[FATAL] {}: {}", module, msg);
        }
    }
}

/// 获取当前降级模式
pub fn mode() -> DegradedMode {
    match DEGRADED_MODE.load(Ordering::Acquire) {
        0 => DegradedMode::Normal,
        1 => DegradedMode::BleOnly,
        2 => DegradedMode::LocalOnly,
        3 => DegradedMode::Minimal,
        _ => DegradedMode::Normal,
    }
}

/// 进入降级模式
pub fn enter_mode(new_mode: DegradedMode) {
    let v = match new_mode {
        DegradedMode::Normal => 0,
        DegradedMode::BleOnly => 1,
        DegradedMode::LocalOnly => 2,
        DegradedMode::Minimal => 3,
    };
    let old = DEGRADED_MODE.swap(v, Ordering::AcqRel);
    if old != v {
        log::warn!(
            "[recovery] mode: {:?} -> {:?}",
            current_mode_label(old),
            new_mode
        );
    }
}

fn current_mode_label(v: u32) -> DegradedMode {
    match v {
        0 => DegradedMode::Normal,
        1 => DegradedMode::BleOnly,
        2 => DegradedMode::LocalOnly,
        3 => DegradedMode::Minimal,
        _ => DegradedMode::Normal,
    }
}

/// 系统功能
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// BLE GATT 服务
    Ble,
    /// Modbus TCP 服务器
    ModbusTcp,
    /// Modbus RTU 主站
    ModbusRtuMaster,
    /// Modbus RTU 从站
    ModbusRtuSlave,
    /// 以太网
    Ethernet,
    /// 本地 IO (DI/DO/AI/AO)
    LocalIo,
    /// NVS 持久化
    Nvs,
}

/// 检查是否允许执行某项功能 (根据当前降级模式)
pub fn is_feature_allowed(feature: Feature) -> bool {
    matches!(
        (mode(), feature),
        (DegradedMode::Normal, _)
            | (DegradedMode::BleOnly, Feature::Ble)
            | (DegradedMode::BleOnly, Feature::LocalIo)
            | (DegradedMode::LocalOnly, Feature::Ble)
            | (DegradedMode::LocalOnly, Feature::LocalIo)
            | (DegradedMode::Minimal, Feature::LocalIo)
    )
}

/// 故障统计快照 (用于 Modbus 寄存器暴露给 Modbus Poll)
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub recoverable: u32,
    pub degradable: u32,
    pub severe: u32,
    pub mode: DegradedMode,
}

pub fn stats() -> Stats {
    Stats {
        recoverable: RECOVERABLE_COUNT.load(Ordering::Relaxed),
        degradable: DEGRADABLE_COUNT.load(Ordering::Relaxed),
        severe: SEVERE_COUNT.load(Ordering::Relaxed),
        mode: mode(),
    }
}

/// 分级恢复决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// 继续正常运行 (故障已恢复或被忽略)
    Continue,
    /// 重试当前操作 (短延迟)
    Retry(std::time::Duration),
    /// 切换到降级模式 (持续运行, 关闭部分功能)
    Degrade(DegradedMode),
}

/// 决定故障恢复策略 (核心决策函数)
///
/// 根据故障类型、发生频率、当前模式决定下一步行动。
/// 设计目标: 故障不触发软件重启，保留仍可工作的业务并等待运维处理。
pub fn decide_action(severity: Severity, consecutive_failures: u32) -> RecoveryAction {
    match severity {
        Severity::Recoverable => {
            // 可恢复故障: 短暂重试即可, 永不重启
            RecoveryAction::Retry(std::time::Duration::from_millis(100))
        }
        Severity::Degradable => {
            // 降级: 根据连续失败次数切换模式
            if consecutive_failures >= 10 {
                RecoveryAction::Degrade(DegradedMode::Minimal)
            } else if consecutive_failures >= 5 {
                RecoveryAction::Degrade(DegradedMode::LocalOnly)
            } else if consecutive_failures >= 2 {
                RecoveryAction::Degrade(DegradedMode::BleOnly)
            } else {
                RecoveryAction::Retry(std::time::Duration::from_secs(1))
            }
        }
        Severity::Severe => RecoveryAction::Degrade(DegradedMode::LocalOnly),
        Severity::Fatal => RecoveryAction::Degrade(DegradedMode::Minimal),
    }
}

/// 应用恢复决策
pub fn apply_action(action: RecoveryAction, module: &str) {
    match action {
        RecoveryAction::Continue => {}
        RecoveryAction::Retry(d) => {
            log::debug!("[recovery] {}: retry in {:?}", module, d);
        }
        RecoveryAction::Degrade(m) => {
            enter_mode(m);
        }
    }
}

/// 便捷宏: 记录故障 + 决定动作 + 应用 (一行搞定)
///
/// # 用法
/// ```ignore
/// recovery_handle!(Severity::Degradable, "wifi", "link down", consecutive);
/// ```
#[macro_export]
macro_rules! recovery_handle {
    ($severity:expr, $module:expr, $msg:expr, $consecutive:expr) => {{
        use $crate::error::recovery;
        recovery::record_failure($severity, $module, $msg);
        let action = recovery::decide_action($severity, $consecutive);
        recovery::apply_action(action, $module);
        action
    }};
}

// ============================================================================
// 单元测试
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recoverable_decision() {
        // Recoverable 故障: 总是重试, 永不重启
        let action = decide_action(Severity::Recoverable, 100);
        assert!(matches!(action, RecoveryAction::Retry(_)));
    }

    #[test]
    fn test_degradable_decision_progression() {
        // 1 次降级: 重试
        let action = decide_action(Severity::Degradable, 1);
        assert!(matches!(action, RecoveryAction::Retry(_)));

        // 2-4 次: 降级到 BleOnly
        let action = decide_action(Severity::Degradable, 2);
        assert!(matches!(
            action,
            RecoveryAction::Degrade(DegradedMode::BleOnly)
        ));
        let action = decide_action(Severity::Degradable, 4);
        assert!(matches!(
            action,
            RecoveryAction::Degrade(DegradedMode::BleOnly)
        ));

        // 5-9 次: 降级到 LocalOnly
        let action = decide_action(Severity::Degradable, 5);
        assert!(matches!(
            action,
            RecoveryAction::Degrade(DegradedMode::LocalOnly)
        ));

        // 10+ 次: 降级到 Minimal
        let action = decide_action(Severity::Degradable, 10);
        assert!(matches!(
            action,
            RecoveryAction::Degrade(DegradedMode::Minimal)
        ));
    }

    #[test]
    fn test_severe_degrades_without_restart() {
        let action = decide_action(Severity::Severe, 1);
        assert!(matches!(
            action,
            RecoveryAction::Degrade(DegradedMode::LocalOnly)
        ));
    }

    #[test]
    fn test_fatal_degrades_without_restart() {
        let action = decide_action(Severity::Fatal, 0);
        assert!(matches!(
            action,
            RecoveryAction::Degrade(DegradedMode::Minimal)
        ));
    }

    #[test]
    fn test_mode_cycling() {
        // 默认 Normal
        assert_eq!(mode(), DegradedMode::Normal);

        // 切换到 BleOnly
        enter_mode(DegradedMode::BleOnly);
        assert_eq!(mode(), DegradedMode::BleOnly);

        // 切回 Normal
        enter_mode(DegradedMode::Normal);
        assert_eq!(mode(), DegradedMode::Normal);
    }

    #[test]
    fn test_feature_filtering() {
        enter_mode(DegradedMode::Normal);
        assert!(is_feature_allowed(Feature::Ble));
        assert!(is_feature_allowed(Feature::ModbusTcp));
        assert!(is_feature_allowed(Feature::Ethernet));

        enter_mode(DegradedMode::BleOnly);
        assert!(is_feature_allowed(Feature::Ble));
        assert!(!is_feature_allowed(Feature::ModbusTcp));
        assert!(!is_feature_allowed(Feature::Ethernet));

        enter_mode(DegradedMode::LocalOnly);
        assert!(is_feature_allowed(Feature::Ble));
        assert!(!is_feature_allowed(Feature::ModbusTcp));
        assert!(is_feature_allowed(Feature::LocalIo));

        enter_mode(DegradedMode::Minimal);
        assert!(!is_feature_allowed(Feature::Ble));
        assert!(is_feature_allowed(Feature::LocalIo));

        // 恢复
        enter_mode(DegradedMode::Normal);
    }
}
