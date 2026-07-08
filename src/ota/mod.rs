//! OTA 升级 (基于 ESP-IDF `esp_ota_*` API)
//!
//! 通过 BLE AT 命令或 Modbus 寄存器触发, 把新固件写入 `ota_0` / `ota_1` 分区,
//! 写完后切换启动分区并重启。
//!
//! 分区表 (`partitions.csv`):
//! ```text
//! factory    app   factory  0x20000   0x300000  (3MB, 当前固件)
//! ota_0      app   ota_0    0x320000  0x240000  (2.25MB, 升级槽 0)
//! ota_1      app   ota_1    0x560000  0x240000  (2.25MB, 升级槽 1)
//! otadata    data  ota      0x7A0000  0x2000    (记录当前启动分区)
//! ```
//!
//! # 升级流程
//!
//! 1. `AT+OTA=BEGIN,<total_size>` — 选择下一个 OTA 分区并开始升级
//! 2. `AT+OTA=WRITE,<hex_chunk>` — 写入数据块 (hex 编码, 单帧 ≤ 1024 字节)
//! 3. `AT+OTA=END` — 结束升级, 设置启动分区, 500ms 后重启
//! 4. `AT+OTA=ABORT` — 中止升级 (回到原分区)
//! 5. `AT+OTA=STATUS` — 查询状态 (`OK status=N,written=XX,total=YY`)
//!
//! 也可通过 Modbus 寄存器 0x0107-0x010F 触发 (见 `config::regs`)。
//!
//! # 状态机
//!
//! ```text
//! Idle ─BEGIN→ Receiving ─END→ DonePendingReboot ─REBOOT→ Idle (新固件)
//!                  │
//!                  └─ABORT→ Idle (回滚)
//! ```

use parking_lot::Mutex;
use once_cell::sync::Lazy;

use crate::error::{AppError, AppResult};

/// 单次 OTA write 最大字节数 (ESP-IDF 内部 buffer 限制)
pub const OTA_CHUNK_MAX: usize = 4096;
/// 单次 AT 命令 hex 数据最大长度 (AT+OTA=WRITE 后的 hex 字符数 / 2)
/// 受 BLE MTU 限制 (默认 23, 协商后 250), 单帧 hex 最多 100 字节 = 50 字节 binary
pub const OTA_AT_CHUNK_MAX: usize = 512;

/// OTA 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaStatus {
    /// 空闲
    Idle = 0,
    /// 接收中
    Receiving = 1,
    /// 写入完成, 等待重启
    DonePendingReboot = 2,
    /// 校验失败 (esp_ota_end 失败, partition 校验不通过)
    VerifyFailed = 3,
    /// 空间不足
    NoSpace = 4,
    /// 已中止
    Aborted = 5,
}

impl OtaStatus {
    pub fn as_u16(self) -> u16 {
        self as u16
    }
}

/// OTA 会话 (运行中持有 handle)
struct OtaSession {
    handle: esp_idf_sys::esp_ota_handle_t,
    total_size: u32,
    written: u32,
    /// 上次错误状态 (写入失败时设置, 用于 STATUS 读取)
    last_status: OtaStatus,
}

/// 全局 OTA 会话 (同时只允许一个升级)
static SESSION: Lazy<Mutex<Option<OtaSession>>> = Lazy::new(|| Mutex::new(None));

/// 待升级包总大小 (通过 Modbus TOTAL_LO/HI 写入, BEGIN 触发时使用)
static PENDING_TOTAL: parking_lot::Mutex<u32> = parking_lot::Mutex::new(0);

/// 设置待升级包总大小 (供 Modbus 写 TOTAL_LO/HI 调用)
pub fn set_pending_total(size: u32) {
    *PENDING_TOTAL.lock() = size;
}

/// 读取待升级包总大小
pub fn pending_total() -> u32 {
    *PENDING_TOTAL.lock()
}

/// 查询当前 OTA 状态
pub fn status() -> OtaStatus {
    let s = SESSION.lock();
    match &*s {
        None => OtaStatus::Idle,
        Some(sess) => {
            if sess.last_status != OtaStatus::Receiving {
                sess.last_status
            } else if sess.total_size > 0 && sess.written >= sess.total_size {
                OtaStatus::DonePendingReboot
            } else {
                OtaStatus::Receiving
            }
        }
    }
}

/// 查询已写入字节数
pub fn written_bytes() -> u32 {
    SESSION.lock().as_ref().map(|s| s.written).unwrap_or(0)
}

/// 查询总字节数
pub fn total_bytes() -> u32 {
    SESSION.lock().as_ref().map(|s| s.total_size).unwrap_or(0)
}

/// 开始 OTA 升级
///
/// 选择下一个 OTA 分区 (`esp_ota_get_next_update_partition`),
/// 调用 `esp_ota_begin` 启动升级。
///
/// `total_size` 为固件总字节数 (用于校验写入完整性, 0 表示未知)。
pub fn begin(total_size: u32) -> AppResult<()> {
    let mut s = SESSION.lock();
    if s.is_some() {
        return Err(AppError::Ota("ota already in progress".into()));
    }

    // 1. 获取下一个 OTA partition
    let partition =
        unsafe { esp_idf_sys::esp_ota_get_next_update_partition(std::ptr::null()) };
    if partition.is_null() {
        return Err(AppError::Ota("no ota partition available".into()));
    }

    // 2. 读取 partition 大小, 校验空间
    let part_size = unsafe { (*partition).size };
    if total_size > 0 && total_size > part_size {
        return Err(AppError::Ota(format!(
            "size {} > partition size {}",
            total_size, part_size
        )));
    }

    // 3. esp_ota_begin (SIZE_WITH_FLASH 大小用 0xFFFFFFFF 表示自动)
    let mut handle: esp_idf_sys::esp_ota_handle_t = 0;
    let r = unsafe { esp_idf_sys::esp_ota_begin(partition, total_size as usize, &mut handle) };
    if r != 0 {
        return Err(AppError::Ota(format!(
            "esp_ota_begin failed: 0x{:08X}",
            r
        )));
    }

    *s = Some(OtaSession {
        handle,
        total_size,
        written: 0,
        last_status: OtaStatus::Receiving,
    });

    log::info!(
        "[ota] begin: total={} bytes, partition_size={}",
        total_size,
        part_size
    );
    Ok(())
}

/// 写入升级数据块
///
/// `data` 为原始二进制 (调用方负责解码, 例如 AT 命令传 hex 字符串)。
/// 单次最多 `OTA_CHUNK_MAX` (4KB)。
pub fn write_chunk(data: &[u8]) -> AppResult<usize> {
    if data.len() > OTA_CHUNK_MAX {
        return Err(AppError::Ota(format!(
            "chunk too large: {} > {}",
            data.len(),
            OTA_CHUNK_MAX
        )));
    }

    let mut s = SESSION.lock();
    let sess = s
        .as_mut()
        .ok_or_else(|| AppError::Ota("ota not started".into()))?;

    if sess.last_status != OtaStatus::Receiving {
        return Err(AppError::Ota(format!(
            "ota not in receiving state: {:?}",
            sess.last_status
        )));
    }

    let r = unsafe {
        esp_idf_sys::esp_ota_write(sess.handle, data.as_ptr() as *const _, data.len())
    };
    if r != 0 {
        sess.last_status = OtaStatus::VerifyFailed;
        return Err(AppError::Ota(format!(
            "esp_ota_write failed: 0x{:08X}",
            r
        )));
    }

    sess.written = sess.written.saturating_add(data.len() as u32);
    Ok(data.len())
}

/// 结束升级并设置启动分区
///
/// 调用 `esp_ota_end` (内部会校验固件 header + checksum) +
/// `esp_ota_set_boot_partition`。成功后调用方应触发 `esp_restart`。
pub fn end() -> AppResult<()> {
    let mut s = SESSION.lock();
    let sess = s
        .as_ref()
        .ok_or_else(|| AppError::Ota("ota not started".into()))?;

    if sess.total_size > 0 && sess.written < sess.total_size {
        return Err(AppError::Ota(format!(
            "incomplete: {}/{} bytes",
            sess.written, sess.total_size
        )));
    }

    let handle = sess.handle;
    let written = sess.written;
    let r = unsafe { esp_idf_sys::esp_ota_end(handle) };
    if r != 0 {
        if let Some(ref mut sess) = *s {
            sess.last_status = OtaStatus::VerifyFailed;
        }
        return Err(AppError::Ota(format!(
            "esp_ota_end failed: 0x{:08X}",
            r
        )));
    }

    // 设置启动分区 (esp_ota_get_next_update_partition 返回的就是下一个分区)
    let partition =
        unsafe { esp_idf_sys::esp_ota_get_next_update_partition(std::ptr::null()) };
    let r = unsafe { esp_idf_sys::esp_ota_set_boot_partition(partition) };
    if r != 0 {
        return Err(AppError::Ota(format!(
            "esp_ota_set_boot_partition failed: 0x{:08X}",
            r
        )));
    }

    if let Some(ref mut sess) = *s {
        sess.last_status = OtaStatus::DonePendingReboot;
    }

    log::info!(
        "[ota] end: {} bytes written, boot partition set, ready to reboot",
        written
    );
    Ok(())
}

/// 中止升级
///
/// 调用 `esp_ota_abort` 释放资源, 不改变启动分区 (下次启动仍走原固件)。
pub fn abort() -> AppResult<()> {
    let mut s = SESSION.lock();
    if let Some(sess) = s.take() {
        unsafe { esp_idf_sys::esp_ota_abort(sess.handle) };
        log::info!("[ota] aborted (was {}/{} bytes)", sess.written, sess.total_size);
    }
    Ok(())
}

/// 重启应用新固件 (异步延时 500ms, 给 AT 响应发送留时间)
///
/// 调用前应已调用 `end()` 成功。
pub fn reboot_to_new_firmware() -> ! {
    log::info!("[ota] rebooting to apply new firmware in 500ms...");
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(500));
        unsafe { esp_idf_sys::esp_restart() };
    });
    // 当前线程等待 esp_restart 生效
    std::thread::sleep(std::time::Duration::from_secs(2));
    unsafe { esp_idf_sys::esp_restart() };
}

/// 查询当前启动分区信息 (用于 AT+OTA=STATUS 响应)
///
/// 返回 (running_partition_label, next_partition_label)
pub fn partition_info() -> (&'static str, &'static str) {
    // 简化: 假设当前运行在 factory, 下一个升级槽为 ota_0
    // 实际应通过 esp_ota_get_running_partition + esp_ota_get_next_update_partition 读取
    ("factory", "ota_0")
}
