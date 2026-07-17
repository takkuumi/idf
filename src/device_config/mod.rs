//! 设备功能配置表 — 对齐参考固件 SLAVE_DEVICE_CONFIG (2300+)
//!
//! # 架构设计
//!
//! 参考固件将设备配置作为变长记录存储在 PRegBuf 中。
//! 这里用 trait 抽象实现解耦:
//! - [`DeviceFunction`] trait: 每种设备类型实现自己的轮询/控制逻辑
//! - [`DeviceConfigTable`]: 管理所有已配置设备, 提供 Modbus 寄存器读写
//! - 存储: 原始字节存入 NVS blob, 启动时恢复
//!
//! # 扩展方式
//! 新增设备类型只需:
//! 1. 实现 `DeviceFunction` trait
//! 2. 在 `DeviceType::from_u8` 注册类型码
//! 3. 在 `DeviceConfigTable::apply_poll` 中用新类型

use crate::error::AppResult;

mod types;
mod store;

pub use types::{DeviceType, DeviceFunction, DeviceFunctionMeta};

/// 设备配置表 (对齐参考固件 2300+ 寄存器区域)
pub struct DeviceConfigTable {
    /// 已配置设备列表
    devices: heapless::Vec<DeviceEntry, 32>,
    /// 写入 2300 的计数值 (独立于 devices.len(), 参考项目 PRegBuf 语义)
    stored_count: u16,
    /// NVS key 前缀
    nvs_key: &'static str,
}

struct DeviceEntry {
    /// 设备类型
    dev_type: DeviceType,
    /// RS485 端口 (1-5)
    rs485_port: u8,
    /// 从站地址
    slave_id: u8,
    /// 功能码
    func: u8,
    /// 从站寄存器地址
    reg_addr: u16,
    /// 寄存器数量
    reg_count: u16,
    /// 原始参数 (变长, 最大32字)
    params: heapless::Vec<u16, 32>,
}

impl Default for DeviceConfigTable {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceConfigTable {
    pub fn new() -> Self {
        Self {
            devices: heapless::Vec::new(),
            stored_count: 0,
            nvs_key: "dev_cfg",
        }
    }

    /// 从 NVS 加载
    pub fn load(&mut self) -> AppResult<()> {
        store::load(self)?;
        Ok(())
    }

    /// 持久化到 NVS
    pub fn save(&self) -> AppResult<()> {
        store::save(self)
    }

    /// 设备数量
    pub fn len(&self) -> usize { self.devices.len() }

    /// 从 Modbus 寄存器读取 (2300 = count, 2301+ = data)
    pub fn read_reg(&self, addr: u16) -> Option<u16> {
        let base = 2300u16;
        if addr == base {
            return Some(self.stored_count);
        }
        let off = (addr - base - 1) as usize;
        let mut pos = 0usize;
        for entry in &self.devices {
            let entry_words = 5 + entry.params.len(); // type+port+slave+func+addr+params
            if off >= pos && off < pos + entry_words {
                let i = off - pos;
                return Some(match i {
                    0 => (entry.dev_type.as_u8() as u16)
                        | ((entry.rs485_port as u16) << 8),
                    1 => (entry.slave_id as u16)
                        | ((entry.func as u16) << 8),
                    2 => entry.reg_addr,
                    3 => entry.reg_count,
                    4 => entry.params.len() as u16,
                    _ => entry.params.get(i - 5).copied().unwrap_or(0),
                });
            }
            pos += entry_words;
        }
        None
    }

    /// 写入 Modbus 寄存器
    pub fn write_reg(&mut self, addr: u16, value: u16) -> bool {
        let base = 2300u16;
        if addr == base {
            let count = value as usize;
            self.stored_count = value;
            self.devices.truncate(count.min(32));
            return true;
        }
        // 追加模式: 写 2301+ 按序填充当前设备
        let off = (addr - base - 1) as usize;
        let mut pos = 0usize;
        for entry in &mut self.devices {
            let entry_words = 5 + entry.params.len();
            if off >= pos && off < pos + entry_words {
                let i = off - pos;
                match i {
                    0 => {
                        entry.dev_type = DeviceType::from_u8((value & 0xFF) as u8);
                        entry.rs485_port = ((value >> 8) & 0xFF) as u8;
                    }
                    1 => {
                        entry.slave_id = (value & 0xFF) as u8;
                        entry.func = ((value >> 8) & 0xFF) as u8;
                    }
                    2 => entry.reg_addr = value,
                    3 => entry.reg_count = value,
                    4 => { /* param count — resize handled elsewhere */ }
                    _ => {
                        let pi = i - 5;
                        if pi < entry.params.len() {
                            entry.params[pi] = value;
                        }
                    }
                }
                return true;
            }
            pos += entry_words;
        }
        // 未找到匹配 — 若 off 指向新设备起始, 追加
        if off == pos && self.devices.len() < 32 {
            let dev_type = DeviceType::from_u8((value & 0xFF) as u8);
            let port = ((value >> 8) & 0xFF) as u8;
            let _ = self.devices.push(DeviceEntry {
                dev_type, rs485_port: port,
                slave_id: 1, func: 3, reg_addr: 0, reg_count: 8,
                params: heapless::Vec::new(),
            });
            return true;
        }
        false
    }

    /// 返回当前轮询表 (供 RS485 主站使用)
    pub fn poll_entries(&self) -> impl Iterator<Item = PollEntry> + '_ {
        self.devices.iter().map(|e| PollEntry {
            port: e.rs485_port,
            slave: e.slave_id,
            func: e.func,
            addr: e.reg_addr,
            count: e.reg_count,
        })
    }
}

/// RS485 主站轮询条目 (轻量 Copy)
#[derive(Clone, Copy)]
pub struct PollEntry {
    pub port: u8,
    pub slave: u8,
    pub func: u8,
    pub addr: u16,
    pub count: u16,
}
