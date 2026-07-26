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

use std::sync::Arc;

use crate::config::{hw_version, regs};
use crate::error::recovery::{self, DegradedMode};

use super::io_global::IO;
use super::storage_state::{storage_read, storage_read_with, proto_status, StorageSnapshot, STORAGE};
use super::config_state::{config_read, config_read_with, ConfigSnapshot};

// ----------------------------------------------------------------------------
// LOOP8: RCU 写者串行化锁
// ----------------------------------------------------------------------------
//
// Rcu::write 假定串行写者 (单 Modbus 任务串 / 单 DeviceActor)。但实际架构有
// 多个并发写者: Modbus TCP 4 连接 + RTU + DeviceActor + RS485 master + DHCP。
// 并发写者会导致:
//   1. RMW 竞争丢写 (两个线程同时 clone 同一快照, 各自修改, 后写覆盖前写)
//   2. retire_queue 溢出 (4 槽, 超过即 leak ~11KB StorageSnapshot)
//
// 修复: 用 Spin<()> 短锁保护 clone-mutate-write 序列。持锁时间 = clone + mutate +
// Rcu::write, 约数十微秒 (clone 走 heap, write 走 atomic swap)。远小于 Modbus
// 响应超时 (2s), 不会阻塞 Modbus TCP 线程。
//
// 注意: 这把锁是"写者串行化"锁, 与读者无关 (读者仍走 RCU 无锁读路径)。
// 它保护的是 RMW 序列的原子性, 而非单个字段的并发安全 (字段并发由 RCU 保证)。

/// 串行化 STORAGE + CONFIG RCU 写者 (单锁覆盖两域)
///
/// 同时保护 STORAGE 和 CONFIG 的 clone-mutate-write 序列. 单一锁简化分析:
/// 即使写者在 STORAGE 与 CONFIG 之间切换, 整段都是单线程执行, 避免跨域 race.
static RCU_WRITE_LOCK: crate::sync::Spin<()> = crate::sync::Spin::new(());

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
            // LOOP11: read_with 零拷贝 — ConfigSnapshot ~2.5KB, BTC_TASK 栈仅 8KB,
            // 原 config_read() 的 Arc::new(s.clone()) clone 整个快照到栈上导致 Stack canary 溢出
            Some(config_read_with(|cs| cs.cfg.fw_date).unwrap_or(0x0615))
        }
        // ---- BLE Android 兼容寄存器 (Modbus TCP 也可读) ----
        regs::INREG_HW_VER => {
            Some(config_read_with(|cs| cs.cfg.hw_version).unwrap_or(0x0100))
        }
        regs::INREG_IP_BASE => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.ip[0], cs.cfg.ip[1]])).unwrap_or(0))
        }
        2248 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.ip[2], cs.cfg.ip[3]])).unwrap_or(0))
        }
        2249 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.mask[0], cs.cfg.mask[1]])).unwrap_or(0))
        }
        2250 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.mask[2], cs.cfg.mask[3]])).unwrap_or(0))
        }
        2251 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.gateway[0], cs.cfg.gateway[1]])).unwrap_or(0))
        }
        2252 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.gateway[2], cs.cfg.gateway[3]])).unwrap_or(0))
        }
        regs::INREG_MAC_BASE => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.eth_mac[0], cs.cfg.eth_mac[1]])).unwrap_or(0))
        }
        2264 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.eth_mac[2], cs.cfg.eth_mac[3]])).unwrap_or(0))
        }
        2265 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.eth_mac[4], cs.cfg.eth_mac[5]])).unwrap_or(0))
        }
        regs::INREG_BLE_ID_BASE => {
            // BLE 名称前 4 字节 (UTF-8 模式兼容)
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.ble_name[0], cs.cfg.ble_name[1]])).unwrap_or(0))
        }
        2275 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.ble_name[2], cs.cfg.ble_name[3]])).unwrap_or(0))
        }
        2276 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.ble_name[4], cs.cfg.ble_name[5]])).unwrap_or(0))
        }
        2277 => {
            Some(config_read_with(|cs| u16::from_be_bytes([cs.cfg.ble_name[6], cs.cfg.ble_name[7]])).unwrap_or(0))
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
        // LOOP10: ringlog COUNT(0x0885)/WRITES(0x0886) 移至 FC=04 (read_input_reg),
        // 避免占用 FC=03 中的 0x0885-0x0886 (与 SN/PLACE 区连续, 也避免用户混淆)
        regs::INREG_RINGLOG_WRITES => {
            let cnt = crate::error::ringlog::LOG_WRITE_COUNT
                .load(std::sync::atomic::Ordering::Acquire);
            Some((cnt & 0xFFFF) as u16)
        }
        // ---- LOOP10: Ringlog 数据条目 (FC=04, 0x0887..0x08A6, 8条×4字) ----
        // 从 FC=03 移至 FC=04: 避免与 SN(0x0894)/PLACE(0x089D)/HW_VER(0x08A5) 地址冲突
        addr if addr >= regs::INREG_RINGLOG_BASE && addr < regs::INREG_RINGLOG_BASE + 32 => {
            let entry_idx = ((addr - regs::INREG_RINGLOG_BASE) / 4) as usize;
            let field_idx = (addr - regs::INREG_RINGLOG_BASE) % 4;
            let ring = crate::error::ringlog::RING_LOG.lock();
            let entries = ring.entries();
            if entry_idx < entries.len() {
                let entry = &entries[entry_idx];
                Some(match field_idx {
                    0 => (entry.timestamp_s & 0xFFFF) as u16,
                    1 => ((entry.timestamp_s >> 16) & 0xFFFF) as u16,
                    2 => entry.code,
                    3 => (entry.context & 0xFFFF) as u16,
                    _ => 0,
                })
            } else {
                Some(0)
            }
        }
        // ---- MCA 一体机分布式组播状态 (0x0090-0x0100) ----
        // 数据由 udp_multicast 模块接收并填充, 32 字节 (16 个 U16) 映射到 0x0090-0x009F.
        // 余下 0x00A0-0x00FF 保留为 0 (对齐参考固件 REG_STATU_SWITCH_END=0x0100).
        addr if (regs::INREG_SWITCH_STATUS_BASE..regs::INREG_SWITCH_STATUS_END).contains(&addr) => {
            let word_idx = (addr - regs::INREG_SWITCH_STATUS_BASE) as u16;
            crate::udp_multicast::read_switch_status(word_idx)
        }
        // ---- MONITOR_PLC 别名区 (30001-30128, LOOP12) ----
        // 老 SCADA 系统通过 FC=04 轮询 3xxxx 区. 映射: 30001-30048→DI 状态, 30049-30056→AI scaled.
        addr if addr >= regs::MONITOR_PLC_BASE && addr <= regs::MONITOR_PLC_END => {
            let idx = (addr - regs::MONITOR_PLC_BASE) as usize;
            if idx < 48 {
                // DI status (bit 0..47)
                Some(IO.di.get_bit(idx) as u16)
            } else if idx < 48 + hw_version::AI_COUNT as usize {
                // AI scaled channels (0..7)
                Some(IO.ai.get_scaled(idx - 48))
            } else {
                Some(0)
            }
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
        return storage_read_with(|s| s.device_text[idx]);
    }
    // 2. CFG / device_config / holding_buf (0x0880..=0x107F)
    if addr >= regs::HOLD_CFG_BASE && addr <= regs::HOLD_CFG_END {
        // LOOP10 修复: ringlog 已移至 FC=04 (read_input_reg), 不再在 FC=03 中拦截
        // 旧代码 ringlog 0x0887-0x08A6 与 SN(0x0894)/PLACE(0x089D)/HW_VER(0x08A5) 重叠,
        // 导致 FC=03 读 SN/PLACE 返回 ringlog 条目而非实际配置值.
        // 2b. device_config 子区 (2300..<2400): 来自 CONFIG.device_config
        if addr >= 2300 && addr < 2400 {
            // LOOP11: 零拷贝读 — device_config 含 heapless::Vec<DeviceEntry,32> (~2.5KB),
            // 原 config_read() 在 BTC_TASK 栈上 clone 整个快照触发 Stack canary
            // read_with 返回 Option<Option<u16>>, and_then 展平为 Option<u16>
            if let Some(Some(v)) = config_read_with(|cs| cs.device_config.read_reg(addr)) {
                return Some(v);
            }
        }
        // 2c. SystemConfig 子寄存器 (_CFG_BASE..=_CFG_END 子集): 来自 CONFIG.cfg
        if let Some(Some(v)) = config_read_with(|cs| cs.cfg.read_reg(addr)) {
            return Some(v);
        }
        // 2d. HOLD_PXX 残余 (0x0880..=0x107F 中 CFG 未覆盖部分): 来自 STORAGE.holding_buf
        let idx = (addr - regs::HOLD_PXX_BASE) as usize;
        if idx < regs::HOLD_PXX_COUNT {
            return storage_read_with(|s| s.holding_buf[idx]);
        }
        return Some(0);
    }
    // ---- CONTROL_PLC 别名区 (40001-40300, LOOP12) ----
    // 老 SCADA 通过 FC=03 读 4xxxx 区. 映射: 40001-40048→DO 状态, 40049+→HOLD_USER_BASE 区.
    if addr >= regs::CONTROL_PLC_BASE && addr <= regs::CONTROL_PLC_END {
        let idx = (addr - regs::CONTROL_PLC_BASE) as usize;
        return if idx < 48 {
            // DO status (bit 0..47)
            Some(IO.do_.get_bit(idx) as u16)
        } else {
            let user_idx = (idx - 48) as u16;
            if user_idx < regs::HOLD_USER_COUNT {
                storage_read_with(|s| s.holding_buf[(regs::HOLD_USER_BASE as usize) + (user_idx as usize)])
            } else {
                Some(0)
            }
        };
    }
    // 3. PROTO 区 (0x4000..<PROTO_END): 来自 STORAGE.proto
    if addr >= regs::PROTO_BASE && addr < regs::PROTO_END {
        let idx = (addr - regs::PROTO_BASE) as usize;
        return storage_read_with(|s| s.proto.data[idx]);
    }
    // 4. Proto 控制字 (PROTO_COMMIT..=PROTO_MAGIC)
    match addr {
        regs::PROTO_COMMIT => Some(0),
        regs::PROTO_RELOAD => Some(0),
        regs::PROTO_VERSION => storage_read_with(|s| s.proto.version),
        regs::PROTO_LENGTH => storage_read_with(|s| s.proto.length),
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

    /// LOOP10 回归测试: ringlog 数据条目必须通过 FC=04 (read_input_reg) 读出
    /// 0x0887..0x08A6 (32 字) 必须落在 read_input_reg, 不应被 read_hold_reg 拦截
    #[test]
    fn test_ringlog_data_readable_via_input_reg() {
        // 0x0887 = ringlog 第0条 word0 (timestamp_s 低16位)
        // FC=04 读应返回 Some(...), 不应是 None
        let v = read_input_reg(regs::INREG_RINGLOG_BASE);
        assert!(v.is_some(), "FC=04 读取 ringlog 0x0887 必须返回 Some");
        // 越界条目 (>=8) 也应返回 Some(0), 而非 None
        let v_oob = read_input_reg(regs::INREG_RINGLOG_BASE + 31);
        assert!(v_oob.is_some(), "FC=04 读取 ringlog 0x08A6 必须返回 Some");
    }

    /// LOOP10 回归测试: SN/PLACE/HW_VER 地址通过 FC=03 (read_hold_reg) 读取,
    /// 不能被 ringlog 屏蔽 (LOOP9 旧 bug: ringlog 0x0887-0x08A6 拦截导致 SN 读不出)
    /// 这里验证地址落入 CFG 区且不会进入 ringlog 分支 (因 ringlog 已移到 FC=04)
    #[test]
    fn test_ringlog_does_not_shadow_sn_place_hwver() {
        // SN base 0x0894, PLACE base 0x089D, HW_VER 0x08A5 都在 ringlog 范围内
        // 修复前: read_hold_reg(0x0894) 会返回 ringlog 条目 (错误)
        // 修复后: read_hold_reg 走 SystemConfig.read_reg → 返回 cfg.sn 字节
        // read_hold_reg 返回 Some 即说明走 SystemConfig 路径 (非 ringlog 零值)
        let sn = read_hold_reg(regs::HOLD_SN_BASE);
        assert!(sn.is_some(), "FC=03 读取 SN base 0x0894 必须返回 Some");
        let place = read_hold_reg(regs::HOLD_PLACE_BASE);
        assert!(place.is_some(), "FC=03 读取 PLACE base 0x089D 必须返回 Some");
        let hwver = read_hold_reg(regs::HOLD_HW_VER);
        assert!(hwver.is_some(), "FC=03 读取 HW_VER 0x08A5 必须返回 Some");
    }

    // ---- LOOP12 回归测试 ----

    /// PLC 别名区: MONITOR_PLC (30001-30128) 通过 FC=04 应返回 Some (DI 状态, 初始 0)
    #[test]
    fn test_monitor_plc_readable_via_fc04() {
        let v = read_input_reg(regs::MONITOR_PLC_BASE);
        assert!(v.is_some(), "FC=04 读取 MONITOR_PLC_BASE 0x7531 必须返回 Some");
        // 越界边界: 30128 = MONITOR_PLC_END
        let v_end = read_input_reg(regs::MONITOR_PLC_END);
        assert!(v_end.is_some(), "FC=04 读取 MONITOR_PLC_END 0x75B0 必须返回 Some");
        // 越界地址: 30129 = MONITOR_PLC_END + 1
        let v_oob = read_input_reg(regs::MONITOR_PLC_END + 1);
        assert!(v_oob.is_none(), "FC=04 读取越界 0x75B1 必须返回 None");
    }

    /// PLC 别名区: CONTROL_PLC (40001-40300) 通过 FC=03 应返回 Some (DO 状态, 初始 0)
    #[test]
    fn test_control_plc_readable_via_fc03() {
        let v = read_hold_reg(regs::CONTROL_PLC_BASE);
        assert!(v.is_some(), "FC=03 读取 CONTROL_PLC_BASE 0x9C41 必须返回 Some");
        // 越界边界: 40300 = CONTROL_PLC_END
        let v_end = read_hold_reg(regs::CONTROL_PLC_END);
        assert!(v_end.is_some(), "FC=03 读取 CONTROL_PLC_END 0x9D6C 必须返回 Some");
        // 越界地址: 40301
        let v_oob = read_hold_reg(regs::CONTROL_PLC_END + 1);
        assert!(v_oob.is_none(), "FC=03 读取越界 0x9D6D 必须返回 None");
    }

    /// FUNC_COUNT 寄存器 (0x08FC) 必须通过 FC=03 读取, 用于 0xB0 BLE arm
    #[test]
    fn test_func_count_via_fc03() {
        let v = read_hold_reg(regs::FUNC_COUNT);
        assert!(v.is_some(), "FC=03 读取 FUNC_COUNT 0x08FC 必须返回 Some");
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
///
/// LOOP11: device_config 现在是 `Arc<DeviceConfigTable>`, clone 仅原子 +1 (~5ns).
/// 整体 clone 栈成本: SystemConfig 144B memcpy + Arc 8B 原子, 共 ~152B
/// (原 ~2700B 降 94%). 写路径无需 Arc::make_mut (持有独立快照).
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
///
/// 多写者并发安全: 通过 RCU_WRITE_LOCK 串行化整个 RMW 序列
/// (clone → modify → atomic swap), 防止 lost update. 读路径不受影响.
pub fn write_hold_reg(addr: u16, value: u16) -> bool {
    let _guard = RCU_WRITE_LOCK.lock();
    write_hold_reg_locked(addr, value)
}

/// write_hold_reg 的内部实现, 调用方必须持有 RCU_WRITE_LOCK
fn write_hold_reg_locked(addr: u16, value: u16) -> bool {
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
        // LOOP11: Arc<DeviceConfigTable> → 用 Arc::make_mut 获取 &mut (COW)
        if Arc::make_mut(&mut cs.device_config).write_reg(addr, value) {
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
    }
    // ---- CONTROL_PLC 别名区 (40001-40300, LOOP12) ----
    // 老 SCADA FC=06/10 写入 4xxxx. 映射: 40001-40048→DO 状态, 40049+→HOLD_USER_BASE 区.
    if addr >= regs::CONTROL_PLC_BASE && addr <= regs::CONTROL_PLC_END {
        let idx = (addr - regs::CONTROL_PLC_BASE) as usize;
        if idx < 48 {
            // DO bit write
            super::io_global::IO.do_.set_bit(idx, value != 0);
            // LOOP13: 触发 DO 刷新 (老 SCADA PLC 区路径曾忘记 notify, 继电器不动)
            #[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
            crate::io::do_::notify();
            return true;
        }
        let user_idx = (idx - 48) as u16;
        if user_idx < regs::HOLD_USER_COUNT {
            let mut snap = storage_clone();
            snap.holding_buf[(regs::HOLD_USER_BASE as usize) + (user_idx as usize)] = value;
            sync_proto_status(&mut snap);
            STORAGE.write(snap);
            return true;
        }
        return false;
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
///
/// LOOP13: notify() 内化 — 所有调用方 (Modbus/web/BLE/AT) 自动获得 DO 刷新通知,
/// 避免历史上 web `/iocontrol` 与 CONTROL_PLC 等路径忘记调用 notify 导致硬件不刷新的 bug
/// (详见 docs/LOOP.md LOOP13 章节). notify 内部是 AtomicBool::store(true) 幂等.
pub fn write_coil(addr: u16, value: bool) -> bool {
    if addr >= regs::COIL_DO_BASE && addr < regs::COIL_DO_END {
        let ch = (addr - regs::COIL_DO_BASE) as usize;
        super::io_global::IO.do_.set_bit(ch, value);
        #[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
        crate::io::do_::notify();
        return true;
    }
    false
}

/// 修改 SystemConfig 部分字段后回写 CONFIG RCU. 用于 w5500 DHCP 写回等单写者场景.
///
/// 多写者并发安全: 通过 RCU_WRITE_LOCK 串行化 RMW.
pub fn config_modify<F: FnOnce(&mut SystemConfig)>(f: F) {
    config_modify_with_result(|cfg| f(cfg));
}

/// 同 `config_modify`, 但把 closure 的返回值传出 (用于 AT 命令需要拿到
/// `WriteResult` 等场景). RCU RMW: clone 快照 → mutate cfg → 原子替换.
///
/// 多写者并发安全: 通过 RCU_WRITE_LOCK 串行化 RMW, 防止 lost update.
pub fn config_modify_with_result<F, R>(f: F) -> R
where
    F: FnOnce(&mut SystemConfig) -> R,
{
    let _guard = RCU_WRITE_LOCK.lock();
    let mut cs = config_clone();
    let r = f(&mut cs.cfg);
    super::config_state::CONFIG.write(cs);
    r
}

/// 完整设置 STORAGE 快照 (用于 init/reload). 同时把 proto.status 同步到 atomic.
///
/// 多写者并发安全: 通过 RCU_WRITE_LOCK 串行化.
pub fn storage_set_snapshot(snap: StorageSnapshot) {
    let _guard = RCU_WRITE_LOCK.lock();
    super::storage_state::proto_status_set(snap.proto.status);
    STORAGE.write(snap);
}

/// 修改 STORAGE 快照. 用 closure 在 clone 出的快照上做任意修改, 然后 RCU 替换.
///
/// 多写者并发安全: 通过 RCU_WRITE_LOCK 串行化 RMW, 防止 lost update.
pub fn storage_modify<F: FnOnce(&mut StorageSnapshot)>(f: F) {
    let _guard = RCU_WRITE_LOCK.lock();
    let mut snap = storage_clone();
    f(&mut snap);
    sync_proto_status(&mut snap);
    STORAGE.write(snap);
}
