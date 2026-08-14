//! AT 命令处理函数
//!
//! 每个 handle_xxx 接收参数字符串, 返回响应字符串。

use crate::ble_at::parser::{err, ok_data, ok_none, parse_u16, parse_u16_list};
use crate::device;

// ----------------------------------------------------------------------------
// AT+READ=<addr>
// ----------------------------------------------------------------------------
pub fn handle_read(args: &str) -> String {
    let addr = match parse_u16(args) {
        Some(a) => a,
        None => return err(10, "invalid addr"),
    };
    match device::proto_read(addr) {
        Some(v) => ok_data(&format!("{},0x{:04X}", v, v)),
        None => err(11, "addr out of range"),
    }
}

// ----------------------------------------------------------------------------
// AT+WRITE=<addr>,<value>
// ----------------------------------------------------------------------------
pub fn handle_write(args: &str) -> String {
    // 用迭代器避免 Vec<&str> heap 分配
    let mut iter = args.splitn(2, ',');
    let addr = match parse_u16(iter.next().unwrap_or("")) {
        Some(a) => a,
        None => return err(10, "usage: AT+WRITE=<addr>,<value>"),
    };
    let value = match parse_u16(iter.next().unwrap_or("")) {
        Some(v) => v,
        None => return err(10, "invalid value"),
    };
    if device::proto_write(addr, value) {
        ok_none()
    } else {
        err(11, "addr out of range")
    }
}

// ----------------------------------------------------------------------------
// AT+BULKR=<start>,<len>
// 返回: OK <v1>,<v2>,...,<vN>\r\n
// 优化: 1500 个 U16 一次 alloc (避免 Vec<String> 1500 次 alloc/free)
// ----------------------------------------------------------------------------
pub fn handle_bulkr(args: &str) -> String {
    // 用迭代器解析前两段, 避免 Vec<&str> heap 分配
    let mut iter = args.splitn(2, ',');
    let start = match parse_u16(iter.next().unwrap_or("")) {
        Some(a) => a,
        None => return err(10, "invalid start"),
    };
    let len = match parse_u16(iter.next().unwrap_or("")) {
        Some(l) => l,
        None => return err(10, "invalid len"),
    };
    if len == 0 || len > 1500 {
        return err(12, "len out of range (1..=1500)");
    }
    let data = device::proto_read_bulk(start, len);
    if data.is_empty() {
        return err(11, "start out of range");
    }
    // 预分配: 每个 U16 最长 6 字节 ("0xXXXX,") + 头尾
    let cap = (data.len() * 6 + 16).min(16 * 1024);
    let mut s = String::with_capacity(cap);
    s.push_str("OK ");
    for (i, &v) in data.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        use std::fmt::Write;
        let _ = write!(s, "0x{:04X}", v);
    }
    s.push_str("\r\n");
    s
}

// ----------------------------------------------------------------------------
// AT+BULKW=<start>,<v1>,<v2>,...,<vN>
// ----------------------------------------------------------------------------
pub fn handle_bulkw(args: &str) -> String {
    let values = parse_u16_list(args);
    if values.len() < 2 {
        return err(10, "usage: AT+BULKW=<start>,<v1>,<v2>,...");
    }
    // 安全拆分: 用 match 替代 unwrap, 任何分支都不 panic
    let (start, rest) = match values.split_first() {
        Some((s, r)) => (s, r),
        None => return err(10, "usage: AT+BULKW=<start>,<v1>,<v2>,..."),
    };
    if device::proto_write_bulk(*start, rest) {
        ok_data(&format!("{} words", rest.len()))
    } else {
        err(11, "write failed")
    }
}

// ----------------------------------------------------------------------------
// AT+COMMIT
// 同步提交到 NVS (阻塞至完成)
// ----------------------------------------------------------------------------
pub fn handle_commit(_args: &str) -> String {
    match device::commit_sync() {
        Ok(_) => ok_data("committed"),
        Err(e) => err(20, &format!("commit: {}", e)),
    }
}

// ----------------------------------------------------------------------------
// AT+RELOAD
// 从 NVS 重载到 RAM
// ----------------------------------------------------------------------------
pub fn handle_reload(_args: &str) -> String {
    match device::reload_sync() {
        Ok(_) => ok_data("reloaded"),
        Err(e) => err(20, &format!("reload: {}", e)),
    }
}

// ----------------------------------------------------------------------------
// AT+INFO
// 返回存储区信息: capacity,version,length,dirty,status,magic
// ----------------------------------------------------------------------------
pub fn handle_info(_args: &str) -> String {
    let info = device::proto_info();
    ok_data(&format!(
        "cap={},ver={},len={},dirty={},status={},magic=0x{:04X},proto_base=0x{:04X}",
        info.capacity,
        info.version,
        info.length,
        info.dirty as u8,
        info.status,
        info.magic,
        crate::config::regs::PROTO_BASE
    ))
}

// ----------------------------------------------------------------------------
// AT+STATUS
// 系统状态: uptime, fw_ver, reset_cnt, di, do, ai(6), ao(4)
// DI/DO 位宽根据硬件版本变化 (默认 8 bit / F3-F4 16-48 bit)
// ----------------------------------------------------------------------------
pub fn handle_status(_args: &str) -> String {
    // 全部从 IO 原子读 (无锁), 不再过 legacy Bus (阶段 D)
    let io = &crate::bus::IO;
    let (uptime, fw, rst, di, do_, ai, ao) = (
        io.sys.get_uptime(),
        io.sys.get_fw_version(),
        io.sys.get_reset_count(),
        io.di.load_bits(),
        io.do_.load_bits(),
        [
            io.ai.get_raw(0),
            io.ai.get_raw(1),
            io.ai.get_raw(2),
            io.ai.get_raw(3),
            io.ai.get_raw(4),
            io.ai.get_raw(5),
        ],
        [
            io.ao.get_scaled(0),
            io.ao.get_scaled(1),
            io.ao.get_scaled(2),
            io.ao.get_scaled(3),
        ],
    );
    ok_data(&format!(
        "uptime={}s,fw=0x{:04X},rst={},ver={},di=0x{:X},do=0x{:X},ai=0x{:04X},0x{:04X},0x{:04X},0x{:04X},0x{:04X},0x{:04X},ao=0x{:04X},0x{:04X},0x{:04X},0x{:04X}",
        uptime,
        fw,
        rst,
        crate::config::hw_version::NAME,
        di,
        do_,
        ai[0],
        ai[1],
        ai[2],
        ai[3],
        ai[4],
        ai[5],
        ao[0],
        ao[1],
        ao[2],
        ao[3]
    ))
}

// ----------------------------------------------------------------------------
// AT+RESET
// 触发设备复位 (响应立即返回, 200ms 后异步复位, 给 AT 响应发送留时间)
// ----------------------------------------------------------------------------
pub fn handle_reset(_args: &str) -> String {
    // 直接置位 IO.sys 请求复位 (无锁)
    crate::bus::IO
        .sys
        .request_reset(crate::bus::io_state::ResetSource::BLE_AT);
    // main_loop 在响应入队后执行复位，不创建一次性 pthread。
    ok_data("resetting in 200ms")
}

// ----------------------------------------------------------------------------
// AT+VERSION
// 固件版本 + 硬件版本 (F3/F4/Default)
// ----------------------------------------------------------------------------
pub fn handle_version(_args: &str) -> String {
    ok_data(&format!(
        "{} v{} (HW: {})",
        crate::config::APP_NAME,
        crate::config::APP_VERSION,
        crate::config::hw_version::NAME,
    ))
}
