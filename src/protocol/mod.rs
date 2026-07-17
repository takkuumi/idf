//! 通信协议插件化 (通信协议抽象层)
//!
//! 统一 Modbus RTU/TCP、BLE Mesh 等通信协议的启动/停止/状态查询接口。
//! 上层通过 [`ProtocolRegistry`] 管理所有协议, 无需关心具体实现。
//!
//! # 设计目标
//!
//! 1. **插件化**: 新增协议只需实现 `Protocol` trait 并注册到 Registry
//! 2. **统一管理**: 启动/停止/状态查询通过统一接口, main.rs 无需 if-else 分支
//! 3. **运行时监控**: 支持查询各协议运行状态 (运行中/错误计数/运行时长)
//!
//! # 使用方式
//!
//! ```no_run
//! use crate::protocol::{ProtocolRegistry, BleMeshProtocol, ModbusRtuProtocol};
//!
//! let mut protocols = ProtocolRegistry::new();
//! protocols.register(Box::new(BleMeshProtocol::new(hal.clone())));
//! protocols.register(Box::new(ModbusRtuProtocol::new(hal.clone())));
//! protocols.start_all()?;
//! ```

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

use crate::error::AppResult;

// Arc + Hal 仅被需要 Arc<Hal> 的适配器使用 (ModbusRtuProtocol)
#[cfg(feature = "modbus-rtu")]
use std::sync::Arc;
#[cfg(feature = "modbus-rtu")]
use crate::hal::Hal;

// ============================================================================
// Protocol trait
// ============================================================================

/// 通信协议抽象 trait
///
/// 所有通信协议 (Modbus RTU/TCP, BLE Mesh, 未来 MQTT/OPC-UA 等) 实现此 trait,
/// 通过 [`ProtocolRegistry`] 统一管理。
///
/// # 线程安全
///
/// 实现需满足 `Send + Sync`, 内部状态使用原子操作或 Mutex 保护,
/// 因为 `start(&self)` 接受不可变引用 (适配器模式, 无需 `&mut self`)。
pub trait Protocol: Send + Sync {
    /// 协议名称 (如 "modbus-rtu", "modbus-tcp", "ble-mesh")
    fn name(&self) -> &str;

    /// 启动协议任务 (spawn 线程, 立即返回)
    fn start(&self) -> AppResult<()>;

    /// 停止协议任务 (标记停止状态)
    ///
    /// 注意: ESP-IDF 任务通常是无限循环, stop 仅标记停止状态,
    /// 实际线程退出需任务内部检查停止标志 (当前实现为 best-effort)。
    fn stop(&self) -> AppResult<()>;

    /// 是否正在运行
    fn is_running(&self) -> bool;

    /// 运行时统计
    fn stats(&self) -> ProtocolStats;
}

// ============================================================================
// ProtocolStats
// ============================================================================

/// 协议运行时统计
///
/// 用于 Modbus 寄存器映射或调试日志查询各协议运行状态。
#[derive(Debug, Clone)]
pub struct ProtocolStats {
    /// 协议名称
    pub name: String,
    /// 是否正在运行
    pub running: bool,
    /// 运行时长 (秒, 0 = 未启动)
    pub uptime_s: u64,
    /// 错误计数
    pub error_count: u32,
}

// ============================================================================
// ProtocolState (适配器共享的状态管理)
// ============================================================================

/// 协议运行状态管理 (适配器嵌入此结构共享状态管理逻辑)
///
/// 使用原子操作, 支持 `&self` 修改 (Protocol::start(&self) 不需要 &mut self)。
struct ProtocolState {
    running: AtomicBool,
    started_at: Mutex<Option<Instant>>,
    error_count: AtomicU32,
}

impl ProtocolState {
    fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            started_at: Mutex::new(None),
            error_count: AtomicU32::new(0),
        }
    }

    fn mark_started(&self) {
        self.running.store(true, Ordering::SeqCst);
        *self.started_at.lock() = Some(Instant::now());
    }

    fn mark_stopped(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    fn record_error(&self) {
        self.error_count.fetch_add(1, Ordering::SeqCst);
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn uptime_s(&self) -> u64 {
        self.started_at
            .lock()
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0)
    }

    fn error_count(&self) -> u32 {
        self.error_count.load(Ordering::SeqCst)
    }

    /// 生成 ProtocolStats (适配器调用此方法实现 stats())
    fn stats(&self, name: &str) -> ProtocolStats {
        ProtocolStats {
            name: name.into(),
            running: self.is_running(),
            uptime_s: self.uptime_s(),
            error_count: self.error_count(),
        }
    }
}

// ============================================================================
// ProtocolRegistry
// ============================================================================

/// 协议注册表
///
/// 管理所有已注册的通信协议, 支持批量启动/停止/状态查询。
///
/// # 示例
///
/// ```no_run
/// let mut registry = ProtocolRegistry::new();
/// registry.register(Box::new(ModbusRtuProtocol::new(hal.clone())));
/// registry.start_all()?;
/// let stats = registry.stats(); // 查询所有协议状态
/// ```
pub struct ProtocolRegistry {
    protocols: Vec<Box<dyn Protocol>>,
}

impl ProtocolRegistry {
    pub fn new() -> Self {
        Self {
            protocols: Vec::new(),
        }
    }

    /// 注册协议
    pub fn register(&mut self, protocol: Box<dyn Protocol>) {
        log::info!("[protocol] registered: {}", protocol.name());
        self.protocols.push(protocol);
    }

    /// 启动所有未运行的协议
    ///
    /// 某个协议启动失败仅记日志, 不影响其它协议启动 (尽力而为策略)。
    pub fn start_all(&self) -> AppResult<()> {
        for p in &self.protocols {
            if p.is_running() {
                log::warn!("[protocol] {} already running, skip", p.name());
                continue;
            }
            match p.start() {
                Ok(()) => log::info!("[protocol] {} started", p.name()),
                Err(e) => log::error!("[protocol] {} start failed: {}", p.name(), e),
            }
        }
        Ok(())
    }

    /// 停止所有协议
    pub fn stop_all(&self) -> AppResult<()> {
        for p in &self.protocols {
            if let Err(e) = p.stop() {
                log::warn!("[protocol] {} stop failed: {}", p.name(), e);
            }
        }
        Ok(())
    }

    /// 按名称查找协议
    pub fn find(&self, name: &str) -> Option<&dyn Protocol> {
        self.protocols
            .iter()
            .find(|p| p.name() == name)
            .map(|p| p.as_ref())
    }

    /// 列出所有协议名称
    pub fn list(&self) -> Vec<&str> {
        self.protocols.iter().map(|p| p.name()).collect()
    }

    /// 收集所有协议的运行时统计
    pub fn stats(&self) -> Vec<ProtocolStats> {
        self.protocols.iter().map(|p| p.stats()).collect()
    }
}

impl Default for ProtocolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 协议适配器
// ============================================================================

/// Modbus RTU 协议适配器
///
/// 封装 `modbus::start_rtu()`, 通过 Protocol trait 统一管理。
/// 持有 `Arc<Hal>` 供 RS485 + Modbus RTU Master/Slave 任务使用。
#[cfg(feature = "modbus-rtu")]
pub struct ModbusRtuProtocol {
    hal: Arc<Hal>,
    state: ProtocolState,
}

#[cfg(feature = "modbus-rtu")]
impl ModbusRtuProtocol {
    pub fn new(hal: Arc<Hal>) -> Self {
        Self {
            hal,
            state: ProtocolState::new(),
        }
    }
}

#[cfg(feature = "modbus-rtu")]
impl Protocol for ModbusRtuProtocol {
    fn name(&self) -> &str {
        "modbus-rtu"
    }

    fn start(&self) -> AppResult<()> {
        crate::modbus::start_rtu(self.hal.clone())?;
        self.state.mark_started();
        Ok(())
    }

    fn stop(&self) -> AppResult<()> {
        self.state.mark_stopped();
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.state.is_running()
    }

    fn stats(&self) -> ProtocolStats {
        self.state.stats(self.name())
    }
}

/// Modbus TCP 协议适配器
///
/// 封装 `modbus::start_tcp()`, 监听 502 端口。
#[cfg(feature = "modbus-tcp")]
pub struct ModbusTcpProtocol {
    state: ProtocolState,
}

#[cfg(feature = "modbus-tcp")]
impl ModbusTcpProtocol {
    pub fn new() -> Self {
        Self {
            state: ProtocolState::new(),
        }
    }
}

#[cfg(feature = "modbus-tcp")]
impl Default for ModbusTcpProtocol {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "modbus-tcp")]
impl Protocol for ModbusTcpProtocol {
    fn name(&self) -> &str {
        "modbus-tcp"
    }

    fn start(&self) -> AppResult<()> {
        crate::modbus::start_tcp()?;
        self.state.mark_started();
        Ok(())
    }

    fn stop(&self) -> AppResult<()> {
        self.state.mark_stopped();
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.state.is_running()
    }

    fn stats(&self) -> ProtocolStats {
        self.state.stats(self.name())
    }
}

