//! 应用层数据总线
//!
//! 全局共享的设备状态容器，所有模块都通过它读写 IO/AI/AO/系统数据。
//! 通过 `once_cell` 提供全局单例，配合 `std::sync::Mutex` 实现线程安全。
//!
//! 设计原则：
//! - 所有外设只与总线交互，模块之间不直接耦合
//! - Modbus 寄存器映射 (modbus_map) 把总线数据暴露给 Modbus 协议
//! - BLE Mesh 模型 (blemesh) 通过总线读写 DO/AO
//! - 数据结构保持简单，便于 Modbus 16-bit 寄存器直接对应

use parking_lot::Mutex;
use once_cell::sync::Lazy;

use crate::config::{hw_version, regs};
use crate::device::system_config::{SystemConfig, WriteResult};
use crate::device_config::DeviceConfigTable;

// ----------------------------------------------------------------------------
// 数据结构
// ----------------------------------------------------------------------------

/// 数字输出 (DO)
/// - 默认版本: bit0..bit7 (8 路)
/// - F3/F4 版本: bit0..bit15 (16 路)
/// 用 u64 统一表示, 兼容所有版本
#[derive(Clone, Copy, Default)]
pub struct DoState {
    pub bits: u64, // bit i 对应 DO i
}

/// 数字输入 (DI)
/// - 默认版本: bit0..bit7 (8 路)
/// - F3 版本: bit0..bit15 (16 路)
/// - F4 版本: bit0..bit47 (48 路)
/// 用 u64 统一表示, 兼容所有版本 (F4 用 48 bit, u64 够)
#[derive(Clone, Copy, Default)]
pub struct DiState {
    pub bits: u64,
}

/// 6 路模拟输入 (AI)
/// 原始 ADC 值 + 工程量 (用户可标定)
#[derive(Clone, Copy, Default)]
pub struct AiState {
    /// 12-bit 原始 ADC 值
    pub raw: [u16; 6],
    /// 工程量 * 1000 (例如 4-20mA 对应 0-10000 表示 0.000-10.000mA)
    pub scaled: [u16; 6],
}

/// 4 路模拟输出 (AO)
/// 工程量 * 1000
#[derive(Clone, Copy, Default)]
pub struct AoState {
    /// 工程量 * 1000 (与 scaled 同尺度)
    pub scaled: [u16; 4],
    /// 标定后转换为 LEDC 占空比 (0..=2^resolution-1)
    pub duty: [u32; 4],
}

/// 系统状态寄存器
#[derive(Clone, Copy)]
pub struct SysState {
    pub firmware_version: u16,
    pub uptime_s: u32,
    pub reset_count: u16,
    /// 复位原因 (esp_reset_reason_t 值):
    /// 1=POWERON 2=EXT 3=SW 4=PANIC 5=INT_WDT 6=TASK_WDT 7=WDT 15=BROWNOUT 16=DEEPSLEEP
    pub reset_reason: u8,
    pub reset_request: bool,
    /// 运行时日志级别 (0=Err 1=Warn 2=Info 3=Debug 4=Trace, 默认 2)
    pub log_level: u8,
}

impl SysState {
    /// 默认日志级别 = Info (2)
    pub const DEFAULT_LOG_LEVEL: u8 = 2;
}

impl Default for SysState {
    fn default() -> Self {
        Self {
            firmware_version: 0,
            uptime_s: 0,
            reset_count: 0,
            reset_reason: 0,
            reset_request: false,
            log_level: Self::DEFAULT_LOG_LEVEL,
        }
    }
}

/// 协议存储区 (RAM 镜像)
/// 由 device 模块负责持久化到 NVS, 由 Modbus / BLE AT 写入
#[derive(Clone)]
pub struct ProtoStore {
    /// 1500 个 U16 协议数据 (3000 字节)
    pub data: [u16; 1500],
    /// 用户自定义协议版本
    pub version: u16,
    /// 用户写入的有效协议长度 (U16 数)
    pub length: u16,
    /// 是否有未持久化的修改
    pub dirty: bool,
    /// 状态: 0=空闲 1=写入中 2=加载中 3=校验失败
    pub status: u8,
}

impl Default for ProtoStore {
    fn default() -> Self {
        Self {
            data: [0u16; 1500],
            version: 0,
            length: 0,
            dirty: false,
            status: 0,
        }
    }
}

/// 总线总状态
pub struct Bus {
    pub di: DiState,
    pub do_: DoState,
    pub ai: AiState,
    pub ao: AoState,
    pub sys: SysState,
    pub proto: ProtoStore,
    pub cfg: SystemConfig,
    pub device_config: DeviceConfigTable,
    /// 设备文本区 (5000-6999 = 2000 字)
    pub device_text: [u16; 2000],
}

impl Default for Bus {
    fn default() -> Self {
        Self {
            di: Default::default(),
            do_: Default::default(),
            ai: Default::default(),
            ao: Default::default(),
            sys: Default::default(),
            proto: Default::default(),
            cfg: SystemConfig::defaults(),
            device_config: DeviceConfigTable::default(),
            device_text: [0u16; 2000],
        }
    }
}

impl Bus {
    fn new() -> Self {
        let mut s = Self::default();
        s.sys.firmware_version = 0x0100;
        s.cfg = SystemConfig::defaults();
        s
    }

    // ---- Modbus 寄存器映射 (供 Modbus 模块使用) ----

    /// 读取线圈 (FC=0x01). 地址范围: COIL_DO_BASE .. COIL_DO_END
    pub fn read_coil(&self, addr: u16) -> Option<bool> {
        if addr >= regs::COIL_DO_BASE && addr < regs::COIL_DO_END {
            let ch = (addr - regs::COIL_DO_BASE) as usize;
            Some(self.do_.bits & (1u64 << ch) != 0)
        } else {
            None
        }
    }

    /// 写入线圈 (FC=0x05/0x0F)
    pub fn write_coil(&mut self, addr: u16, value: bool) -> bool {
        if addr >= regs::COIL_DO_BASE && addr < regs::COIL_DO_END {
            let ch = (addr - regs::COIL_DO_BASE) as usize;
            if value {
                self.do_.bits |= 1u64 << ch;
            } else {
                self.do_.bits &= !(1u64 << ch);
            }
            true
        } else {
            false
        }
    }

    /// 读取离散输入 (FC=0x02)
    pub fn read_disc(&self, addr: u16) -> Option<bool> {
        if addr < regs::DISC_DI_COUNT {
            Some(self.di.bits & (1u64 << addr) != 0)
        } else {
            None
        }
    }

    /// 读取输入寄存器 (FC=0x04). 参考固件: AI raw @ 0x80, AI status @ 0x88, sys info @ 0x87C
    pub fn read_input_reg(&self, addr: u16) -> Option<u16> {
        if addr >= regs::INREG_AI_BASE && addr < regs::INREG_AI_BASE + regs::INREG_AI_COUNT {
            let idx = (addr - regs::INREG_AI_BASE) as usize;
            Some(self.ai.raw.get(idx).copied().unwrap_or(0))
        } else if addr >= regs::INREG_AI_STATUS_BASE && addr < regs::INREG_AI_STATUS_BASE + regs::INREG_AI_COUNT {
            let idx = (addr - regs::INREG_AI_STATUS_BASE) as usize;
            Some(self.ai.scaled.get(idx).copied().unwrap_or(0))
        } else {
            match addr {
                regs::INREG_QI_COUNT => Some(((hw_version::DO_COUNT as u16) << 8) | (hw_version::DI_COUNT as u16)),
                regs::INREG_ADC485 => Some((regs::INREG_AI_COUNT << 8) | 2), // 4 ADC + 2 RS485
                regs::INREG_FW_VER => Some(self.sys.firmware_version),
                regs::INREG_FW_DATE => Some(0x0615), // MMDD format
                _ => None,
            }
        }
    }

    /// 读取保持寄存器 (FC=0x03). 匹配参考固件 PRegBuf 全范围返回.
    pub fn read_hold_reg(&self, addr: u16) -> Option<u16> {
        // 设备文本区 (5000-6999)
        if addr >= regs::DEVICE_TEXT_BASE && addr <= regs::DEVICE_TEXT_END {
            let idx = (addr - regs::DEVICE_TEXT_BASE) as usize;
            return Some(self.device_text[idx]);
        }
        // 设备功能配置区 (2300+)
        if addr >= regs::HOLD_DEVICE_CONFIG && addr <= regs::HOLD_CFG_END {
            if addr >= 2300 && addr < 2400 {
                // Try device_config first
                if let Some(v) = self.device_config.read_reg(addr) {
                    return Some(v);
                }
            }
            // 其余走 SystemConfig
            return Some(self.cfg.read_reg(addr).unwrap_or(0));
        }
        // 配置区 (2176-4223): 优先走 SystemConfig, 未映射的返回 0
        if addr >= regs::HOLD_CFG_BASE && addr <= regs::HOLD_CFG_END {
            return Some(self.cfg.read_reg(addr).unwrap_or(0));
        }
        // 协议存储区
        if addr >= regs::PROTO_BASE && addr < regs::PROTO_END {
            let idx = (addr - regs::PROTO_BASE) as usize;
            return Some(self.proto.data[idx]);
        }
        match addr {
            regs::PROTO_COMMIT => Some(0),
            regs::PROTO_RELOAD => Some(0),
            regs::PROTO_VERSION => Some(self.proto.version),
            regs::PROTO_LENGTH => Some(self.proto.length),
            regs::PROTO_STATUS => Some(self.proto.status as u16),
            regs::PROTO_MAGIC => Some(crate::device::PROTO_MAGIC),
            _ => None,
        }
    }

    /// 写入保持寄存器 (FC=0x06/0x10)
    pub fn write_hold_reg(&mut self, addr: u16, value: u16) -> bool {
        // 设备文本区 (5000-6999)
        if addr >= regs::DEVICE_TEXT_BASE && addr <= regs::DEVICE_TEXT_END {
            let idx = (addr - regs::DEVICE_TEXT_BASE) as usize;
            self.device_text[idx] = value;
            return true;
        }
        // 设备功能配置区 (2300+)
        if addr >= 2300 && addr < 2400 {
            return self.device_config.write_reg(addr, value);
        }
        if addr >= regs::HOLD_CFG_BASE && addr <= regs::HOLD_CFG_END {
            // 总是尝试写入 — write_reg 返回 NotFound 时也允许(存到 NVS 原始区)
            match self.cfg.write_reg(addr, value) {
                WriteResult::Ok => true,
                WriteResult::Apply => {
                    self.cfg.cfg_version = self.cfg.cfg_version.wrapping_add(1);
                    crate::device::request_apply_config();
                    true
                }
                WriteResult::Reset => {
                    self.cfg = SystemConfig::defaults();
                    crate::device::request_apply_config();
                    true
                }
                WriteResult::NotFound => true, // 允许写入未映射地址(参考固件 PRegBuf 全范围可写)
            }
        } else if addr >= regs::PROTO_BASE && addr < regs::PROTO_END {
            let idx = (addr - regs::PROTO_BASE) as usize;
            self.proto.data[idx] = value;
            self.proto.dirty = true;
            true
        } else {
            match addr {
                regs::PROTO_COMMIT => {
                    if value == 0xC5C5 { self.proto.status = 1; crate::device::request_commit(); }
                    true
                }
                regs::PROTO_RELOAD => {
                    if value == 0xA5A5 { self.proto.status = 2; crate::device::request_reload(); }
                    true
                }
                regs::PROTO_VERSION => { self.proto.version = value; self.proto.dirty = true; true }
                regs::PROTO_LENGTH => { self.proto.length = value; self.proto.dirty = true; true }
                _ => false,
            }
        }
    }
}

// ----------------------------------------------------------------------------
// 全局单例
// ----------------------------------------------------------------------------

pub static BUS: Lazy<Mutex<Bus>> = Lazy::new(|| Mutex::new(Bus::new()));

/// 便利函数：获取总线锁，超时 100ms
pub fn lock_timeout() -> Option<parking_lot::MutexGuard<'static, Bus>> {
    use std::time::Duration;
    BUS.try_lock_for(Duration::from_millis(100))
}

/// 便利宏：在闭包中持有总线锁并执行操作
#[macro_export]
macro_rules! with_bus {
    ($bus:ident, $body:block) => {{
        let $bus = match $crate::bus::lock_timeout() {
            Some(b) => b,
            None => {
                log::error!("bus lock timeout");
                continue;
            }
        };
        $body
    }};
}
