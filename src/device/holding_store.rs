//! holding_buf 掉电安全持久化。
//!
//! holding_buf 为 4096 字节；连同 proto A/B、设备文本和系统配置继续放在
//! 24KB NVS 中会耗尽 NVS 页面。这里使用独立 `holding` 原始分区的 A/B 双槽：
//! 擦除并写入 inactive slot，回读 CRC 成功后才发布新的 generation。掉电时至少
//! 保留一个完整槽。旧版 NVS `hld_buf_a/b` 仅作为一次兼容回退，不再继续写入。

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};

use esp_idf_svc::nvs::EspDefaultNvs;
use esp_idf_svc::partition::EspPartition;

use crate::bus::storage_state::{
    HOLDING_DIRTY, HOLDING_NVS_VALID, LEGACY_IO_DIRTY, storage_read_with,
};
use crate::error::{AppError, AppResult};

const PARTITION_LABEL: &str = "holding";
const SLOT_COUNT: usize = 2;
const SLOT_BYTES: usize = 0x2000;
const PARTITION_MIN_BYTES: usize = 0x8000;
const NO_ACTIVE_SLOT: u8 = u8::MAX;

const RAW_MAGIC: u32 = 0x484C_4431; // "HLD1"
const RAW_VERSION: u16 = 2;
const LEGACY_RAW_VERSION: u16 = 1;
const RAW_HEADER_BYTES: usize = 16;

const LEGACY_NVS_KEY_BLOB_A: &str = "hld_buf_a";
const LEGACY_NVS_KEY_BLOB_B: &str = "hld_buf_b";
const LEGACY_NVS_KEY_ACTIVE: &str = "hld_act";
const LEGACY_MAGIC: u16 = 0x4842;
const LEGACY_VERSION: u16 = 1;
const LEGACY_HEADER_BYTES: usize = 8;

pub const HOLDING_WORDS: usize = 2048;
pub const HOLDING_DATA_BYTES: usize = HOLDING_WORDS * 2;
const MONITOR_WORDS: usize = 128;
const CONTROL_WORDS: usize = 300;
const LEGACY_COIL_BYTES: usize = 1536;
const MONITOR_DATA_BYTES: usize = MONITOR_WORDS * 2;
const CONTROL_DATA_BYTES: usize = CONTROL_WORDS * 2;
const RAW_TOTAL_BYTES: usize = RAW_HEADER_BYTES
    + HOLDING_DATA_BYTES
    + MONITOR_DATA_BYTES
    + CONTROL_DATA_BYTES
    + LEGACY_COIL_BYTES;
const LEGACY_RAW_TOTAL_BYTES: usize = RAW_HEADER_BYTES + HOLDING_DATA_BYTES;
const LEGACY_TOTAL_BYTES: usize = LEGACY_HEADER_BYTES + HOLDING_DATA_BYTES;

static ACTIVE_SLOT: AtomicU8 = AtomicU8::new(NO_ACTIVE_SLOT);
static ACTIVE_GENERATION: AtomicU32 = AtomicU32::new(0);
static LEGACY_CLEANUP_DONE: AtomicBool = AtomicBool::new(false);

static HOLDING_BUFFER: LazyLock<Mutex<Box<[u8]>>> =
    LazyLock::new(|| Mutex::new(vec![0u8; RAW_TOTAL_BYTES].into_boxed_slice()));

static HOLDING_PARTITION: LazyLock<Mutex<Option<EspPartition>>> = LazyLock::new(|| {
    let partition = match unsafe { EspPartition::new(PARTITION_LABEL) } {
        Ok(Some(partition)) if partition.size() >= PARTITION_MIN_BYTES => {
            log::info!(
                "[holding] raw partition found: offset={:#x} size={}KB",
                partition.address(),
                partition.size() / 1024
            );
            Some(partition)
        }
        Ok(Some(partition)) => {
            log::error!(
                "[holding] partition too small: {} < {} bytes",
                partition.size(),
                PARTITION_MIN_BYTES
            );
            None
        }
        Ok(None) => {
            log::error!("[holding] partition missing; flash partitions.csv before production use");
            None
        }
        Err(error) => {
            log::error!("[holding] partition lookup failed: {error:?}");
            None
        }
    };
    Mutex::new(partition)
});

#[derive(Debug)]
struct DecodedSlot {
    slot: u8,
    generation: u32,
    words: Vec<u16>,
    monitor_words: Vec<u16>,
    control_words: Vec<u16>,
    legacy_coils: Vec<u8>,
}

pub struct LoadedHoldingState {
    pub words: Vec<u16>,
    pub monitor_words: Vec<u16>,
    pub control_words: Vec<u16>,
    pub legacy_coils: Vec<u8>,
}

impl LoadedHoldingState {
    pub fn empty() -> Self {
        Self {
            words: vec![0u16; HOLDING_WORDS],
            monitor_words: vec![0u16; MONITOR_WORDS],
            control_words: vec![0u16; CONTROL_WORDS],
            legacy_coils: vec![0u8; LEGACY_COIL_BYTES],
        }
    }
}

/// 保存当前 holding_buf。函数名保留以避免改变 DeviceActor 调用接口；实际权威
/// 存储已经迁到独立 raw partition。
pub fn save_to_nvs() -> AppResult<()> {
    let holding_dirty = HOLDING_DIRTY.swap(false, Ordering::AcqRel);
    let legacy_dirty = LEGACY_IO_DIRTY.swap(false, Ordering::AcqRel);
    if !holding_dirty && !legacy_dirty {
        return Ok(());
    }

    let validation = storage_read_with(|_storage| {
        if !holding_dirty {
            return Ok::<(), &'static str>(());
        }
        #[cfg(feature = "modbus-rtu")]
        {
            crate::modbus::rtu_master::validate_poll_config(&_storage.holding_buf).map(|_| ())
        }
        #[cfg(not(feature = "modbus-rtu"))]
        {
            Ok::<(), &'static str>(())
        }
    });
    match validation {
        Some(Ok(())) => {}
        Some(Err(error)) => {
            // Persist the raw register image even while a project is being
            // transferred and its logic table is temporarily incomplete.
            // Validation belongs to the polling engine; blocking persistence
            // here caused a power-cycle to erase a valid partial/legacy table
            // (and made subsequent logic reads return zeros).
            log::warn!(
                "[holding] 2300+ configuration is currently invalid; persisting raw data: {error}"
            );
        }
        None => {
            if holding_dirty {
                HOLDING_DIRTY.store(true, Ordering::Release);
            }
            if legacy_dirty {
                LEGACY_IO_DIRTY.store(true, Ordering::Release);
            }
            return Err(AppError::Config("holding storage unavailable".into()));
        }
    }

    let mut blob = HOLDING_BUFFER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    blob.fill(0);
    let encoded = storage_read_with(|storage| {
        if storage.holding_buf.len() != HOLDING_WORDS
            || storage.monitor_words.len() != MONITOR_WORDS
            || storage.control_words.len() != CONTROL_WORDS
            || storage.legacy_coils.len() != LEGACY_COIL_BYTES
        {
            return false;
        }
        for (index, value) in storage.holding_buf.iter().copied().enumerate() {
            let offset = RAW_HEADER_BYTES + index * 2;
            blob[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        let mut offset = RAW_HEADER_BYTES + HOLDING_DATA_BYTES;
        for value in storage.monitor_words.iter().copied() {
            blob[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            offset += 2;
        }
        for value in storage.control_words.iter().copied() {
            blob[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            offset += 2;
        }
        blob[offset..offset + LEGACY_COIL_BYTES].copy_from_slice(&storage.legacy_coils);
        true
    })
    .unwrap_or(false);
    if !encoded {
        if holding_dirty {
            HOLDING_DIRTY.store(true, Ordering::Release);
        }
        if legacy_dirty {
            LEGACY_IO_DIRTY.store(true, Ordering::Release);
        }
        return Err(AppError::Config("invalid holding buffer length".into()));
    }

    let generation = ACTIVE_GENERATION.load(Ordering::Acquire).wrapping_add(1);
    encode_raw_header(&mut blob, generation);
    let target_slot = if ACTIVE_SLOT.load(Ordering::Acquire) == 0 {
        1
    } else {
        0
    };

    let result = write_slot(target_slot, &mut blob);
    match result {
        Ok(()) => {
            ACTIVE_SLOT.store(target_slot, Ordering::Release);
            ACTIVE_GENERATION.store(generation, Ordering::Release);
            HOLDING_NVS_VALID.store(true, Ordering::Release);
            cleanup_legacy_nvs_blobs();
            log::info!(
                "[holding] persisted {} words to raw slot {} generation {}",
                HOLDING_WORDS,
                target_slot,
                generation
            );
            Ok(())
        }
        Err(error) => {
            HOLDING_DIRTY.store(true, Ordering::Release);
            if legacy_dirty {
                LEGACY_IO_DIRTY.store(true, Ordering::Release);
            }
            log::warn!("[holding] raw persist failed, dirty retained: {error}");
            Err(error)
        }
    }
}

fn cleanup_legacy_nvs_blobs() {
    if LEGACY_CLEANUP_DONE.load(Ordering::Acquire) {
        return;
    }
    let removed = crate::device::try_with_nvs_mut(|nvs| {
        for key in [
            LEGACY_NVS_KEY_BLOB_A,
            LEGACY_NVS_KEY_BLOB_B,
            LEGACY_NVS_KEY_ACTIVE,
        ] {
            if let Err(error) = nvs.remove(key) {
                log::warn!("[holding] failed to remove legacy NVS key {key}: {error:?}");
            }
        }
    });
    if removed.is_some() {
        LEGACY_CLEANUP_DONE.store(true, Ordering::Release);
        log::info!("[holding] legacy NVS blobs retired after raw snapshot verification");
    }
}

/// 从 raw A/B 槽加载；若两个槽均无效，再尝试旧 NVS blob。
pub fn load_from_nvs(nvs: &EspDefaultNvs) -> AppResult<LoadedHoldingState> {
    if let Some(slot) = load_best_raw_slot()? {
        ACTIVE_SLOT.store(slot.slot, Ordering::Release);
        ACTIVE_GENERATION.store(slot.generation, Ordering::Release);
        HOLDING_NVS_VALID.store(true, Ordering::Release);
        log::info!(
            "[holding] loaded raw slot {} generation {} ({} words)",
            slot.slot,
            slot.generation,
            slot.words.len()
        );
        return Ok(LoadedHoldingState {
            words: slot.words,
            monitor_words: slot.monitor_words,
            control_words: slot.control_words,
            legacy_coils: slot.legacy_coils,
        });
    }

    if let Some(words) = load_legacy_nvs(nvs)? {
        HOLDING_NVS_VALID.store(true, Ordering::Release);
        HOLDING_DIRTY.store(true, Ordering::Release);
        log::warn!("[holding] loaded legacy NVS snapshot; queued one-time copy to raw partition");
        return Ok(LoadedHoldingState {
            words,
            ..LoadedHoldingState::empty()
        });
    }

    ACTIVE_SLOT.store(NO_ACTIVE_SLOT, Ordering::Release);
    ACTIVE_GENERATION.store(0, Ordering::Release);
    HOLDING_NVS_VALID.store(false, Ordering::Release);
    log::warn!(
        "[holding] no valid persistent snapshot; using empty configuration until project sync"
    );
    Ok(LoadedHoldingState::empty())
}

fn load_best_raw_slot() -> AppResult<Option<DecodedSlot>> {
    // 全部路径统一先锁 buffer、再锁 partition，避免启动加载与后台保存交叉时反序。
    let mut buffer = HOLDING_BUFFER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut partition_guard = HOLDING_PARTITION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(partition) = partition_guard.as_mut() else {
        return Ok(None);
    };
    let mut best: Option<DecodedSlot> = None;
    for slot in 0..SLOT_COUNT as u8 {
        if let Some(decoded) = read_slot(partition, slot, &mut buffer)?
            && best
                .as_ref()
                .is_none_or(|current| generation_is_newer(decoded.generation, current.generation))
        {
            best = Some(decoded);
        }
    }
    Ok(best)
}

fn write_slot(slot: u8, blob: &mut [u8]) -> AppResult<()> {
    let mut partition_guard = HOLDING_PARTITION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let partition = partition_guard
        .as_mut()
        .ok_or_else(|| AppError::Config("holding partition unavailable".into()))?;
    let offset = slot as usize * SLOT_BYTES;
    partition
        .erase(offset, SLOT_BYTES)
        .map_err(|error| AppError::Config(format!("erase raw slot {slot}: {error:?}")))?;
    partition
        .write(offset, &blob[..RAW_TOTAL_BYTES])
        .map_err(|error| AppError::Config(format!("write raw slot {slot}: {error:?}")))?;

    blob.fill(0);
    partition
        .read(offset, &mut blob[..RAW_TOTAL_BYTES])
        .map_err(|error| AppError::Config(format!("verify raw slot {slot}: {error:?}")))?;
    decode_raw_blob(blob, slot)
        .ok_or_else(|| AppError::Config(format!("raw slot {slot} verification failed")))?;
    Ok(())
}

fn read_slot(
    partition: &mut EspPartition,
    slot: u8,
    buffer: &mut [u8],
) -> AppResult<Option<DecodedSlot>> {
    buffer.fill(0);
    partition
        .read(slot as usize * SLOT_BYTES, &mut buffer[..RAW_TOTAL_BYTES])
        .map_err(|error| AppError::Config(format!("read raw slot {slot}: {error:?}")))?;
    Ok(decode_raw_blob(buffer, slot))
}

fn encode_raw_header(blob: &mut [u8], generation: u32) {
    blob[0..4].copy_from_slice(&RAW_MAGIC.to_le_bytes());
    blob[4..6].copy_from_slice(&RAW_VERSION.to_le_bytes());
    blob[6..8].copy_from_slice(&(HOLDING_WORDS as u16).to_le_bytes());
    blob[8..12].copy_from_slice(&generation.to_le_bytes());
    let crc = crate::device::crc32(&blob[RAW_HEADER_BYTES..RAW_TOTAL_BYTES]);
    blob[12..16].copy_from_slice(&crc.to_le_bytes());
}

fn decode_raw_blob(blob: &[u8], slot: u8) -> Option<DecodedSlot> {
    if blob.len() < LEGACY_RAW_TOTAL_BYTES
        || u32::from_le_bytes(blob[0..4].try_into().ok()?) != RAW_MAGIC
        || u16::from_le_bytes(blob[6..8].try_into().ok()?) as usize != HOLDING_WORDS
    {
        return None;
    }
    let version = u16::from_le_bytes(blob[4..6].try_into().ok()?);
    if version != RAW_VERSION && version != LEGACY_RAW_VERSION {
        return None;
    }
    let generation = u32::from_le_bytes(blob[8..12].try_into().ok()?);
    let stored_crc = u32::from_le_bytes(blob[12..16].try_into().ok()?);
    let data_len = if version == LEGACY_RAW_VERSION {
        HOLDING_DATA_BYTES
    } else {
        RAW_TOTAL_BYTES - RAW_HEADER_BYTES
    };
    if blob.len() < RAW_HEADER_BYTES + data_len {
        return None;
    }
    let data = &blob[RAW_HEADER_BYTES..RAW_HEADER_BYTES + data_len];
    if crate::device::crc32(data) != stored_crc {
        return None;
    }
    let words = data[..HOLDING_DATA_BYTES]
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    let (monitor_words, control_words, legacy_coils) = if version == LEGACY_RAW_VERSION {
        (
            vec![0u16; MONITOR_WORDS],
            vec![0u16; CONTROL_WORDS],
            vec![0u8; LEGACY_COIL_BYTES],
        )
    } else {
        let mut offset = HOLDING_DATA_BYTES;
        let monitor_words = data[offset..offset + MONITOR_DATA_BYTES]
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        offset += MONITOR_DATA_BYTES;
        let control_words = data[offset..offset + CONTROL_DATA_BYTES]
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        offset += CONTROL_DATA_BYTES;
        (
            monitor_words,
            control_words,
            data[offset..offset + LEGACY_COIL_BYTES].to_vec(),
        )
    };
    Some(DecodedSlot {
        slot,
        generation,
        words,
        monitor_words,
        control_words,
        legacy_coils,
    })
}

fn generation_is_newer(candidate: u32, current: u32) -> bool {
    candidate != current && candidate.wrapping_sub(current) < (1u32 << 31)
}

fn load_legacy_nvs(nvs: &EspDefaultNvs) -> AppResult<Option<Vec<u16>>> {
    let active = nvs.get_u8(LEGACY_NVS_KEY_ACTIVE).ok().flatten();
    let first = if matches!(active, Some(1)) {
        LEGACY_NVS_KEY_BLOB_B
    } else {
        LEGACY_NVS_KEY_BLOB_A
    };
    let second = if first == LEGACY_NVS_KEY_BLOB_A {
        LEGACY_NVS_KEY_BLOB_B
    } else {
        LEGACY_NVS_KEY_BLOB_A
    };
    if let Some(words) = try_load_legacy_blob(nvs, first)? {
        return Ok(Some(words));
    }
    try_load_legacy_blob(nvs, second)
}

fn try_load_legacy_blob(nvs: &EspDefaultNvs, key: &str) -> AppResult<Option<Vec<u16>>> {
    let mut buffer = HOLDING_BUFFER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    buffer.fill(0);
    let bytes = match nvs
        .get_blob(key, buffer.as_mut())
        .map_err(|error| AppError::Config(format!("nvs get_blob {key}: {error:?}")))?
    {
        Some(bytes) if bytes.len() == LEGACY_TOTAL_BYTES => bytes,
        _ => return Ok(None),
    };
    if u16::from_le_bytes([bytes[0], bytes[1]]) != LEGACY_MAGIC
        || u16::from_le_bytes([bytes[2], bytes[3]]) != LEGACY_VERSION
    {
        return Ok(None);
    }
    let stored_crc = u32::from_le_bytes(bytes[4..8].try_into().expect("fixed legacy header"));
    let data = &bytes[LEGACY_HEADER_BYTES..LEGACY_TOTAL_BYTES];
    if crate::device::crc32(data) != stored_crc {
        return Ok(None);
    }
    Ok(Some(
        data.chunks_exact(2)
            .map(|word| u16::from_le_bytes([word[0], word[1]]))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_blob_roundtrip_and_crc() {
        let mut blob = vec![0u8; RAW_TOTAL_BYTES];
        for index in 0..HOLDING_WORDS {
            let offset = RAW_HEADER_BYTES + index * 2;
            blob[offset..offset + 2]
                .copy_from_slice(&(index as u16).wrapping_mul(37).to_le_bytes());
        }
        let monitor_offset = RAW_HEADER_BYTES + HOLDING_DATA_BYTES;
        blob[monitor_offset..monitor_offset + 2].copy_from_slice(&0x1234u16.to_le_bytes());
        let control_offset = monitor_offset + MONITOR_DATA_BYTES;
        blob[control_offset..control_offset + 2].copy_from_slice(&0x5678u16.to_le_bytes());
        let coils_offset = control_offset + CONTROL_DATA_BYTES;
        blob[coils_offset + 17] = 1;
        encode_raw_header(&mut blob, 42);
        let decoded = decode_raw_blob(&blob, 1).expect("valid raw blob");
        assert_eq!(decoded.slot, 1);
        assert_eq!(decoded.generation, 42);
        assert_eq!(decoded.words[100], 3700);
        assert_eq!(decoded.monitor_words[0], 0x1234);
        assert_eq!(decoded.control_words[0], 0x5678);
        assert_eq!(decoded.legacy_coils[17], 1);

        blob[RAW_HEADER_BYTES + 7] ^= 0x80;
        assert!(decode_raw_blob(&blob, 1).is_none());
    }

    #[test]
    fn test_v1_raw_slot_loads_with_zeroed_legacy_state() {
        let mut blob = vec![0u8; RAW_TOTAL_BYTES];
        blob[0..4].copy_from_slice(&RAW_MAGIC.to_le_bytes());
        blob[4..6].copy_from_slice(&LEGACY_RAW_VERSION.to_le_bytes());
        blob[6..8].copy_from_slice(&(HOLDING_WORDS as u16).to_le_bytes());
        blob[8..12].copy_from_slice(&7u32.to_le_bytes());
        blob[RAW_HEADER_BYTES..RAW_HEADER_BYTES + 2].copy_from_slice(&0xABCDu16.to_le_bytes());
        let crc =
            crate::device::crc32(&blob[RAW_HEADER_BYTES..RAW_HEADER_BYTES + HOLDING_DATA_BYTES]);
        blob[12..16].copy_from_slice(&crc.to_le_bytes());

        let decoded = decode_raw_blob(&blob, 0).expect("v1 raw slot remains readable");
        assert_eq!(decoded.words[0], 0xABCD);
        assert!(decoded.monitor_words.iter().all(|&value| value == 0));
        assert!(decoded.control_words.iter().all(|&value| value == 0));
        assert!(decoded.legacy_coils.iter().all(|&value| value == 0));
    }

    #[test]
    fn test_generation_wrap_order() {
        assert!(generation_is_newer(11, 10));
        assert!(!generation_is_newer(10, 11));
        assert!(generation_is_newer(0, u32::MAX));
    }

    #[test]
    fn test_slot_geometry() {
        assert!(RAW_TOTAL_BYTES <= SLOT_BYTES);
        assert!(SLOT_COUNT * SLOT_BYTES <= PARTITION_MIN_BYTES);
        assert_eq!(HOLDING_DATA_BYTES, 4096);
    }
}
