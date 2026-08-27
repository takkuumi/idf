//! 系统配置 AT 命令处理
//!
//! 提供用户友好的 AT 命令访问 SystemConfig, 与 Modbus 寄存器 0x0200-0x025F 等效。
//!
//! 命令集:
//!   AT+CFGSN=<sn>                    设置 SN (32 字符)
//!   AT+CFGSN                         读 SN
//!   AT+CFGNAME=<name>                设置设备名称 (16 字符)
//!   AT+CFGNAME                       读设备名称
//!   AT+CFGIP=<ip>,<mask>,<gw>[,<dns>] 设置网络 (静态)
//!   AT+CFGIP                         读网络配置
//!   AT+CFGDHCP=<0|1>                 启用/禁用 DHCP
//!   AT+CFGMAC                        读以太网 MAC
//!   AT+CFGBTMAC                      读蓝牙 MAC
//!   AT+CFGBTNAME=<name>              设置 BLE 名称 (8 字符)
//!   AT+CFGBTNAME                     读 BLE 名称
//!   AT+CFG485=<idx>,<baud>,<data>,<stop>,<parity>,<slave>,<mode>  设置 RS485
//!   AT+CFG485=<idx>                   读 RS485 配置
//!   AT+CFGAPPLY                      应用配置 (持久化 + 生效)
//!   AT+CFGRESET                      恢复默认配置
//!   AT+CFGINFO                       列出所有配置
//!   AT+CFGREAD=<addr>                按 Modbus 地址读 U16
//!   AT+CFGWRITE=<addr>,<value>        按 Modbus 地址写 U16

use crate::ble_at::parser::{err, ok_data, ok_none, parse_u16};
use crate::config::regs;
use crate::device::{self, SystemConfig, parse_ipv4};

// ----------------------------------------------------------------------------
// AT+CFGSN[=<sn>]
// ----------------------------------------------------------------------------
pub fn handle_cfgsn(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        // 读
        let sn = with_cfg(|c| c.sn_str());
        ok_data(&sn)
    } else {
        // 写
        if args.len() > 32 {
            return err(10, "sn too long (max 32)");
        }
        let mut sn = [0u8; 32];
        sn[..args.len()].copy_from_slice(args.as_bytes());
        with_cfg_mut(|c| c.sn = sn);
        ok_none()
    }
}

// ----------------------------------------------------------------------------
// AT+CFGNAME[=<name>]
// ----------------------------------------------------------------------------
pub fn handle_cfgname(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        ok_data(&with_cfg(|c| c.name_str()))
    } else {
        if args.len() > 16 {
            return err(10, "name too long (max 16)");
        }
        let mut name = [0u8; 16];
        name[..args.len()].copy_from_slice(args.as_bytes());
        with_cfg_mut(|c| c.name = name);
        ok_none()
    }
}

// ----------------------------------------------------------------------------
// AT+CFGIP[=<ip>,<mask>,<gw>[,<dns>]]
// ----------------------------------------------------------------------------
pub fn handle_cfgip(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        let s = with_cfg(|c| {
            format!(
                "ip={},mask={},gw={},dns={},dhcp={}",
                c.ip_str(),
                ipv4_str(&c.mask),
                ipv4_str(&c.gateway),
                ipv4_str(&c.dns),
                c.dhcp as u8
            )
        });
        ok_data(&s)
    } else {
        let parts: Vec<&str> = args.split(',').collect();
        if parts.len() < 3 || parts.len() > 4 {
            return err(10, "usage: AT+CFGIP=<ip>,<mask>,<gw>[,<dns>]");
        }
        let ip = match parse_ipv4(parts[0]) {
            Some(v) => v,
            None => return err(11, "invalid ip"),
        };
        let mask = match parse_ipv4(parts[1]) {
            Some(v) => v,
            None => return err(11, "invalid mask"),
        };
        let gw = match parse_ipv4(parts[2]) {
            Some(v) => v,
            None => return err(11, "invalid gw"),
        };
        let dns = if parts.len() == 4 {
            parse_ipv4(parts[3]).unwrap_or([192, 168, 1, 1])
        } else {
            gw
        };
        with_cfg_mut(|c| {
            c.ip = ip;
            c.mask = mask;
            c.gateway = gw;
            c.dns = dns;
            c.dhcp = false;
        });
        ok_none()
    }
}

// ----------------------------------------------------------------------------
// AT+CFGDHCP=<0|1>
// ----------------------------------------------------------------------------
pub fn handle_cfgdhcp(args: &str) -> String {
    let v = match parse_u16(args) {
        Some(v) => v,
        None => return err(10, "invalid value"),
    };
    with_cfg_mut(|c| c.dhcp = v != 0);
    ok_data(&format!("dhcp={}", v))
}

// ----------------------------------------------------------------------------
// AT+CFGMAC  (读以太网 MAC)
// ----------------------------------------------------------------------------
pub fn handle_cfgmac(_args: &str) -> String {
    ok_data(&with_cfg(|c| c.mac_str()))
}

// ----------------------------------------------------------------------------
// AT+CFGBTMAC  (读蓝牙 MAC)
// ----------------------------------------------------------------------------
pub fn handle_cfgbtmac(_args: &str) -> String {
    ok_data(&with_cfg(|c| c.ble_mac_str()))
}

// ----------------------------------------------------------------------------
// AT+CFGBTNAME[=<name>]
// ----------------------------------------------------------------------------
pub fn handle_cfgbtname(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        ok_data(&with_cfg(|c| c.ble_name_str()))
    } else {
        if args.len() > 8 {
            return err(10, "ble name too long (max 8)");
        }
        let mut name = [0u8; 8];
        name[..args.len()].copy_from_slice(args.as_bytes());
        with_cfg_mut(|c| c.ble_name = name);
        // BLE name is an externally visible identity. Persist it immediately
        // so an AT write cannot appear successful and then revert after reset.
        crate::bus::backends::sync_ble_name_holding();
        device::request_persist_config();
        // LOOP7: 写完立即同步 GAP 设备名
        crate::ble_at::update_gap_device_name();
        ok_none()
    }
}

// ----------------------------------------------------------------------------
// AT+CFG485=<idx>,<baud>,<data>,<stop>,<parity>,<slave>,<mode>
// AT+CFG485=<idx>
// ----------------------------------------------------------------------------
pub fn handle_cfg485(args: &str) -> String {
    let parts: Vec<&str> = args.split(',').collect();
    if parts.is_empty() {
        return err(
            10,
            "usage: AT+CFG485=<idx>[,<baud>,<data>,<stop>,<parity>,<slave>,<mode>]",
        );
    }
    let idx = match parse_u16(parts[0]) {
        Some(i) if i < 3 => i as usize,
        _ => return err(11, "invalid idx (0..2)"),
    };

    if parts.len() == 1 {
        // 读
        let s = with_cfg(|c| {
            let r = &c.rs485[idx];
            format!(
                "idx={},baud={},data={},stop={},parity={},slave={},mode={}",
                idx, r.baudrate, r.data_bits, r.stop_bits, r.parity, r.slave_addr, r.mode
            )
        });
        ok_data(&s)
    } else if parts.len() == 7 {
        // 写
        let baud = match parse_u16(parts[1]) {
            Some(v) => v as u32 * 100,
            None => return err(11, "invalid baud"),
        };
        let data = match parse_u16(parts[2]) {
            Some(v @ 7) | Some(v @ 8) => v as u8,
            _ => return err(11, "data_bits must be 7 or 8"),
        };
        let stop = match parse_u16(parts[3]) {
            Some(v @ 1) | Some(v @ 2) => v as u8,
            _ => return err(11, "stop_bits must be 1 or 2"),
        };
        let parity = match parse_u16(parts[4]) {
            Some(v @ 0) | Some(v @ 1) | Some(v @ 2) => v as u8,
            _ => return err(11, "parity must be 0/1/2"),
        };
        let slave = match parse_u16(parts[5]) {
            Some(v) => v as u8,
            None => return err(11, "invalid slave"),
        };
        let mode = match parse_u16(parts[6]) {
            Some(v @ 0) | Some(v @ 1) | Some(v @ 2) => v as u8,
            _ => return err(11, "mode must be 0/1/2"),
        };
        with_cfg_mut(|c| {
            c.rs485[idx].baudrate = baud;
            c.rs485[idx].data_bits = data;
            c.rs485[idx].stop_bits = stop;
            c.rs485[idx].parity = parity;
            c.rs485[idx].slave_addr = slave;
            c.rs485[idx].mode = mode;
        });
        ok_none()
    } else {
        err(10, "expected 1 or 7 args")
    }
}

// ----------------------------------------------------------------------------
// AT+CFGAPPLY
// ----------------------------------------------------------------------------
pub fn handle_cfgapply(_args: &str) -> String {
    // cfg_version 自增
    with_cfg_mut(|c| c.cfg_version = c.cfg_version.wrapping_add(1));
    match device::apply_config_sync() {
        Ok(_) => ok_data("applied (restart to take effect)"),
        Err(e) => err(20, &format!("apply: {}", e)),
    }
}

// ----------------------------------------------------------------------------
// AT+CFGRESET
// ----------------------------------------------------------------------------
pub fn handle_cfgreset(_args: &str) -> String {
    with_cfg_mut(|c| *c = SystemConfig::defaults());
    match device::apply_config_sync() {
        Ok(_) => ok_data("reset to defaults"),
        Err(e) => err(20, &format!("apply: {}", e)),
    }
}

// ----------------------------------------------------------------------------
// AT+CFGINFO
// ----------------------------------------------------------------------------
pub fn handle_cfginfo(_args: &str) -> String {
    let s = with_cfg(|c| {
        format!(
            "sn={},name={},hw=0x{:04X},fw=0x{:04X},cfg_ver={},ip={},mask={},gw={},dhcp={},eth_mac={},ble_mac={},ble_name={},rs485_0=baud{}/slave{},rs485_1=baud{}/slave{},rs485_2=baud{}/slave{}",
            c.sn_str(),
            c.name_str(),
            c.hw_version,
            c.fw_version,
            c.cfg_version,
            c.ip_str(),
            ipv4_str(&c.mask),
            ipv4_str(&c.gateway),
            c.dhcp as u8,
            c.mac_str(),
            c.ble_mac_str(),
            c.ble_name_str(),
            c.rs485[0].baudrate,
            c.rs485[0].slave_addr,
            c.rs485[1].baudrate,
            c.rs485[1].slave_addr,
            c.rs485[2].baudrate,
            c.rs485[2].slave_addr
        )
    });
    ok_data(&s)
}

// ----------------------------------------------------------------------------
// AT+CFGREAD=<addr>          按 Modbus 地址读 U16 (0x0200-0x025F)
// ----------------------------------------------------------------------------
pub fn handle_cfgread(args: &str) -> String {
    let addr = match parse_u16(args) {
        Some(a) => a,
        None => return err(10, "invalid addr"),
    };
    if !(regs::CFG_BASE..regs::CFG_END).contains(&addr) {
        return err(
            11,
            &format!(
                "addr out of range [{:#06X}..{:#06X})",
                regs::CFG_BASE,
                regs::CFG_END
            ),
        );
    }
    let v = with_cfg(|c| c.read_reg(addr).unwrap_or(0));
    ok_data(&format!("{},0x{:04X}", v, v))
}

// ----------------------------------------------------------------------------
// AT+CFGWRITE=<addr>,<value>
// ----------------------------------------------------------------------------
pub fn handle_cfgwrite(args: &str) -> String {
    let parts: Vec<&str> = args.splitn(2, ',').collect();
    if parts.len() != 2 {
        return err(10, "usage: AT+CFGWRITE=<addr>,<value>");
    }
    let addr = match parse_u16(parts[0]) {
        Some(a) => a,
        None => return err(10, "invalid addr"),
    };
    let value = match parse_u16(parts[1]) {
        Some(v) => v,
        None => return err(10, "invalid value"),
    };
    if !(regs::CFG_BASE..regs::CFG_END).contains(&addr) {
        return err(11, "addr out of range");
    }
    use crate::device::system_config::WriteResult;
    let result = with_cfg_mut(|c| c.write_reg(addr, value));
    if !matches!(&result, WriteResult::NotFound)
        && (regs::HOLD_BLE_ADDR_BASE..regs::HOLD_BLE_ADDR_BASE + regs::HOLD_BLE_ADDR_COUNT)
            .contains(&addr)
    {
        crate::bus::backends::sync_ble_name_holding();
        // CFGWRITE 走同一 SystemConfig 别名路径时，也要让 GAP 广播名称
        // 在下一 BLE tick 更新；持久化仍由下面的结果分支负责。
        crate::ble_at::notify_ble_name_changed();
    }
    match result {
        WriteResult::Ok => ok_none(),
        WriteResult::Persist => {
            // 与 backends::write_hold_reg 保持一致: 触发异步持久化.
            // 不增 cfg_version, 仅 NVS 写入. AT+CFG* 命令不需要额外回复.
            crate::device::request_apply_config();
            ok_data("persist requested")
        }
        WriteResult::Apply => {
            // 触发异步应用
            crate::device::request_apply_config();
            ok_data("apply requested")
        }
        WriteResult::Reset => {
            // 已恢复默认, 触发应用
            crate::device::request_apply_config();
            ok_data("reset to defaults")
        }
        WriteResult::NotFound => err(12, "field not found"),
    }
}

// ----------------------------------------------------------------------------
// 辅助
// ----------------------------------------------------------------------------

fn ipv4_str(b: &[u8; 4]) -> String {
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

/// 读取 SystemConfig (RCU, 零拷贝).
///
/// LOOP11: 改用 `config_read_with` 闭包模式, 避免 ConfigSnapshot
/// 在 BTC_TASK 栈上 clone (~2.5KB). 闭包内直接借用, Drop 时退计数.
/// `.unwrap_or_else(defaults)` 仅为防御性回退 (单元测试或非生产路径).
fn with_cfg<R>(f: impl FnOnce(&SystemConfig) -> R) -> R {
    // config_read_with 返回 Option<R>; f 是 FnOnce 只能调一次.
    // 把 f 包在 Option 里, 哪个分支用就 take 出来.
    let mut f = Some(f);
    match crate::bus::config_state::config_read_with(|cs| f.take().unwrap()(&cs.cfg)) {
        Some(r) => r,
        None => f.take().unwrap()(&SystemConfig::defaults()),
    }
}

/// 修改 SystemConfig 后回写 CONFIG RCU (RMW: clone → mutate → 原子替换).
/// 旧 `with_cfg_mut` 不触发 apply, 此处保持一致 (apply 由调用方/Modbus 写 CFG_APPLY 触发).
fn with_cfg_mut<R>(f: impl FnOnce(&mut SystemConfig) -> R) -> R {
    crate::bus::backends::config_modify_with_result(|cfg| f(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg485_exposes_all_three_legacy_ports() {
        for index in 0..3 {
            let response = handle_cfg485(&index.to_string());
            assert!(response.contains(&format!("idx={index}")), "{response}");
        }
        assert!(handle_cfg485("3").contains("invalid idx (0..2)"));
    }

    #[test]
    fn cfginfo_includes_third_legacy_port() {
        assert!(handle_cfginfo("").contains("rs485_2="));
    }

    #[test]
    fn cfgbtname_updates_authoritative_config_and_marks_it_dirty() {
        let response = handle_cfgbtname("Node221");
        assert!(response.starts_with("OK"), "{response}");
        assert_eq!(with_cfg(|cfg| cfg.ble_name_str()), "Node221");
        assert!(device::config_is_dirty());
    }
}
