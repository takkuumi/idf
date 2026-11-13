//! BLE 逻辑配置子协议处理器 (MCA 0xD0/0xD1/0xD2/0xD3)
//!
//! 移植自参考固件 BleMsgDeal.cpp 的 BLE 逻辑配置协议:
//! - 0xD0 LOGIC_CONFIG: 写入设备功能配置 (按 sub-command 存储到 NVS)
//! - 0xD1 LOGIC_RETRIEVE: 读取已存储的设备功能配置
//! - 0xD2 DELETE_CONFIG: 删除所有设备功能配置
//! - 0xD3 COM_REQUEST: 查询当前 DI/DO 状态位图
//!
//! 帧格式 (对齐参考固件 vComboPacket):
//!   length(1) + mac(6) + cmd(1) + sub(1) + data(N) + crc(2 LE)
//!
//! 配置存储: 每个设备类型 (sub-command 0x00-0x22) 的配置以 NVS blob 持久化.

use heapless::Vec as HVec;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// 配置项 NVS 命名空间
const NVS_NAMESPACE: &str = "logic_cfg";
/// 配置有效标志字节
const CFG_VALID_FLAG: u8 = 0x55;

const LEGACY_TABLE_MAX_WORDS: usize = 1700;
const LEGACY_TABLE_MAX_RECORDS: usize = 32;
const LEGACY_TABLE_COVERAGE_BYTES: usize = LEGACY_TABLE_MAX_WORDS.div_ceil(8);
const LEGACY_TABLE_SESSION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyTableWriteStatus {
    Staged,
    Committed,
}

struct LegacyTableStage {
    session_epoch: u32,
    header_words: usize,
    terminal_word: usize,
    baseline_crc32: u32,
    words: HVec<u16, LEGACY_TABLE_MAX_WORDS>,
    received: [u8; LEGACY_TABLE_COVERAGE_BYTES],
    updated_at: Instant,
}

impl LegacyTableStage {
    fn begin(session_epoch: u32, header: &[u16]) -> Result<Self, ()> {
        if header.len() < 2 {
            return Err(());
        }
        let count = usize::from(header[0]);
        let header_words = count.checked_add(2).ok_or(())?;
        if count > LEGACY_TABLE_MAX_RECORDS || header.len() != header_words {
            return Err(());
        }
        let terminal_word = usize::from(header[count + 1]);
        if terminal_word < header_words || terminal_word > LEGACY_TABLE_MAX_WORDS {
            return Err(());
        }
        if count == 0 {
            if terminal_word != 2 {
                return Err(());
            }
        } else {
            let mut expected = header_words;
            for index in 0..count {
                let start = usize::from(header[index + 1]);
                let end = usize::from(header[index + 2]);
                if start != expected || start >= end || end > terminal_word {
                    return Err(());
                }
                expected = end;
            }
            if expected != terminal_word {
                return Err(());
            }
        }
        let mut words = HVec::new();
        for _ in 0..terminal_word {
            words.push(0).map_err(|_| ())?;
        }
        words[..header_words].copy_from_slice(header);
        let mut stage = Self {
            session_epoch,
            header_words,
            terminal_word,
            baseline_crc32: 0,
            words,
            received: [0; LEGACY_TABLE_COVERAGE_BYTES],
            updated_at: Instant::now(),
        };
        if terminal_word == header_words {
            for index in 0..header_words {
                stage.mark_received(index);
            }
        }
        Ok(stage)
    }

    fn append(
        &mut self,
        session_epoch: u32,
        start_word: usize,
        values: &[u16],
    ) -> Result<bool, ()> {
        if self.session_epoch != session_epoch
            || self.updated_at.elapsed() > LEGACY_TABLE_SESSION_TIMEOUT
        {
            return Err(());
        }
        let end_word = start_word.checked_add(values.len()).ok_or(())?;
        if values.is_empty()
            || start_word < self.header_words
            || end_word > self.terminal_word
            || end_word <= start_word
        {
            return Err(());
        }
        for (offset, &value) in values.iter().enumerate() {
            let index = start_word + offset;
            if self.is_received(index) && self.words[index] != value {
                return Err(());
            }
            self.words[index] = value;
            self.mark_received(index);
        }
        self.updated_at = Instant::now();
        Ok((self.header_words..self.terminal_word).all(|index| self.is_received(index)))
    }

    fn mark_received(&mut self, index: usize) {
        self.received[index / 8] |= 1 << (index % 8);
    }

    fn is_received(&self, index: usize) -> bool {
        self.received[index / 8] & (1 << (index % 8)) != 0
    }
}

static LEGACY_TABLE_STAGE: LazyLock<Mutex<Option<LegacyTableStage>>> =
    LazyLock::new(|| Mutex::new(None));

/// Stage the handheld B1/B3 FC16 table in RAM. Only the final contiguous table
/// is published to the live RCU snapshot and A/B flash slot; interrupted or
/// malformed uploads leave the currently active table untouched.
pub fn stage_legacy_function_table_block(
    session_epoch: u32,
    address: u16,
    values: &[u16],
) -> Option<Result<LegacyTableWriteStatus, ()>> {
    let base = crate::config::regs::HOLD_DEVICE_CONFIG;
    let mut state = LEGACY_TABLE_STAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if address == base {
        let stage = LegacyTableStage::begin(session_epoch, values).and_then(|mut stage| {
            stage.baseline_crc32 = active_function_table_crc32()?;
            Ok(stage)
        });
        return Some(match stage {
            Ok(stage) if stage.header_words == stage.terminal_word => {
                let committed = commit_legacy_function_table(&stage.words, stage.baseline_crc32);
                if committed.is_ok() {
                    *state = None;
                    Ok(LegacyTableWriteStatus::Committed)
                } else {
                    *state = Some(stage);
                    Err(())
                }
            }
            Ok(stage) => {
                *state = Some(stage);
                Ok(LegacyTableWriteStatus::Staged)
            }
            Err(()) => {
                *state = None;
                Err(())
            }
        });
    }
    let table_end = crate::config::regs::HOLD_USER_BASE;
    if address < base || address >= table_end {
        return None;
    }
    let Some(stage) = state.as_mut() else {
        // A data block without its B1 offset map must never be written into
        // the active function table as an ordinary FC16 operation.
        return Some(Err(()));
    };
    let start_word = usize::from(address.saturating_sub(base));
    match stage.append(session_epoch, start_word, values) {
        Ok(false) => Some(Ok(LegacyTableWriteStatus::Staged)),
        Ok(true) => {
            let committed = commit_legacy_function_table(&stage.words, stage.baseline_crc32);
            if committed.is_ok() {
                *state = None;
                Some(Ok(LegacyTableWriteStatus::Committed))
            } else {
                Some(Err(()))
            }
        }
        Err(()) => {
            *state = None;
            Some(Err(()))
        }
    }
}

pub fn cancel_staged_legacy_function_table() {
    *LEGACY_TABLE_STAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

fn active_function_table_crc32() -> Result<u32, ()> {
    let base = crate::config::regs::HOLD_DEVICE_CONFIG;
    let offset = usize::from(base - crate::config::regs::HOLD_PXX_BASE);
    crate::bus::storage_state::storage_read_with(|storage| {
        let words = &storage.holding_buf[offset..offset + LEGACY_TABLE_MAX_WORDS];
        let mut crc = !0u32;
        for word in words {
            for byte in word.to_le_bytes() {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
        }
        Some(!crc)
    })
    .flatten()
    .ok_or(())
}

fn commit_legacy_function_table(words: &[u16], expected_baseline_crc32: u32) -> Result<(), ()> {
    crate::modbus::rtu_master::validate_staged_logic_table(words).map_err(|error| {
        log::warn!("[logic_cfg] staged handheld table failed runtime validation: {error}");
    })?;
    if active_function_table_crc32()? != expected_baseline_crc32 {
        log::warn!(
            "[logic_cfg] staged handheld table rejected after concurrent active-table change"
        );
        return Err(());
    }
    let base = crate::config::regs::HOLD_DEVICE_CONFIG;
    let previous = crate::bus::storage_state::storage_read_with(|storage| {
        let offset = usize::from(base - crate::config::regs::HOLD_PXX_BASE);
        let mut words: HVec<u16, LEGACY_TABLE_MAX_WORDS> = HVec::new();
        words
            .extend_from_slice(&storage.holding_buf[offset..offset + LEGACY_TABLE_MAX_WORDS])
            .ok()?;
        Some(words)
    })
    .flatten()
    .ok_or(())?;
    crate::bus::backends::arm_logic_write();
    if !crate::bus::backends::write_hold_regs(base, words) {
        crate::bus::backends::disarm_logic_write();
        return Err(());
    }
    if let Err(error) = crate::device::holding_store::save_to_nvs() {
        log::error!("[logic_cfg] staged handheld table persist failed: {error}");
        // Keep the previous active runtime table and leave its snapshot dirty
        // for the normal actor retry path if flash remains unavailable.
        crate::bus::backends::arm_logic_write();
        if !crate::bus::backends::write_hold_regs(base, &previous) {
            crate::bus::backends::disarm_logic_write();
            log::error!("[logic_cfg] staged table rollback to previous runtime state failed");
        } else if let Err(rollback_error) = crate::device::holding_store::save_to_nvs() {
            log::error!(
                "[logic_cfg] previous table rollback persist retry failed: {rollback_error}"
            );
        }
        return Err(());
    }
    Ok(())
}

/// 设备功能子命令定义 (对齐参考固件 PROTOCOL_CONFIG_*)
/// 0x00-0x22: 35 种设备类型的配置写入/读取
/// 0x23-0x28: IO 状态查询/响应 (0xD3 专用)
pub const SUB_TRAFFIC_00: u8 = 0x00;
pub const SUB_DEVICE_01: u8 = 0x01;
pub const SUB_TRAFFIC_02: u8 = 0x02;
pub const SUB_TRAFFIC_03: u8 = 0x03;
pub const SUB_TRAFFIC_04: u8 = 0x04;
pub const SUB_TRAFFIC_05: u8 = 0x05;
pub const SUB_TRAFFIC_06: u8 = 0x06;
pub const SUB_TRAFFIC_07: u8 = 0x07;
pub const SUB_TRAFFIC_08: u8 = 0x08;
pub const SUB_TRAFFIC_09: u8 = 0x09;
pub const SUB_CROSS_HOLE: u8 = 0x0A;
pub const SUB_3LIGHT: u8 = 0x0B;
pub const SUB_4LIGHT: u8 = 0x0C;
pub const SUB_JET_FAN_2: u8 = 0x0D;
pub const SUB_JET_FAN_3: u8 = 0x0E;
pub const SUB_JET_FAN_2IL: u8 = 0x0F;
pub const SUB_BLOWER_2: u8 = 0x10;
pub const SUB_BLOWER_1: u8 = 0x11;
pub const SUB_LIGHT_2: u8 = 0x12;
pub const SUB_LIGHT_1: u8 = 0x13;
pub const SUB_PUMP_2: u8 = 0x14;
pub const SUB_PUMP_1: u8 = 0x15;
pub const SUB_SHUTTER: u8 = 0x16;
pub const SUB_FIRE_DOOR_1: u8 = 0x17;
pub const SUB_FIRE_DOOR_2: u8 = 0x18;
pub const SUB_COVI: u8 = 0x19;
pub const SUB_NO2: u8 = 0x1A;
pub const SUB_COVI_NO2: u8 = 0x1B;
pub const SUB_WIND: u8 = 0x1C;
pub const SUB_HOLE_LIGHT: u8 = 0x1D;
pub const SUB_OUT_LIGHT: u8 = 0x1E;
pub const SUB_CAR_CHECK: u8 = 0x1F;
pub const SUB_RS485_01: u8 = 0x20;
pub const SUB_RS485_02: u8 = 0x21;
pub const SUB_POSITION: u8 = 0x22;
pub const SUB_COM_INPUT_MSG: u8 = 0x23;
pub const SUB_COM_OUTPUT_MSG: u8 = 0x24;
pub const SUB_COM_REQUEST: u8 = 0x26;
pub const SUB_COM_INPUT_ANS: u8 = 0x27;
pub const SUB_COM_OUTPUT_ANS: u8 = 0x28;

/// 子命令名称表 (用于日志)
fn sub_name(sub: u8) -> &'static str {
    match sub {
        0x00 => "TrafficSignal00",
        0x01 => "DeviceSignal01",
        0x02 => "TrafficSignal02",
        0x03 => "TrafficSignal03",
        0x04 => "TrafficSignal04",
        0x05 => "TrafficSignal05",
        0x06 => "TrafficSignal06",
        0x07 => "TrafficSignal07",
        0x08 => "TrafficSignal08",
        0x09 => "TrafficSignal09",
        0x0A => "CrossHole",
        0x0B => "3Light",
        0x0C => "4Light",
        0x0D => "JetFan2",
        0x0E => "JetFan3",
        0x0F => "JetFan2IL",
        0x10 => "Blower2",
        0x11 => "Blower1",
        0x12 => "Light2",
        0x13 => "Light1",
        0x14 => "Pump2",
        0x15 => "Pump1",
        0x16 => "Shutter",
        0x17 => "FireDoor1",
        0x18 => "FireDoor2",
        0x19 => "COVI",
        0x1A => "NO2",
        0x1B => "COVI+NO2",
        0x1C => "WindDirSpeed",
        0x1D => "HoleLight",
        0x1E => "OutLight",
        0x1F => "CarCheck",
        0x20 => "RS485-1",
        0x21 => "RS485-2",
        0x22 => "Position",
        0x23 => "ComInputMsg",
        0x24 => "ComOutputMsg",
        0x26 => "ComRequest",
        0x27 => "ComInputAns",
        0x28 => "ComOutputAns",
        _ => "Unknown",
    }
}

/// NVS key for a given sub-command
fn nvs_key_for(sub: u8) -> String {
    format!("cfg_{:02x}", sub)
}

/// 存储配置到 NVS
///
/// 复用 device 模块的全局 NVS 句柄 (imc namespace), 用 key 前缀 "lg_" 隔离.
/// 避免 open 新 namespace (EspDefaultNvsInstance 在 esp-idf-svc 0.52 不存在).
fn store_config(sub: u8, data: &[u8]) -> bool {
    // blob = [valid_flag][len][data...]
    let mut blob: HVec<u8, 256> = HVec::new();
    if blob.push(CFG_VALID_FLAG).is_err() {
        log::error!("[logic_cfg] blob overflow (flag)");
        return false;
    }
    if blob.push(data.len() as u8).is_err() {
        log::error!("[logic_cfg] blob overflow (len)");
        return false;
    }
    if blob.extend_from_slice(data).is_err() {
        log::error!("[logic_cfg] blob overflow (data len={})", data.len());
        return false;
    }
    let key = nvs_key_for(sub);
    match crate::device::try_with_nvs_mut(|nvs| {
        nvs.set_blob(&key, &blob)
            .map_err(|e| format!("nvs set_blob {key}: {e:?}"))
    }) {
        Some(Ok(())) => {
            log::info!(
                "[logic_cfg] stored sub=0x{:02X} ({}): {} bytes",
                sub,
                sub_name(sub),
                data.len()
            );
            true
        }
        Some(Err(msg)) => {
            log::error!("[logic_cfg] NVS write failed sub=0x{:02X}: {}", sub, msg);
            false
        }
        None => {
            log::warn!("[logic_cfg] NVS unavailable, sub=0x{:02X} not stored", sub);
            false
        }
    }
}

/// Store the legacy device-function table in the same PRegBuf window used by
/// the handheld B1/B3 commands. Only the D0 aggregate count is little-endian;
/// each record has a one-byte transport length followed by big-endian fields.
/// Holding uses relative word offsets and excludes the transport length.
fn store_device_function_config(data: &[u8]) -> bool {
    let base = crate::config::regs::HOLD_DEVICE_CONFIG;
    if data.len() < 2 {
        return false;
    }
    let count = u16::from_le_bytes([data[0], data[1]]) as usize;
    if count > 32 {
        return false;
    }
    // PRegBuf layout: count, `count` record pointers, then one terminal offset.
    // `parse_poll_table` reads the terminal offset at base + count + 1, so the
    // first record must start after all count + 2 header words.
    let table_words = count + 2;
    let max_words = usize::from(crate::config::regs::HOLD_USER_BASE - base);
    if table_words > max_words {
        return false;
    }
    let mut words: heapless::Vec<u16, 2048> = heapless::Vec::new();
    for _ in 0..table_words {
        let _ = words.push(0);
    }
    words[0] = count as u16;
    let mut pos = 2usize;
    let mut data_word = table_words;
    for index in 0..count {
        let Some(&len_byte) = data.get(pos) else {
            return false;
        };
        let len = len_byte as usize;
        // All supported record fields occupy complete registers. Reject a
        // truncated header or odd field payload instead of shifting a pad byte
        // into the following record.
        if len < 9 || !(len - 1).is_multiple_of(2) || pos + len > data.len() {
            return false;
        }
        if data_word >= max_words {
            return false;
        }
        words[index + 1] = data_word as u16;
        let record = &data[pos + 1..pos + len];
        for chunk in record.chunks(2) {
            if words.len() >= max_words {
                return false;
            }
            let value = u16::from_be_bytes([chunk[0], chunk[1]]);
            if words.push(value).is_err() {
                return false;
            }
        }
        data_word += record.len().div_ceil(2);
        pos += len;
    }
    // This offset bounds the final variable-length record and is mandatory for
    // the runtime parser. Without it, the runtime treats the config as corrupt
    // after reboot even though the raw words were persisted successfully.
    if pos != data.len() || data_word > max_words {
        return false;
    }
    words[count + 1] = data_word as u16;
    // BLE receives the complete legacy configuration frame atomically.  The
    // MCA transport opens the same 0x55AA PRegBuf commit gate around that
    // frame; use the shared backend gate so BLE and PC Modbus persistence are
    // identical.
    crate::bus::backends::arm_logic_write();
    let written = crate::bus::backends::write_hold_regs(base, &words);
    if !written {
        crate::bus::backends::disarm_logic_write();
    }
    if !written {
        return false;
    }

    // The holding store coalesces ordinary Modbus writes for two seconds. A
    // handheld D0 transaction has an explicit success ACK, so make that ACK
    // mean the complete table has reached the verified A/B flash slot.
    match crate::device::holding_store::save_to_nvs() {
        Ok(()) => true,
        Err(error) => {
            log::error!("[logic_cfg] table persist failed: {error}");
            false
        }
    }
}

/// MCA D0 subcommands 0x20/0x21 are the two RS485 port records.  The legacy
/// firmware writes five little-endian words straight into PRegBuf, and the
/// runtime port task reads those same words.  Keeping them in the generic
/// logic_cfg namespace would acknowledge the packet while leaving the active
/// serial configuration unchanged.
fn store_rs485_config(sub: u8, data: &[u8]) -> bool {
    if data.len() != 10 {
        return false;
    }
    let base = match sub {
        SUB_RS485_01 => crate::config::regs::HOLD_RS485_BASE,
        SUB_RS485_02 => {
            crate::config::regs::HOLD_RS485_BASE + crate::config::regs::HOLD_RS485_STRIDE
        }
        _ => return false,
    };
    let mut words: HVec<u16, 5> = HVec::new();
    for pair in data.chunks_exact(2) {
        if words.push(u16::from_le_bytes([pair[0], pair[1]])).is_err() {
            return false;
        }
    }
    crate::bus::backends::write_hold_regs(base, &words)
}

fn load_rs485_config(sub: u8) -> Option<HVec<u8, 256>> {
    let base = match sub {
        SUB_RS485_01 => crate::config::regs::HOLD_RS485_BASE,
        SUB_RS485_02 => {
            crate::config::regs::HOLD_RS485_BASE + crate::config::regs::HOLD_RS485_STRIDE
        }
        _ => return None,
    };
    let mut out = HVec::new();
    for offset in 0..5u16 {
        let word = crate::bus::backends::read_hold_reg(base + offset)?;
        out.extend_from_slice(&word.to_le_bytes()).ok()?;
    }
    Some(out)
}

/// Read the legacy device-function table back in the exact 0xD1 wire layout.
fn load_device_function_config() -> Option<HVec<u8, 256>> {
    let base = crate::config::regs::HOLD_DEVICE_CONFIG;
    let count = crate::bus::backends::read_hold_reg(base)? as usize;
    if count > 32 {
        return None;
    }
    let terminal = crate::bus::backends::read_hold_reg(base + count as u16 + 1)?;
    if terminal < (count + 2) as u16
        || base.checked_add(terminal)? > crate::config::regs::HOLD_USER_BASE
        || (count == 0 && terminal != 2)
    {
        return None;
    }
    let mut out = HVec::new();
    out.extend_from_slice(&(count as u16).to_le_bytes()).ok()?;
    let mut expected_start = (count + 2) as u16;
    for index in 0..count {
        let offset = crate::bus::backends::read_hold_reg(base + 1 + index as u16)?;
        let end = crate::bus::backends::read_hold_reg(base + 2 + index as u16)?;
        if offset != expected_start || offset >= end || end > terminal {
            return None;
        }
        let len = usize::from(end - offset) * 2;
        // Large tables are read using FC03 blocks, not a truncated D1 response.
        if !(8..=254).contains(&len) || out.len() + len + 1 > 254 {
            return None;
        }
        out.push((len + 1) as u8).ok()?;
        for word_index in offset..end {
            let word = crate::bus::backends::read_hold_reg(base + word_index)?;
            out.extend_from_slice(&word.to_be_bytes()).ok()?;
        }
        expected_start = end;
    }
    Some(out)
}

/// 从 NVS 读取配置
fn load_config(sub: u8) -> Option<HVec<u8, 256>> {
    let key = nvs_key_for(sub);
    let mut buf = [0u8; 256];
    let blob = crate::device::try_with_nvs(|nvs| nvs.get_blob(&key, &mut buf).ok())
        .flatten()
        .flatten()?;
    if blob.len() < 2 || blob[0] != CFG_VALID_FLAG {
        return None;
    }
    let len = blob[1] as usize;
    if blob.len() < 2 + len {
        return None;
    }
    let mut out = HVec::new();
    out.extend_from_slice(&blob[2..2 + len]).ok()?;
    Some(out)
}

/// 删除所有配置 (NVS erase)
fn delete_all_configs() -> bool {
    let mut deleted = 0;
    for sub in 0x00..=0x28u8 {
        let key = nvs_key_for(sub);
        if let Some(Ok(_existed)) = crate::device::try_with_nvs_mut(|nvs| nvs.remove(&key)) {
            deleted += 1;
        }
    }
    log::info!("[logic_cfg] deleted {} config entries", deleted);
    true
}

/// 构建 ACK 帧 (对齐参考固件 vNormalAckPacket)
/// 返回: [cmd=0xD0, sub, status=0x00(success)]
fn build_ack(cmd: u8, sub: u8, status: u8) -> heapless::Vec<u8, 256> {
    let mut rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = rsp.push(cmd);
    let _ = rsp.push(sub);
    let _ = rsp.push(status);
    rsp
}

/// 处理 0xD0 LOGIC_CONFIG (写入设备功能配置)
///
/// 对齐参考固件 DealLogicConfigCMD: 接收 sub=0x00-0x28 的配置数据并存储.
/// data 已剥离 BLE 帧头 (从 length 字节之后开始).
pub fn handle_logic_config(data: &[u8]) -> Option<heapless::Vec<u8, 256>> {
    if data.len() < 2 {
        log::warn!("[logic_cfg] D0: data too short ({} bytes)", data.len());
        return None;
    }
    // data[0] = sub-command (设备类型)
    // data[1..] = 配置有效载荷
    let sub = data[0];
    let payload = &data[1..];

    log::debug!(
        "[logic_cfg] D0 LOGIC_CONFIG: sub=0x{:02X} ({}), payload={} bytes",
        sub,
        sub_name(sub),
        payload.len()
    );

    let success = if sub == SUB_DEVICE_01 {
        store_device_function_config(payload)
    } else if sub == SUB_RS485_01 || sub == SUB_RS485_02 {
        store_rs485_config(sub, payload)
    } else if sub <= 0x22 || sub == 0x23 || sub == 0x24 || sub == 0x26 {
        // 通用配置存储 (0x00-0x22, 0x23, 0x24, 0x26)
        store_config(sub, payload)
    } else {
        log::warn!("[logic_cfg] D0: unsupported sub=0x{:02X}", sub);
        false
    };

    if success {
        Some(build_ack(0xD0, sub, 0x00))
    } else {
        Some(build_ack(0xD0, sub, 0x84)) // DEAL error
    }
}

/// 处理 0xD1 LOGIC_RETRIEVE (读取设备功能配置)
///
/// 对齐参考固件 DealLogicRetrieveCMD: 根据 sub 查询已存储的配置并返回.
pub fn handle_logic_retrieve(data: &[u8]) -> Option<heapless::Vec<u8, 256>> {
    if data.is_empty() {
        log::warn!("[logic_cfg] D1: empty data");
        return None;
    }
    let sub = data[0];
    log::debug!(
        "[logic_cfg] D1 LOGIC_RETRIEVE: sub=0x{:02X} ({})",
        sub,
        sub_name(sub)
    );

    let config = if sub == SUB_DEVICE_01 {
        load_device_function_config()
    } else if sub == SUB_RS485_01 || sub == SUB_RS485_02 {
        load_rs485_config(sub)
    } else {
        load_config(sub)
    };
    match config {
        Some(cfg_data) => {
            let mut rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
            rsp.push(0xD1).ok()?;
            rsp.push(sub).ok()?;
            rsp.extend_from_slice(&cfg_data).ok()?;
            log::debug!(
                "[logic_cfg] D1: sub=0x{:02X} ({}) → {} bytes",
                sub,
                sub_name(sub),
                cfg_data.len()
            );
            Some(rsp)
        }
        None => {
            log::debug!(
                "[logic_cfg] D1: sub=0x{:02X} ({}) not configured",
                sub,
                sub_name(sub)
            );
            // 参考固件: 未配置时返回错误帧 (cmd 字段保持 0xD1 与请求匹配)
            Some(build_ack(0xD1, sub, 0x84))
        }
    }
}

/// 处理 0xD2 DELETE_CONFIG (删除所有设备功能配置)
///
/// 对齐参考固件 vDeleteAllLogic: 清除所有配置文件.
pub fn handle_delete_config() -> Option<heapless::Vec<u8, 256>> {
    log::info!("[logic_cfg] D2 DELETE_CONFIG: clearing all configs");
    cancel_staged_legacy_function_table();
    let success = delete_all_configs();
    if success {
        Some(build_ack(0xD2, 0x00, 0x00))
    } else {
        Some(build_ack(0xD2, 0x00, 0x84))
    }
}

/// 处理 0xD3 COM_REQUEST (查询 DI/DO 状态)
///
/// 对齐参考固件 vConfigComReuest: 返回当前 DI/DO 位图.
/// 返回两帧: 第一帧 DI 位图 (sub=0x27), 第二帧 DO 位图 (sub=0x28).
pub fn handle_com_request() -> Option<heapless::Vec<u8, 256>> {
    log::debug!("[logic_cfg] D3 COM_REQUEST: querying DI/DO status");

    let [di, do_] = com_request_frames();
    let mut rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = rsp.extend_from_slice(&di);
    let _ = rsp.extend_from_slice(&do_);
    Some(rsp)
}

/// Build the two independent legacy D3 response payloads.  The MCA firmware
/// appends two complete vComboPacket frames to the TX buffer; the BLE transport
/// must therefore wrap and notify them separately instead of nesting both
/// payloads in one outer frame.
pub fn com_request_frames() -> [heapless::Vec<u8, 256>; 2] {
    log::debug!("[logic_cfg] D3 COM_REQUEST: querying DI/DO status");

    // 读取当前 DI 状态 (离散输入位图)
    let di_bitmap = read_io_bitmap(true);
    // 读取当前 DO 状态 (线圈位图)
    let do_bitmap = read_io_bitmap(false);

    let mut di_rsp: heapless::Vec<u8, 256> = heapless::Vec::new();

    // 第一帧: DI 位图 (cmd=0xD3, sub=0x27)
    let _ = di_rsp.push(0xD3);
    let _ = di_rsp.push(SUB_COM_INPUT_ANS);
    let _ = di_rsp.extend_from_slice(&di_bitmap);

    let mut do_rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
    // 第二帧: DO 位图 (cmd=0xD3, sub=0x28)
    let _ = do_rsp.push(0xD3);
    let _ = do_rsp.push(SUB_COM_OUTPUT_ANS);
    let _ = do_rsp.extend_from_slice(&do_bitmap);

    [di_rsp, do_rsp]
}

/// 读取 IO 位图 (DI 或 DO)
fn read_io_bitmap(is_di: bool) -> heapless::Vec<u8, 8> {
    let mut bitmap: heapless::Vec<u8, 8> = heapless::Vec::new();
    let count = if is_di {
        crate::config::hw_version::DI_COUNT
    } else {
        crate::config::hw_version::DO_COUNT
    };
    let io = &crate::bus::IO;

    // The MCA packet builder walks from the last channel to the first and
    // shifts left, so channel 0 is the least-significant bit of the final
    // byte in a full 16/48-channel response.
    for group_start in (0..count).step_by(8) {
        let group_len = (count - group_start).min(8);
        let mut byte = 0u8;
        for offset in 0..group_len {
            let ch = count - group_start - 1 - offset;
            let bit = if is_di {
                io.di.get_bit(ch)
            } else {
                io.do_.get_bit(ch)
            };
            byte = (byte << 1) | u8::from(bit);
        }
        if group_len < 8 {
            byte <<= 8 - group_len;
        }
        let _ = bitmap.push(byte);
    }
    bitmap
}

/// 入口: 根据 func 分发到对应处理函数
///
/// 返回 Some(response_data) 表示已处理, 调用方用 send_ble_frame 发送.
/// 返回 None 表示未处理 (fallback 到标准 Modbus 路径).
pub fn dispatch_logic_cmd(func: u8, data: &[u8]) -> Option<heapless::Vec<u8, 256>> {
    match func {
        0xD0 => handle_logic_config(data),
        0xD1 => handle_logic_retrieve(data),
        0xD2 => handle_delete_config(),
        0xD3 => handle_com_request(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LegacyTableStage, SUB_RS485_01, handle_logic_config, handle_logic_retrieve,
        load_device_function_config, store_device_function_config,
    };

    #[test]
    fn staged_legacy_table_waits_for_every_word_before_publish() {
        let header = [2, 4, 6, 8];
        let mut stage = LegacyTableStage::begin(77, &header).expect("valid B1 header");
        assert_eq!(stage.append(77, 4, &[0x0102, 0x0304]).unwrap(), false);
        assert_eq!(stage.append(77, 6, &[0x0506, 0x0708]).unwrap(), true);
        assert_eq!(
            stage.words.as_slice(),
            &[2, 4, 6, 8, 0x0102, 0x0304, 0x0506, 0x0708]
        );
    }

    #[test]
    fn staged_legacy_table_rejects_out_of_order_pointers_and_foreign_connection() {
        assert!(LegacyTableStage::begin(77, &[2, 5, 6, 8]).is_err());
        let mut stage = LegacyTableStage::begin(77, &[1, 3, 5]).expect("valid table");
        assert!(stage.append(88, 3, &[0x0102, 0x0304]).is_err());
        assert!(stage.append(77, 5, &[0x0102]).is_err());
    }

    #[test]
    fn staged_legacy_table_accepts_identical_retry_and_rejects_conflicting_retry() {
        let mut stage = LegacyTableStage::begin(77, &[1, 3, 5]).expect("valid header");
        assert!(!stage.append(77, 3, &[0x1234, 0x5678]).unwrap());
        assert!(!stage.append(77, 3, &[0x1234]).unwrap());
        assert!(stage.append(77, 3, &[0xffff]).is_err());
    }

    #[test]
    fn empty_legacy_table_header_is_a_complete_transaction() {
        let stage = LegacyTableStage::begin(77, &[0, 2]).expect("empty table");
        assert_eq!(stage.header_words, stage.terminal_word);
        assert!(LegacyTableStage::begin(77, &[0, 0]).is_err());
    }

    #[test]
    fn device_function_table_round_trips_through_preg() {
        // One actual analog record: D0 count + transport length + BE fields.
        // Storage must remove the length prefix and use runtime-relative offsets.
        let packet = [
            1, 0, 23, 0, 3, 0, 0, 0, 0, 0, 0, 12, 1, 0, 0, 0, 0, 0, 0, 15, 255, 0, 0, 0, 0,
        ];
        assert!(store_device_function_config(&packet));
        let loaded = load_device_function_config().expect("stored table must load");
        assert_eq!(loaded.as_slice(), packet);
    }

    #[test]
    fn rs485_logic_config_round_trips_through_runtime_preg() {
        let mut packet = vec![SUB_RS485_01];
        for value in [0x1234u16, 7, 3, 450, 60] {
            packet.extend_from_slice(&value.to_le_bytes());
        }
        let ack = handle_logic_config(&packet).expect("D0 ack");
        assert_eq!(&ack[..3], &[0xD0, SUB_RS485_01, 0]);

        let response = handle_logic_retrieve(&[SUB_RS485_01]).expect("D1 response");
        assert_eq!(&response[..2], &[0xD1, SUB_RS485_01]);
        assert_eq!(&response[2..], &packet[1..]);
    }
}
