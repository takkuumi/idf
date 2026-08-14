//! OTA 升级 AT 命令处理
//!
//! 命令集:
//! - `AT+OTA=BEGIN,<total_size>` — 开始升级
//! - `AT+OTA=WRITE,<hex_chunk>` — 写入 hex 编码的数据块
//! - `AT+OTA=END` — 结束升级 + 设置启动分区
//! - `AT+OTA=ABORT` — 中止升级
//! - `AT+OTA=STATUS` — 查询状态 (`OK status=N,written=XX,total=YY`)
//! - `AT+OTA=REBOOT` — 重启应用新固件

use crate::ble_at::parser::{err, ok_data, ok_none};
use crate::ota::{self, OTA_AT_CHUNK_MAX};

/// AT+OTA=BEGIN,<total_size>
pub fn handle_ota_begin(args: &str) -> String {
    let total = match args.trim().parse::<u32>() {
        Ok(n) => n,
        Err(_) => return err(10, "usage: AT+OTA=BEGIN,<total_size>"),
    };
    ota::set_pending_total(total);
    match ota::begin(total) {
        Ok(()) => ok_data(&format!("begin total={}", total)),
        Err(e) => err(20, &format!("begin: {}", e)),
    }
}

/// AT+OTA=WRITE,<hex_chunk>
///
/// hex 字符串解码为二进制后写入。单帧最多 `OTA_AT_CHUNK_MAX` 字节 binary。
pub fn handle_ota_write(args: &str) -> String {
    let hex = args.trim();
    if hex.is_empty() {
        return err(10, "empty data");
    }
    if !hex.len().is_multiple_of(2) {
        return err(11, "odd hex length");
    }
    if hex.len() / 2 > OTA_AT_CHUNK_MAX {
        return err(
            12,
            &format!("chunk too large (max {} bytes)", OTA_AT_CHUNK_MAX),
        );
    }

    // hex → bytes (用 heapless::Vec 避免 heap 分配)
    let mut buf: heapless::Vec<u8, OTA_AT_CHUNK_MAX> = heapless::Vec::new();
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = match (bytes[i] as char).to_digit(16) {
            Some(v) => v as u8,
            None => return err(13, &format!("invalid hex at {}", i)),
        };
        let lo = match (bytes[i + 1] as char).to_digit(16) {
            Some(v) => v as u8,
            None => return err(13, &format!("invalid hex at {}", i + 1)),
        };
        if buf.push((hi << 4) | lo).is_err() {
            return err(14, "buffer overflow");
        }
        i += 2;
    }

    let n = buf.len();
    match ota::write_chunk(&buf) {
        Ok(written) => ok_data(&format!("written={},total={}", written, n)),
        Err(e) => err(20, &format!("write: {}", e)),
    }
}

/// AT+OTA=END
pub fn handle_ota_end(_args: &str) -> String {
    match ota::end() {
        Ok(()) => ok_data("ended, send AT+OTA=REBOOT to apply"),
        Err(e) => err(20, &format!("end: {}", e)),
    }
}

/// AT+OTA=ABORT
pub fn handle_ota_abort(_args: &str) -> String {
    match ota::abort() {
        Ok(()) => ok_data("aborted"),
        Err(e) => err(20, &format!("abort: {}", e)),
    }
}

/// AT+OTA=STATUS
pub fn handle_ota_status(_args: &str) -> String {
    let st = ota::status();
    let written = ota::written_bytes();
    let total = ota::total_bytes();
    ok_data(&format!(
        "status={},written={},total={}",
        st.as_u16(),
        written,
        total
    ))
}

/// AT+OTA=REBOOT (重启应用新固件)
pub fn handle_ota_reboot(_args: &str) -> String {
    let st = ota::status();
    if st != ota::OtaStatus::DonePendingReboot {
        return err(30, &format!("cannot reboot: status={}", st.as_u16()));
    }
    // 交给 main_loop 的 1s 调度点执行，避免为一次性延时申请 12KB pthread 栈。
    crate::bus::IO
        .sys
        .request_reset(crate::bus::io_state::ResetSource::BLE_OTA);
    ok_none()
}
