//! 应用层数据总线 - 细粒度无锁分片架构 (Phase 2, legacy `Spin<Bus>` 已退役)
//!
//! 三段独立无锁状态:
//!
//! | 子模块 | 内容 | 大小 | 实现 | 访问频率 |
//! |-------|------|------|------|---------|
//! | [`io_state`] | di/do_/ai/ao/sys | ~80B | 无锁 (AtomicBits64 / Atomic*) | 5-100ms 高频 |
//! | [`storage_state`] | proto/device_text/holding_buf | ~11KB | RCU 无锁 | 偶尔 |
//! | [`config_state`] | cfg/device_config | ~1.1KB | RCU 无锁 | 偶尔 |
//!
//! `proto.status` 状态机 (commit=1 / reload=2 / failed=3) 由
//! [`storage_state::PROTO_STATUS_ATOMIC`] 独立承担 — 不进 RCU 快照, 高频写无需克隆 11KB.
//!
//! Modbus 寄存器访问统一由 [`backends`] 自由函数承担 (读 RCU / 写 RCU RMW + atomic),
//! 不再有 `Bus` / `lock_timeout()`. 见 `docs/system/LEGACY_BUS_RETIRE.md` 阶段 D.
//!
//! ## 优势
//! - I/O 任务完全无锁, 不会被 Modbus 慢任务阻塞
//! - Modbus 多寄存器写只持 RCU (原子 swap), 不再持大锁 N 次
//! - 配置读写零锁 (RCU), Modbus 高频读不阻塞
pub mod config_state;
pub mod buffer_pool;
pub mod rcu;
pub mod event_bus;
pub mod io_global;
pub mod io_state;
pub mod storage_state;
pub mod backends;

// 仅重导出被 crate 内部实际使用的符号. 模块本身 (config_state / storage_state /
// io_state / backends) 的所有 API 也可通过 `crate::bus::<mod>::...` 完整路径访问.
pub use config_state::config_read;
pub use io_global::IO;
pub use event_bus::{send_event, IoEvent};
pub use storage_state::proto_status;
