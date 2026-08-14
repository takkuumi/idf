//! OTA 升级 (基于 ESP-IDF `esp_ota_*` API)
//!
//! 通过 BLE AT 命令或 Modbus 寄存器触发, 把新固件写入 `ota_0` / `ota_1` 分区,
//! 写完后切换启动分区并重启。
//!
//! 分区表 (`partitions.csv`):
//! ```text
//! factory    app   factory  0x020000  0x240000  (2.25MB)
//! ota_0      app   ota_0    0x260000  0x240000  (2.25MB, 升级槽 0)
//! ota_1      app   ota_1    0x4A0000  0x240000  (2.25MB, 升级槽 1)
//! otadata    data  ota      0x010000  0x002000  (记录当前启动分区)
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

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

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

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Receiving,
            2 => Self::DonePendingReboot,
            3 => Self::VerifyFailed,
            4 => Self::NoSpace,
            5 => Self::Aborted,
            _ => Self::Idle,
        }
    }
}

/// OTA 会话 (运行中持有 handle)
struct OtaSession {
    handle: esp_idf_sys::esp_ota_handle_t,
    /// esp_ota_end 无论成功失败都会释放 handle；防止随后错误调用 esp_ota_abort。
    handle_active: bool,
    /// BEGIN 时选中的 OTA 分区地址，END 时复用同一槽位.
    partition_addr: usize,
    partition_size: u32,
    total_size: u32,
    written: u32,
    /// 上次错误状态 (写入失败时设置, 用于 STATUS 读取)
    last_status: OtaStatus,
}

/// 全局 OTA 会话。Flash 擦写可能持续毫秒到秒，禁止持有自旋锁，否则另一核心查询
/// STATUS 会持续忙等并饿死实时任务；系统 Mutex 会让竞争任务阻塞让出 CPU。
static SESSION: LazyLock<Mutex<Option<OtaSession>>> = LazyLock::new(|| Mutex::new(None));
static LAST_STATUS: AtomicU8 = AtomicU8::new(OtaStatus::Idle as u8);

fn lock_session() -> MutexGuard<'static, Option<OtaSession>> {
    // 生产固件 panic=abort，正常运行不会留下 poisoned mutex；测试构建若发生中毒，
    // 仍取回内部状态以便 abort 清理 OTA handle。
    SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 待升级包总大小 (通过 Modbus TOTAL_LO/HI 写入, BEGIN 触发时使用; 无锁原子)
static PENDING_TOTAL: AtomicU32 = AtomicU32::new(0);

/// 设置待升级包总大小 (供 Modbus 写 TOTAL_LO/HI 调用)
pub fn set_pending_total(size: u32) {
    PENDING_TOTAL.store(size, Ordering::Release);
}

/// 读取待升级包总大小
pub fn pending_total() -> u32 {
    PENDING_TOTAL.load(Ordering::Acquire)
}

/// 查询当前 OTA 状态
pub fn status() -> OtaStatus {
    let s = lock_session();
    match &*s {
        None => OtaStatus::from_u8(LAST_STATUS.load(Ordering::Acquire)),
        Some(sess) => sess.last_status,
    }
}

/// 查询已写入字节数
pub fn written_bytes() -> u32 {
    lock_session().as_ref().map(|s| s.written).unwrap_or(0)
}

/// 查询总字节数
pub fn total_bytes() -> u32 {
    lock_session().as_ref().map(|s| s.total_size).unwrap_or(0)
}

/// 开始 OTA 升级
///
/// 选择下一个 OTA 分区 (`esp_ota_get_next_update_partition`),
/// 调用 `esp_ota_begin` 启动升级。
///
/// `total_size` 为固件总字节数 (用于校验写入完整性, 0 表示未知)。
pub fn begin(total_size: u32) -> AppResult<()> {
    let mut s = lock_session();
    if s.is_some() {
        return Err(AppError::Ota("ota already in progress".into()));
    }

    // 1. 获取下一个 OTA partition
    let partition = unsafe { esp_idf_sys::esp_ota_get_next_update_partition(std::ptr::null()) };
    if partition.is_null() {
        return Err(AppError::Ota("no ota partition available".into()));
    }

    // 2. 读取 partition 大小, 校验空间
    let part_size = unsafe { (*partition).size };
    if total_size > 0 && total_size > part_size {
        LAST_STATUS.store(OtaStatus::NoSpace as u8, Ordering::Release);
        return Err(AppError::Ota(format!(
            "size {} > partition size {}",
            total_size, part_size
        )));
    }

    // 3. 未知大小使用 OTA_WITH_SEQUENTIAL_WRITES，边写边擦除，避免一次擦除整个
    // 2.25MB 分区长时间阻塞。已知大小交给 IDF 精确擦除所需范围。
    const OTA_WITH_SEQUENTIAL_WRITES: usize = 0xFFFF_FFFE;
    let image_size = if total_size == 0 {
        OTA_WITH_SEQUENTIAL_WRITES
    } else {
        total_size as usize
    };
    let mut handle: esp_idf_sys::esp_ota_handle_t = 0;
    let r = unsafe { esp_idf_sys::esp_ota_begin(partition, image_size, &mut handle) };
    if r != 0 {
        return Err(AppError::Ota(format!("esp_ota_begin failed: 0x{:08X}", r)));
    }

    *s = Some(OtaSession {
        handle,
        handle_active: true,
        partition_addr: partition as usize,
        partition_size: part_size,
        total_size,
        written: 0,
        last_status: OtaStatus::Receiving,
    });
    LAST_STATUS.store(OtaStatus::Receiving as u8, Ordering::Release);

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

    let mut s = lock_session();
    let sess = s
        .as_mut()
        .ok_or_else(|| AppError::Ota("ota not started".into()))?;

    if sess.last_status != OtaStatus::Receiving {
        return Err(AppError::Ota(format!(
            "ota not in receiving state: {:?}",
            sess.last_status
        )));
    }

    let next_written = sess
        .written
        .checked_add(data.len() as u32)
        .ok_or_else(|| AppError::Ota("written byte counter overflow".into()))?;
    // 即使 BEGIN 使用未知大小，也不能依赖底层写失败来发现越过分区边界。
    let limit = if sess.total_size > 0 {
        sess.total_size.min(sess.partition_size)
    } else {
        sess.partition_size
    };
    if next_written > limit {
        return Err(AppError::Ota(format!(
            "write would overflow: written={}, chunk={}, limit={limit}",
            sess.written,
            data.len()
        )));
    }

    let r =
        unsafe { esp_idf_sys::esp_ota_write(sess.handle, data.as_ptr() as *const _, data.len()) };
    if r != 0 {
        let handle = sess.handle;
        let _ = unsafe { esp_idf_sys::esp_ota_abort(handle) };
        *s = None;
        LAST_STATUS.store(OtaStatus::VerifyFailed as u8, Ordering::Release);
        return Err(AppError::Ota(format!("esp_ota_write failed: 0x{:08X}", r)));
    }

    sess.written = next_written;
    Ok(data.len())
}

/// 结束升级并设置启动分区
///
/// 调用 `esp_ota_end` (内部会校验固件 header + checksum) +
/// `esp_ota_set_boot_partition`。成功后调用方应触发 `esp_restart`。
pub fn end() -> AppResult<()> {
    let mut s = lock_session();
    let sess = s
        .as_mut()
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
    // ESP-IDF 明确规定：esp_ota_end 无论返回值如何，handle 都已释放。
    sess.handle_active = false;
    if r != 0 {
        *s = None;
        LAST_STATUS.store(OtaStatus::VerifyFailed as u8, Ordering::Release);
        return Err(AppError::Ota(format!("esp_ota_end failed: 0x{:08X}", r)));
    }

    // 保留 BEGIN 时选中的分区，不能在 end 阶段重新查询目标槽
    let partition = sess.partition_addr as *const esp_idf_sys::esp_partition_t;
    if partition.is_null() {
        *s = None;
        LAST_STATUS.store(OtaStatus::VerifyFailed as u8, Ordering::Release);
        return Err(AppError::Ota(
            "ota target partition unavailable after end".into(),
        ));
    }
    let r = unsafe { esp_idf_sys::esp_ota_set_boot_partition(partition) };
    if r != 0 {
        // esp_ota_end 已结束句柄，不能再 abort；清理会话避免卡死在 Receiving.
        *s = None;
        LAST_STATUS.store(OtaStatus::VerifyFailed as u8, Ordering::Release);
        return Err(AppError::Ota(format!(
            "esp_ota_set_boot_partition failed: 0x{:08X}",
            r
        )));
    }

    if let Some(ref mut sess) = *s {
        sess.last_status = OtaStatus::DonePendingReboot;
    }
    LAST_STATUS.store(OtaStatus::DonePendingReboot as u8, Ordering::Release);

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
    let mut s = lock_session();
    if let Some(sess) = s.take() {
        if sess.handle_active {
            let r = unsafe { esp_idf_sys::esp_ota_abort(sess.handle) };
            if r != esp_idf_sys::ESP_OK {
                return Err(AppError::Ota(format!("esp_ota_abort failed: 0x{r:08X}")));
            }
        }
        log::info!(
            "[ota] aborted (was {}/{} bytes)",
            sess.written,
            sess.total_size
        );
        LAST_STATUS.store(OtaStatus::Aborted as u8, Ordering::Release);
    }
    Ok(())
}

/// 重启应用新固件 (异步延时 500ms, 给 AT 响应发送留时间)
///
/// 调用前应已调用 `end()` 成功。
pub fn reboot_to_new_firmware() -> ! {
    log::info!("[ota] rebooting to apply new firmware in 500ms...");
    std::thread::sleep(std::time::Duration::from_millis(500));
    unsafe { esp_idf_sys::esp_restart() };
}

/// 查询当前启动分区信息 (用于 AT+OTA=STATUS 响应)
///
/// 返回 (running_partition_label, next_partition_label)
pub fn partition_info() -> (&'static str, &'static str) {
    let running = unsafe { esp_idf_sys::esp_ota_get_running_partition() };
    let next = unsafe { esp_idf_sys::esp_ota_get_next_update_partition(std::ptr::null()) };
    (partition_label(running), partition_label(next))
}

fn partition_label(partition: *const esp_idf_sys::esp_partition_t) -> &'static str {
    if partition.is_null() {
        return "none";
    }
    // ESP-IDF 分区描述来自静态 partition table，在整个固件生命周期内有效。
    let label = unsafe { std::ffi::CStr::from_ptr((*partition).label.as_ptr()) };
    label.to_str().unwrap_or("invalid")
}
