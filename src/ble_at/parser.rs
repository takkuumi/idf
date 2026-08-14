//! AT 命令解析器
//!
//! 把一行 AT 命令字符串解析为 (cmd, args), 调用 handlers 处理。
//!
//! 支持的命令见 [mod.rs](../mod.rs)

use crate::ble_at::cfg_handlers;
use crate::ble_at::handlers;
use crate::ble_at::ota_handlers;

/// 处理一条 AT 命令, 返回响应字符串 (含 \r\n)
pub fn process(line: &str) -> String {
    let line = line.trim();
    if line.is_empty() {
        return ok_none();
    }

    // 必须以 AT 开头 (大小写不敏感, 用 eq_ignore_ascii_case 避免分配)
    if line.len() < 2 || !line[..2].eq_ignore_ascii_case("AT") {
        return err(1, "not AT command");
    }

    // AT 单独出现
    if line.len() == 2 && line.eq_ignore_ascii_case("AT") {
        return ok_none();
    }

    // AT+XXX[=ARGS]；兼容不带加号的 ATXXX，但分派前必须剥离可选的 '+'。
    // 旧实现直接使用 line[2..]，导致标准命令 AT+VERSION 被解析成 "+VERSION"。
    let rest = &line[2..];
    let rest = rest.strip_prefix('+').unwrap_or(rest);
    let (cmd, args) = match rest.find(['=', '?']) {
        Some(pos) => {
            let sep = rest.as_bytes()[pos] as char;
            let cmd = &rest[..pos];
            let args = if sep == '?' { "" } else { &rest[pos + 1..] };
            (cmd, args)
        }
        None => (rest, ""),
    };

    log::debug!("[ble_at] cmd={} args={}", cmd, args);

    // 命令分派 (大小写不敏感, 避免分配 to_uppercase)
    if cmd.eq_ignore_ascii_case("READ") {
        handlers::handle_read(args)
    } else if cmd.eq_ignore_ascii_case("WRITE") {
        handlers::handle_write(args)
    } else if cmd.eq_ignore_ascii_case("BULKR") {
        handlers::handle_bulkr(args)
    } else if cmd.eq_ignore_ascii_case("BULKW") {
        handlers::handle_bulkw(args)
    } else if cmd.eq_ignore_ascii_case("COMMIT") {
        handlers::handle_commit(args)
    } else if cmd.eq_ignore_ascii_case("RELOAD") {
        handlers::handle_reload(args)
    } else if cmd.eq_ignore_ascii_case("INFO") {
        handlers::handle_info(args)
    } else if cmd.eq_ignore_ascii_case("STATUS") {
        handlers::handle_status(args)
    } else if cmd.eq_ignore_ascii_case("RESET") {
        handlers::handle_reset(args)
    } else if cmd.eq_ignore_ascii_case("VERSION") {
        handlers::handle_version(args)
    } else if cmd.eq_ignore_ascii_case("CFGSN") {
        cfg_handlers::handle_cfgsn(args)
    } else if cmd.eq_ignore_ascii_case("CFGNAME") {
        cfg_handlers::handle_cfgname(args)
    } else if cmd.eq_ignore_ascii_case("CFGIP") {
        cfg_handlers::handle_cfgip(args)
    } else if cmd.eq_ignore_ascii_case("CFGDHCP") {
        cfg_handlers::handle_cfgdhcp(args)
    } else if cmd.eq_ignore_ascii_case("CFGMAC") {
        cfg_handlers::handle_cfgmac(args)
    } else if cmd.eq_ignore_ascii_case("CFGBTMAC") {
        cfg_handlers::handle_cfgbtmac(args)
    } else if cmd.eq_ignore_ascii_case("CFGBTNAME") {
        cfg_handlers::handle_cfgbtname(args)
    } else if cmd.eq_ignore_ascii_case("CFG485") {
        cfg_handlers::handle_cfg485(args)
    } else if cmd.eq_ignore_ascii_case("CFGAPPLY") {
        cfg_handlers::handle_cfgapply(args)
    } else if cmd.eq_ignore_ascii_case("CFGRESET") {
        cfg_handlers::handle_cfgreset(args)
    } else if cmd.eq_ignore_ascii_case("CFGINFO") {
        cfg_handlers::handle_cfginfo(args)
    } else if cmd.eq_ignore_ascii_case("CFGREAD") {
        cfg_handlers::handle_cfgread(args)
    } else if cmd.eq_ignore_ascii_case("CFGWRITE") {
        cfg_handlers::handle_cfgwrite(args)
    } else if cmd.eq_ignore_ascii_case("OTA") {
        // AT+OTA=<sub>,<args>: sub=BEGIN/WRITE/END/ABORT/STATUS/REBOOT
        handle_ota(args)
    } else {
        err(2, "unknown command")
    }
}

// ----------------------------------------------------------------------------
// 响应构造助手
// ----------------------------------------------------------------------------

pub fn ok_none() -> String {
    "OK\r\n".to_string()
}

pub fn ok_data(data: &str) -> String {
    format!("OK {}\r\n", data)
}

pub fn err(code: u32, msg: &str) -> String {
    format!("ERROR {}: {}\r\n", code, msg)
}

/// 解析单个 u16: "1234" -> 1234, 或 "0x4D2" -> 1234
#[inline]
pub fn parse_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u16>().ok()
    }
}

/// 解析 u16 列表: "1,2,3" -> [1,2,3]
/// 用 heapless::Vec 避免 heap 分配 (Modbus 单帧最多 125 reg, AT+BULKW 上限 64)
pub fn parse_u16_list(s: &str) -> heapless::Vec<u16, 128> {
    let mut v = heapless::Vec::new();
    for part in s.split(',') {
        if let Some(n) = parse_u16(part) {
            // 容量 128 足够 AT+BULKW; 超出时丢弃后续 (调用方已校验)
            if v.push(n).is_err() {
                break;
            }
        }
    }
    v
}

// ----------------------------------------------------------------------------
// AT+OTA 路由 (BEGIN/WRITE/END/ABORT/STATUS/REBOOT)
// ----------------------------------------------------------------------------
fn handle_ota(args: &str) -> String {
    // 用迭代器拆分 sub 和 rest, 避免 Vec<&str> heap 分配
    let mut iter = args.splitn(2, ',');
    let sub = iter.next().unwrap_or("").trim();
    let rest = iter.next().unwrap_or("");

    if sub.eq_ignore_ascii_case("BEGIN") {
        ota_handlers::handle_ota_begin(rest)
    } else if sub.eq_ignore_ascii_case("WRITE") {
        ota_handlers::handle_ota_write(rest)
    } else if sub.eq_ignore_ascii_case("END") {
        ota_handlers::handle_ota_end(rest)
    } else if sub.eq_ignore_ascii_case("ABORT") {
        ota_handlers::handle_ota_abort(rest)
    } else if sub.eq_ignore_ascii_case("STATUS") {
        ota_handlers::handle_ota_status(rest)
    } else if sub.eq_ignore_ascii_case("REBOOT") {
        ota_handlers::handle_ota_reboot(rest)
    } else {
        err(2, "usage: AT+OTA=BEGIN/WRITE/END/ABORT/STATUS/REBOOT")
    }
}

// ============================================================================
// 单元测试
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_u16_decimal() {
        assert_eq!(parse_u16("1234"), Some(1234));
        assert_eq!(parse_u16("0"), Some(0));
        assert_eq!(parse_u16("65535"), Some(65535));
    }

    #[test]
    fn test_parse_u16_hex() {
        assert_eq!(parse_u16("0x100"), Some(256));
        assert_eq!(parse_u16("0XFF"), Some(255));
        assert_eq!(parse_u16("0xABCD"), Some(0xABCD));
    }

    #[test]
    fn test_parse_u16_invalid() {
        assert_eq!(parse_u16(""), None);
        assert_eq!(parse_u16("abc"), None);
        assert_eq!(parse_u16("99999999"), None); // overflow
    }

    #[test]
    fn test_parse_u16_list() {
        let v = parse_u16_list("1,2,3,0xFF");
        assert_eq!(
            v,
            heapless::Vec::<u16, 128>::from_slice(&[1, 2, 3, 255]).unwrap()
        );
    }

    #[test]
    fn test_response_formats() {
        assert_eq!(ok_none(), "OK\r\n");
        assert_eq!(ok_data("hello"), "OK hello\r\n");
        assert_eq!(err(10, "bad"), "ERROR 10: bad\r\n");
    }

    /// AT 命令分发测试 — 通过调用 process() 验证命令分发正确
    #[test]
    fn test_at_basic() {
        assert_eq!(process("AT"), "OK\r\n");
        assert_eq!(process("at"), "OK\r\n");
        assert_eq!(process("AT+VERSION"), "OK ");
        // AT+VERSION 实际值由 handlers::handle_version 返回, 这里仅验证前缀
        let resp = process("AT+VERSION");
        assert!(resp.starts_with("OK "));
    }

    #[test]
    fn test_at_unknown() {
        let resp = process("AT+BOGUS_CMD");
        assert!(resp.starts_with("ERROR 2:"));
    }

    #[test]
    fn test_at_invalid() {
        // 不以 AT 开头
        let resp = process("HELLO");
        assert!(resp.starts_with("ERROR 1:"));
    }

    #[test]
    fn test_at_without_plus() {
        // 兼容不带 + 的旧命令
        let resp = process("ATVERSION");
        assert!(resp.starts_with("OK "));
    }
}
