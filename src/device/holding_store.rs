//! holding_buf NVS 持久化 (LOOP13)
//!
//! # 设计目标
//!
//! 修复 NVS 持久化全链路审计发现的 holding_buf 永久丢失问题:
//! FUNC_COUNT (0x08FC)、SENSOR_MIN/MAX (2280-2295)、用户自定义 P 区
//! (0x0880-0x107F) 全部存于 holding_buf, 之前仅在 NFC 标签在场时恢复,
//! 标签不在场时重启后归零.
//!
//! # 方案
//!
//! 复用 proto blob 的 A/B 双 blob 切换模式:
//! - NVS keys: `hld_buf_a`, `hld_buf_b`, `hld_act` (active flag)
//! - 写入: 检查 `HOLDING_DIRTY` 原子标志, 仅在 dirty=true 时执行;
//!   序列化 → A/B 切换写 → 切换 active flag → 清 dirty
//! - 启动: init() 中加载, magic 校验失败/CRC 错误回退默认值
//! - 触发点: `bus::backends::storage_modify_holding()` 改 holding_buf 后
//!   自动置 dirty
//!
//! # 与 proto blob 的差异
//!
//! proto blob 数据 3000 字节, holding_buf 4096 字节. 同样 8B header,
//! 总 blob 4104 字节. CRC32 覆盖 data 区不含 header.

use std::sync::{LazyLock, atomic::Ordering};

use esp_idf_svc::nvs::EspDefaultNvs;

use crate::bus::storage_state::{HOLDING_DIRTY, HOLDING_NVS_VALID, storage_read_with};
use crate::error::{AppError, AppResult};
use crate::sync::Spin;

// ---- NVS keys ----
/// blob A (header + 4096B data)
const NVS_KEY_BLOB_A: &str = "hld_buf_a";
/// blob B
const NVS_KEY_BLOB_B: &str = "hld_buf_b";
/// 当前 active blob (0=A, 1=B)
const NVS_KEY_ACTIVE: &str = "hld_act";

/// holding_buf 持久化 magic ('H'<<8 | 'B' = 0x4842)
const HOLDING_MAGIC: u16 = 0x4842;
/// blob format 版本
const HOLDING_BLOB_VERSION: u16 = 1;

/// header: magic(2) + version(2) + crc32(4) = 8 字节
const HOLDING_HEADER_BYTES: usize = 8;
/// holding_buf 字数 (对齐 StorageSnapshot 长度)
pub const HOLDING_WORDS: usize = 2048;
/// holding_buf 字节数
pub const HOLDING_DATA_BYTES: usize = HOLDING_WORDS * 2; // 4096
/// blob 完整大小
pub const HOLDING_BLOB_TOTAL: usize = HOLDING_HEADER_BYTES + HOLDING_DATA_BYTES; // 4104

/// 保存/加载共用工作区。4104B 超过 ALWAYSINTERNAL 阈值，优先进入 PSRAM；
/// 首次分配后永久复用，NVS 周期内不再反复申请大块内部堆。
static HOLDING_BLOB_BUFFER: LazyLock<Spin<Box<[u8]>>> =
    LazyLock::new(|| Spin::new(vec![0u8; HOLDING_BLOB_TOTAL].into_boxed_slice()));

/// 把当前 holding_buf (RCU 快照) 写入 NVS A/B 双 blob.
///
/// - 若 `HOLDING_DIRTY == false` 则直接 return Ok(()) (无变化, 跳过写).
/// - 在短 RCU reader 内直接序列化到复用缓冲 → A/B 切换写。
/// - 写失败时保留 dirty 标志 (下次 actor idle 重试).
pub fn save_to_nvs() -> AppResult<()> {
    // 先消费当前 dirty。若保存期间又发生写入，写者会重新置 true，不能在保存
    // 结束时无条件清除，否则会丢失并发写入的下一次持久化机会。
    if !HOLDING_DIRTY.swap(false, Ordering::AcqRel) {
        return Ok(());
    }

    // 1. 直接序列化，避免先深拷贝 4KB holding 再创建 4KB blob。
    let mut blob = HOLDING_BLOB_BUFFER.lock();
    blob.fill(0);
    blob[0..2].copy_from_slice(&HOLDING_MAGIC.to_le_bytes());
    blob[2..4].copy_from_slice(&HOLDING_BLOB_VERSION.to_le_bytes());
    let encoded = storage_read_with(|s| {
        if s.holding_buf.len() != HOLDING_WORDS {
            return false;
        }
        for (i, &v) in s.holding_buf.iter().enumerate() {
            let bytes = v.to_le_bytes();
            let off = HOLDING_HEADER_BYTES + 2 * i;
            blob[off..off + 2].copy_from_slice(&bytes);
        }
        true
    })
    .unwrap_or(false);
    if !encoded {
        log::warn!("[holding] RCU storage unavailable or invalid length");
        HOLDING_DIRTY.store(true, Ordering::Release);
        return Ok(());
    }
    let crc = crate::device::crc32(&blob[HOLDING_HEADER_BYTES..]);
    blob[4..8].copy_from_slice(&crc.to_le_bytes());

    // 2. A/B 切换写入 (复用 proto 模式)
    let write_result = crate::device::try_with_nvs_mut(|nvs| -> AppResult<()> {
        let active = nvs.get_u8(NVS_KEY_ACTIVE).ok().flatten().unwrap_or(0);
        let write_key = if active == 0 {
            NVS_KEY_BLOB_B
        } else {
            NVS_KEY_BLOB_A
        };
        let new_active = if active == 0 { 1u8 } else { 0u8 };

        nvs.set_blob(write_key, blob.as_ref())
            .map_err(|e| AppError::Config(format!("nvs set_blob {write_key}: {e:?}")))?;
        nvs.set_u8(NVS_KEY_ACTIVE, new_active)
            .map_err(|e| AppError::Config(format!("nvs set active: {e:?}")))?;
        Ok(())
    });

    // 3. 仅在 NVS 写入明确成功时清 dirty; Some(Err) 和 None 都保留,
    //    否则一次 flash/NVS 瞬态错误会让未落盘数据永久失去重试机会.
    match &write_result {
        Some(Ok(())) => {
            HOLDING_NVS_VALID.store(true, Ordering::Release);
            log::debug!("[holding] persisted {} words to NVS", HOLDING_WORDS);
        }
        Some(Err(e)) => {
            HOLDING_DIRTY.store(true, Ordering::Release);
            log::warn!("[holding] NVS write failed: {e}, dirty retained for retry");
        }
        None => {
            HOLDING_DIRTY.store(true, Ordering::Release);
            log::warn!("[holding] NVS unavailable, dirty retained for retry");
        }
    }
    write_result.unwrap_or(Ok(()))
}

/// 从 NVS 加载 holding_buf. 双 blob 容错: active 优先, 失败回退 inactive.
/// 全部失败时返回默认全 0 (4096B).
pub fn load_from_nvs(nvs: &EspDefaultNvs) -> AppResult<Vec<u16>> {
    let active = nvs.get_u8(NVS_KEY_ACTIVE).ok().flatten();
    let first = match active {
        Some(0) | None => NVS_KEY_BLOB_A,
        Some(_) => NVS_KEY_BLOB_B,
    };
    let second = if first == NVS_KEY_BLOB_A {
        NVS_KEY_BLOB_B
    } else {
        NVS_KEY_BLOB_A
    };

    if let Some(data) = try_load_one(nvs, first)? {
        HOLDING_NVS_VALID.store(true, Ordering::Release);
        return Ok(data);
    }
    log::warn!("[holding] active blob corrupted/missing, trying inactive");
    if let Some(data) = try_load_one(nvs, second)? {
        HOLDING_NVS_VALID.store(true, Ordering::Release);
        return Ok(data);
    }
    log::warn!("[holding] both blobs corrupted/missing, using defaults");
    HOLDING_NVS_VALID.store(false, Ordering::Release);
    Ok(vec![0u16; HOLDING_WORDS])
}

/// 从指定 NVS blob key 读取并校验, 成功则返回 Some(Vec<u16>), 失败返回 None.
fn try_load_one(nvs: &EspDefaultNvs, key: &str) -> AppResult<Option<Vec<u16>>> {
    let mut buf = HOLDING_BLOB_BUFFER.lock();
    buf.fill(0);
    let blob = nvs
        .get_blob(key, buf.as_mut())
        .map_err(|e| AppError::Config(format!("nvs get_blob {key}: {e:?}")))?;
    let bytes = match blob {
        Some(b) if b.len() == HOLDING_BLOB_TOTAL => b,
        _ => return Ok(None),
    };

    // magic 校验
    let magic = u16::from_le_bytes([bytes[0], bytes[1]]);
    if magic != HOLDING_MAGIC {
        return Ok(None);
    }
    // version 校验
    let version = u16::from_le_bytes([bytes[2], bytes[3]]);
    if version != HOLDING_BLOB_VERSION {
        return Ok(None);
    }

    // CRC 校验 (覆盖 data 区)
    let stored_crc = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let calc_crc = crate::device::crc32(&bytes[HOLDING_HEADER_BYTES..]);
    if stored_crc != calc_crc {
        log::warn!(
            "[holding] CRC mismatch in {key}: stored={:#x} calc={:#x}",
            stored_crc,
            calc_crc
        );
        return Ok(None);
    }

    // 反序列化
    let mut data = vec![0u16; HOLDING_WORDS];
    for i in 0..HOLDING_WORDS {
        data[i] = u16::from_le_bytes([
            bytes[HOLDING_HEADER_BYTES + 2 * i],
            bytes[HOLDING_HEADER_BYTES + 2 * i + 1],
        ]);
    }
    log::info!(
        "[holding] loaded {} words from NVS key '{}'",
        HOLDING_WORDS,
        key
    );
    Ok(Some(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_holding_constants() {
        assert_eq!(HOLDING_WORDS, 2048);
        assert_eq!(HOLDING_DATA_BYTES, 4096);
        assert_eq!(HOLDING_BLOB_TOTAL, 4104);
        assert_eq!(HOLDING_HEADER_BYTES, 8);
        assert_eq!(HOLDING_MAGIC, 0x4842);
    }

    #[test]
    fn test_blob_serialize_deserialize_roundtrip() {
        // 模拟: holding_buf = vec![0x1234, 0xABCD, ...]
        let mut data = vec![0u16; HOLDING_WORDS];
        for i in 0..HOLDING_WORDS {
            data[i] = (i as u16).wrapping_mul(0xBEEF);
        }

        let mut blob = [0u8; HOLDING_BLOB_TOTAL];
        blob[0..2].copy_from_slice(&HOLDING_MAGIC.to_le_bytes());
        blob[2..4].copy_from_slice(&HOLDING_BLOB_VERSION.to_le_bytes());
        let mut data_bytes = [0u8; HOLDING_DATA_BYTES];
        for (i, &v) in data.iter().enumerate() {
            data_bytes[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        let crc = crate::device::crc32(&data_bytes);
        blob[4..8].copy_from_slice(&crc.to_le_bytes());
        blob[HOLDING_HEADER_BYTES..].copy_from_slice(&data_bytes);

        // 反序列化
        let magic = u16::from_le_bytes([blob[0], blob[1]]);
        assert_eq!(magic, HOLDING_MAGIC);
        let version = u16::from_le_bytes([blob[2], blob[3]]);
        assert_eq!(version, HOLDING_BLOB_VERSION);
        let stored_crc = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]);
        let calc_crc = crate::device::crc32(&blob[HOLDING_HEADER_BYTES..]);
        assert_eq!(stored_crc, calc_crc);

        let mut decoded = vec![0u16; HOLDING_WORDS];
        for i in 0..HOLDING_WORDS {
            decoded[i] = u16::from_le_bytes([
                blob[HOLDING_HEADER_BYTES + 2 * i],
                blob[HOLDING_HEADER_BYTES + 2 * i + 1],
            ]);
        }
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_blob_crc_detect_corruption() {
        let mut blob = [0u8; HOLDING_BLOB_TOTAL];
        blob[0..2].copy_from_slice(&HOLDING_MAGIC.to_le_bytes());
        blob[2..4].copy_from_slice(&HOLDING_BLOB_VERSION.to_le_bytes());
        // crc = 0
        blob[4..8].copy_from_slice(&0u32.to_le_bytes());
        // data 区第一个字节设为非零
        blob[HOLDING_HEADER_BYTES] = 0xFF;

        let stored_crc = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]);
        let calc_crc = crate::device::crc32(&blob[HOLDING_HEADER_BYTES..]);
        assert_ne!(
            stored_crc, calc_crc,
            "CRC mismatch should detect corruption"
        );
    }
}
