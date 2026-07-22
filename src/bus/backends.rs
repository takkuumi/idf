//! Modbus 后端自由函数 — 完全无锁 (legacy `Spin<Bus>` 已退役)
//!
//! FC=01/02/03/04 读端 + FC=05/06/0F/10 写端全部经此模块, 不再过 `Spin<Bus>`:
//! - DI / DO / AI / AO / sys → `bus::IO` (原子 / `AtomicBits64`), 真无锁
//! - proto / device_text / holding_buf → `STORAGE` (`Rcu<StorageSnapshot>`) RCU RMW
//! - cfg / device_config → `CONFIG` (`Rcu<ConfigSnapshot>`) RCU RMW
//! - `proto.status` 状态机 → 独立 `AtomicU8` (`PROTO_STATUS_ATOMIC`), 不进快照 (高频写)
//!
//! 见 `docs/system/LEGACY_BUS_RETIRE.md` 阶段 B/C/D (退役完成).
//!
//! # 写者并发
//! `STORAGE` / `CONFIG` 的 `Rcu::write` 假定串行执笔 (单 Modbus 任务串 / 单 DeviceActor).
//! 多写者并发 push 同一 retire 槽可能丢一个未回收快照 (泄漏 1 帧, 极罕见) — 见 `rcu.rs`.

use crate::config::{hw_version, regs};
use crate::error::recovery::{self, DegradedMode};

use super::io_global::IO;
use super::storage_state::{storage_read, proto_status, StorageSnapshot, STORAGE};
use super::config_state::{config_read, ConfigSnapshot};

// ----------------------------------------------------------------------------
// DI / DO (AtomicBits64 via IO)
// ----------------------------------------------------------------------------

/// 读线圈 (DO, FC=01): 地址语义同 `Bus::read_coil`.
#[inline]
pub fn read_coil(addr: u16) -> Option<bool> {
    if addr >= regs::COIL_DO_BASE && addr < regs::COIL_DO_END {
        let ch = (addr - regs::COIL_DO_BASE) as usize;
        return Some(IO.do_.get_bit(ch));
    }
    if addr < regs::DISC_DI_COUNT {
        return Some(IO.di.get_bit(addr as usize));
    }
    None
}

/// 读离散输入 (DI, FC=02).
#[inline]
pub fn read_disc(addr: u16) -> Option<bool> {
    if addr < regs::DISC_DI_COUNT {
        Some(IO.di.get_bit(addr as usize))
    } else {
        None
    }
}

// ----------------------------------------------------------------------------
// AI / AO / sys / recovery / ringlog / health (无锁原子 + 模块自有 API)
// ----------------------------------------------------------------------------

/// 读输入寄存器 (AI + 系统状态 + 故障统计, FC=04). 地址语义同 `Bus::read_input_reg`.
pub fn read_input_reg(addr: u16) -> Option<u16> {
    if addr >= regs::INREG_AI_BASE && addr < regs::INREG_AI_BASE + regs::INREG_AI_COUNT {
        let idx = (addr - regs::INREG_AI_BASE) as usize;
        return Some(IO.ai.get_raw(idx));
    }
    if addr >= regs::INREG_AI_STATUS_BASE
        && addr < regs::INREG_AI_STATUS_BASE + regs::INREG_AI_COUNT
    {
        let idx = (addr - regs::INREG_AI_STATUS_BASE) as usize;
        return Some(IO.ai.get_scaled(idx));
    }
    match addr {
        regs::INREG_QI_COUNT => {
            Some(((hw_version::DO_COUNT as u16) << 8) | (hw_version::DI_COUNT as u16))
        }
        regs::INREG_ADC485 => Some((regs::INREG_AI_COUNT << 8) | 2),
        regs::INREG_FW_VER => Some(IO.sys.get_fw_version()),
        regs::INREG_FW_DATE => {
            // 从 CONFIG 快照读取 fw_date (Android metuory 期望: 显示 fw.fw_date 后缀)
            if let Some(cs) = config_read() {
                Some(cs.cfg.fw_date)
            } else {
                Some(0x0615) // fallback
            }
        }
        regs::INREG_RECOV_RECOVERABLE => {
            let s = recovery::stats();
            Some(s.recoverable.min(0xFFFF) as u16)
        }
        regs::INREG_RECOV_DEGRADABLE => {
            let s = recovery::stats();
            Some(s.degradable.min(0xFFFF) as u16)
        }
        regs::INREG_RECOV_SEVERE => {
            let s = recovery::stats();
            Some(s.severe.min(0xFFFF) as u16)
        }
        regs::INREG_RECOV_MODE => Some(match recovery::mode() {
            DegradedMode::Normal => 0,
            DegradedMode::BleOnly => 1,
            DegradedMode::LocalOnly => 2,
            DegradedMode::Minimal => 3,
        }),
        regs::INREG_RECOV_BLE_DROPS => {
            let drops = crate::ble_at::binary_tx_drops();
            Some(drops.min(0xFFFF) as u16)
        }
        regs::INREG_RINGLOG_COUNT => {
            let ring = crate::error::ringlog::RING_LOG.lock();
            Some(ring.len().min(0xFFFF) as u16)
        }
        regs::INREG_RINGLOG_WRITES => {
            let cnt = crate::error::ringlog::LOG_WRITE_COUNT
                .load(std::sync::atomic::Ordering::Acquire);
            Some((cnt & 0xFFFF) as u16)
        }
        _ => None,
    }
}

// ----------------------------------------------------------------------------
// Holding (PROTOCOL_BASE / DEVICE_TEXT / HOLD_CFG / ProtoStatus) — RCU 读
// ----------------------------------------------------------------------------

/// 读保持寄存器 (FC=03). 等价 `Bus::read_hold_reg`, 但走 `STORAGE`/`CONFIG` RCU.
pub fn read_hold_reg(addr: u16) -> Option<u16> {
    // 1. device_text (5000..=6999): 来自 STORAGE (Rcu 主存)
    if addr >= regs::DEVICE_TEXT_BASE && addr <= regs::DEVICE_TEXT_END {
        let idx = (addr - regs::DEVICE_TEXT_BASE) as usize;
        return storage_read().map(|s| s.device_text[idx]);
    }
    // 2. CFG / device_config / holding_buf (0x0880..=0x107F)
    if addr >= regs::HOLD_CFG_BASE && addr <= regs::HOLD_CFG_END {
        // 2a. device_config 子区 (2300..<2400): 来自 CONFIG.device_config
        if addr >= 2300 && addr < 2400 {
            // 先尝试 CONFIG 快照内的 device_config 表
            if let Some(cs) = config_read() {
                if let Some(v) = cs.device_config.read_reg(addr) {
                    return Some(v);
                }
            }
        }
        // 2b. SystemConfig 子寄存器 (_CFG_BASE..=_CFG_END 子集): 来自 CONFIG.cfg
        if let Some(cs) = config_read() {
            if let Some(v) = cs.cfg.read_reg(addr) {
                return Some(v);
            }
        }
        // 2c. HOLD_PXX 残余 (0x0880..=0x107F 中 CFG 未覆盖部分): 来自 STORAGE.holding_buf
        let idx = (addr - regs::HOLD_PXX_BASE) as usize;
        if idx < regs::HOLD_PXX_COUNT {
            return storage_read().map(|s| s.holding_buf[idx]);
        }
        return Some(0);
    }
    // 3. PROTO 区 (0x4000..<PROTO_END): 来自 STORAGE.proto
    if addr >= regs::PROTO_BASE && addr < regs::PROTO_END {
        let idx = (addr - regs::PROTO_BASE) as usize;
        return storage_read().map(|s| s.proto.data[idx]);
    }
    // 4. 错误环日志最近 8 条 (0x0887..=0x08A6): 来自 ringlog
    if addr >= regs::INREG_RINGLOG_BASE && addr < regs::INREG_RINGLOG_BASE + 32 {
        let entry_idx = ((addr - regs::INREG_RINGLOG_BASE) / 4) as usize;
        let field_idx = (addr - regs::INREG_RINGLOG_BASE) % 4;
        let ring = crate::error::ringlog::RING_LOG.lock();
        let entries = ring.entries();
        if entry_idx < entries.len() {
            let entry = &entries[entry_idx];
            return Some(match field_idx {
                0 => (entry.timestamp_ms & 0xFFFF) as u16,
                1 => ((entry.timestamp_ms >> 16) & 0xFFFF) as u16,
                2 => entry.code,
                3 => (entry.context & 0xFFFF) as u16,
                _ => 0,
            });
        }
        return Some(0);
    }
    // 5. Proto 控制字 (PROTO_COMMIT..=PROTO_MAGIC)
    match addr {
        regs::PROTO_COMMIT => Some(0),
        regs::PROTO_RELOAD => Some(0),
        regs::PROTO_VERSION => storage_read().map(|s| s.proto.version),
        regs::PROTO_LENGTH => storage_read().map(|s| s.proto.length),
        regs::PROTO_STATUS => Some(proto_status() as u16),
        regs::PROTO_MAGIC => Some(crate::device::PROTO_MAGIC),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_coil_out_of_range() {
        assert!(read_coil(0xFFFF).is_none());
    }

    #[test]
    fn read_input_reg_unknown() {
        assert!(read_input_reg(0x0001).is_none());
    }
}

// ----------------------------------------------------------------------------
// 阶段 B 写端: RCU read-modify-write + AtomicU8 proto.status
// ----------------------------------------------------------------------------

use crate::device::system_config::{SystemConfig, WriteResult};

/// 克隆当前 STORAGE 快照 (无 Arc, 拿到独立可改 T).
#[inline]
fn storage_clone() -> StorageSnapshot {
    storage_read()
        .map(|a| (*a).clone())
        .unwrap_or_else(StorageSnapshot::new)
}

/// 克隆当前 CONFIG 快照.
#[inline]
fn config_clone() -> ConfigSnapshot {
    config_read()
        .map(|a| (*a).clone())
        .unwrap_or_else(ConfigSnapshot::new)
}

/// 把 snapshot 的 proto.status 字段与 atomic 同步 (每次 RCU RMW 回写前调用).
#[inline]
fn sync_proto_status(snap: &mut StorageSnapshot) {
    snap.proto.status = super::storage_state::proto_status();
}

/// 写保持寄存器 (FC=06/16), 等价 `Bus::write_hold_reg` 但走 RCU RMW + atomics, 无 `Spin`.
pub fn write_hold_reg(addr: u16, value: u16) -> bool {
    // 1. device_text (5000..=6999): STORAGE RMW + NVS persist
    //    Android 1.0.78 WRITE_DEVICE_TEXT_COUNT (0xB5) + WRITE_DEVICE_TEXT_DATA (0xB7)
    //    通过 Modbus FC=10 写入, 我们写后立即持久化以保证工业可靠性.
    if addr >= regs::DEVICE_TEXT_BASE && addr <= regs::DEVICE_TEXT_END {
        let idx = (addr - regs::DEVICE_TEXT_BASE) as usize;
        let mut snap = storage_clone();
        snap.device_text[idx] = value;
        sync_proto_status(&mut snap);
        STORAGE.write(snap);
        // 触发设备文本 NVS 持久化 (Android 写入后立即落盘, 复位保留)
        crate::device::request_save_device_text();
        return true;
    }
    // 2. device_config 子区 (2300..<2400): CONFIG RMW
    if addr >= 2300 && addr < 2400 {
        let mut cs = config_clone();
        if cs.device_config.write_reg(addr, value) {
            super::config_state::CONFIG.write(cs);
            return true;
        }
        return false;
    }
    // 3. HOLD_CFG_BASE..=HOLD_CFG_END: CFG + holding_buf
    //
    // WriteResult 语义 (见 device::system_config::WriteResult):
    // - Ok       : 仅 RCU, 不持久化 (诊断/只读寄存器)
    // - Persist  : RCU + NVS (用户可编辑但不需要重启, e.g. SN / PLACE / RS485)
    // - Apply    : RCU + NVS + cfg_version++ (网络/BLE 等需要重新初始化外设)
    // - Reset    : 恢复出厂 + NVS + apply_config
    // - NotFound : 地址不在本配置区, 落 holding_buf 兜底
    if addr >= regs::HOLD_CFG_BASE && addr <= regs::HOLD_CFG_END {
        let mut cs = config_clone();
        match cs.cfg.write_reg(addr, value) {
            WriteResult::Ok => {
                super::config_state::CONFIG.write(cs);
                return true;
            }
            WriteResult::Persist => {
                // 写 RCU + 触发 NVS 持久化. 不增 cfg_version (运行时不需要重新初始化).
                super::config_state::CONFIG.write(cs);
                crate::device::request_apply_config();
                return true;
            }
            WriteResult::Apply => {
                cs.cfg.cfg_version = cs.cfg.cfg_version.wrapping_add(1);
                super::config_state::CONFIG.write(cs);
                crate::device::request_apply_config();
                return true;
            }
            WriteResult::Reset => {
                cs.cfg = SystemConfig::defaults();
                super::config_state::CONFIG.write(cs);
                crate::device::request_apply_config();
                return true;
            }
            WriteResult::NotFound => {
                drop(cs);
                let mut snap = storage_clone();
                let idx = (addr - regs::HOLD_PXX_BASE) as usize;
                if idx < regs::HOLD_PXX_COUNT {
                    snap.holding_buf[idx] = value;
                    sync_proto_status(&mut snap);
                    STORAGE.write(snap);
                    return true;
                }
                return false;
            }
        }
    } else if addr >= regs::PROTO_BASE && addr < regs::PROTO_END {
        let mut snap = storage_clone();
        let idx = (addr - regs::PROTO_BASE) as usize;
        snap.proto.data[idx] = value;
        snap.proto.dirty = true;
        sync_proto_status(&mut snap);
        STORAGE.write(snap);
        return true;
    } else {
        match addr {
            regs::PROTO_COMMIT => {
                if value == 0xC5C5 {
                    super::storage_state::proto_status_set(1);
                    crate::device::request_commit();
                }
                true
            }
            regs::PROTO_RELOAD => {
                if value == 0xA5A5 {
                    super::storage_state::proto_status_set(2);
                    crate::device::request_reload();
                }
                true
            }
            regs::PROTO_VERSION => {
                let mut snap = storage_clone();
                snap.proto.version = value;
                snap.proto.dirty = true;
                sync_proto_status(&mut snap);
                STORAGE.write(snap);
                true
            }
            regs::PROTO_LENGTH => {
                let mut snap = storage_clone();
                snap.proto.length = value;
                snap.proto.dirty = true;
                sync_proto_status(&mut snap);
                STORAGE.write(snap);
                true
            }
            _ => false,
        }
    }
}

/// 写线圈 (FC=05/0F): DO 段原子 IO.do_, 其它 (DI) 不可写, false.
pub fn write_coil(addr: u16, value: bool) -> bool {
    if addr >= regs::COIL_DO_BASE && addr < regs::COIL_DO_END {
        let ch = (addr - regs::COIL_DO_BASE) as usize;
        super::io_global::IO.do_.set_bit(ch, value);
        return true;
    }
    false
}

/// 修改 SystemConfig 部分字段后回写 CONFIG RCU. 用于 w5500 DHCP 写回等单写者场景.
pub fn config_modify<F: FnOnce(&mut SystemConfig)>(f: F) {
    config_modify_with_result(|cfg| f(cfg));
}

/// 同 `config_modify`, 但把 closure 的返回值传出 (用于 AT 命令需要拿到
/// `WriteResult` 等场景). RCU RMW: clone 快照 → mutate cfg → 原子替换.
pub fn config_modify_with_result<F, R>(f: F) -> R
where
    F: FnOnce(&mut SystemConfig) -> R,
{
    let mut cs = config_clone();
    let r = f(&mut cs.cfg);
    super::config_state::CONFIG.write(cs);
    r
}

/// 完整设置 STORAGE 快照 (用于 init/reload). 同时把 proto.status 同步到 atomic.
pub fn storage_set_snapshot(snap: StorageSnapshot) {
    super::storage_state::proto_status_set(snap.proto.status);
    STORAGE.write(snap);
}

/// 修改 STORAGE 快照. 用 closure 在 clone 出的快照上做任意修改, 然后 RCU 替换.
pub fn storage_modify<F: FnOnce(&mut StorageSnapshot)>(f: F) {
    let mut snap = storage_clone();
    f(&mut snap);
    sync_proto_status(&mut snap);
    STORAGE.write(snap);
}
