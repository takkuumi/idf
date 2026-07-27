//! 设备配置表 NVS 持久化 — 复用 device 模块的 NVS

use crate::error::AppResult;
use super::{DeviceConfigTable, DeviceEntry};

const MAGIC: u32 = 0x44455643; // "DEVC"

pub fn load(table: &mut DeviceConfigTable) -> AppResult<()> {
    let mut buf = [0u8; 1024];
    let len = match crate::device::nvs_read_to_buf("dev_cfg", &mut buf) {
        Some(n) => n,
        None => return Ok(()),
    };

    if len < 4 { return Ok(()); }
    let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if magic != MAGIC { return Ok(()); }

    let mut pos = 4;
    // 严格边界: 每个条目固定头部 6B (dev_type/port/slave_id/func/reg_addr_lo/reg_addr_hi)
    while pos + 6 <= len {
        let dev_type = super::types::DeviceType::from_u8(buf[pos]);
        let rs485_port = buf[pos + 1];
        let slave_id = buf[pos + 2];
        let func = buf[pos + 3];
        let reg_addr = u16::from_le_bytes([buf[pos + 4], buf[pos + 5]]);
        pos += 6;

        let reg_count = if pos + 2 <= len {
            let v = u16::from_le_bytes([buf[pos], buf[pos + 1]]);
            pos += 2;
            v
        } else { 8u16 };

        let param_count = if pos + 1 <= len { buf[pos] as usize } else { 0 };
        pos += 1;

        let mut params = heapless::Vec::new();
        for _ in 0..param_count.min(32) {
            if pos + 2 <= len {
                let _ = params.push(u16::from_le_bytes([buf[pos], buf[pos + 1]]));
                pos += 2;
            }
        }

        if table.devices.len() < 32 {
            let _ = table.devices.push(DeviceEntry {
                dev_type, rs485_port, slave_id, func, reg_addr, reg_count, params,
            });
        }
    }
    log::info!("[dev_cfg] loaded {} devices from NVS", table.devices.len());
    table.stored_count = table.devices.len() as u16;
    Ok(())
}

pub fn save(table: &DeviceConfigTable) -> AppResult<()> {
    let mut buf = [0u8; 1024];
    let mut pos = 0;
    buf[pos] = (MAGIC & 0xFF) as u8; pos += 1;
    buf[pos] = ((MAGIC >> 8) & 0xFF) as u8; pos += 1;
    buf[pos] = ((MAGIC >> 16) & 0xFF) as u8; pos += 1;
    buf[pos] = ((MAGIC >> 24) & 0xFF) as u8; pos += 1;

    for entry in &table.devices {
        // 最小条目大小: dev_type(1) + port(1) + slave_id(1) + func(1)
        //              + reg_addr(2) + reg_count(2) + param_count(1) = 9B
        if pos + 9 > buf.len() { break; }
        buf[pos] = entry.dev_type.as_u8(); pos += 1;
        buf[pos] = entry.rs485_port; pos += 1;
        buf[pos] = entry.slave_id; pos += 1;
        buf[pos] = entry.func; pos += 1;
        buf[pos] = (entry.reg_addr & 0xFF) as u8; pos += 1;
        buf[pos] = ((entry.reg_addr >> 8) & 0xFF) as u8; pos += 1;
        buf[pos] = (entry.reg_count & 0xFF) as u8; pos += 1;
        buf[pos] = ((entry.reg_count >> 8) & 0xFF) as u8; pos += 1;
        buf[pos] = entry.params.len() as u8; pos += 1;
        for &p in &entry.params {
            if pos + 2 > buf.len() { break; }
            buf[pos] = (p & 0xFF) as u8; pos += 1;
            buf[pos] = ((p >> 8) & 0xFF) as u8; pos += 1;
        }
    }

    crate::device::nvs_write("dev_cfg", &buf[..pos])?;
    log::info!("[dev_cfg] saved {} devices to NVS", table.devices.len());
    Ok(())
}
