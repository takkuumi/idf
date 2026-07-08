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

use crate::config::regs;
use crate::device::system_config::{SystemConfig, WriteResult};

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
#[derive(Default)]
pub struct Bus {
    pub di: DiState,
    pub do_: DoState,
    pub ai: AiState,
    pub ao: AoState,
    pub sys: SysState,
    pub proto: ProtoStore,
    pub cfg: SystemConfig,
}

impl Bus {
    fn new() -> Self {
        let mut s = Self::default();
        s.sys.firmware_version = 0x0100; // v1.00
        s.cfg = SystemConfig::defaults();
        s
    }

    // ---- Modbus 寄存器映射 (供 Modbus 模块使用) ----

    /// 读取线圈 (FC=0x01)
    pub fn read_coil(&self, addr: u16) -> Option<bool> {
        if addr < regs::COIL_DO_COUNT {
            Some(self.do_.bits & (1u64 << addr) != 0)
        } else {
            None
        }
    }

    /// 写入线圈 (FC=0x05/0x0F)
    pub fn write_coil(&mut self, addr: u16, value: bool) -> bool {
        if addr < regs::COIL_DO_COUNT {
            if value {
                self.do_.bits |= 1u64 << addr;
            } else {
                self.do_.bits &= !(1u64 << addr);
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

    /// 读取输入寄存器 (FC=0x04)
    pub fn read_input_reg(&self, addr: u16) -> Option<u16> {
        if addr < regs::INREG_AI_COUNT {
            Some(self.ai.raw[addr as usize])
        } else {
            let scaled_addr = addr.wrapping_sub(regs::INREG_AI_SCALED_BASE);
            if scaled_addr < 6 {
                Some(self.ai.scaled[scaled_addr as usize])
            } else {
                None
            }
        }
    }

    /// 读取保持寄存器 (FC=0x03)
    pub fn read_hold_reg(&self, addr: u16) -> Option<u16> {
        if addr < regs::HOLD_AO_COUNT {
            Some(self.ao.scaled[addr as usize])
        } else if addr >= regs::CFG_BASE && addr < regs::CFG_END {
            // 系统配置区
            self.cfg.read_reg(addr)
        } else if addr >= regs::PROTO_BASE && addr < regs::PROTO_END {
            // 协议存储区数据
            let idx = (addr - regs::PROTO_BASE) as usize;
            Some(self.proto.data[idx])
        } else {
            match addr {
                regs::HOLD_SYS_FW_VER => Some(self.sys.firmware_version),
                regs::HOLD_SYS_UPTIME_S => Some((self.sys.uptime_s & 0xFFFF) as u16),
                regs::HOLD_SYS_RESET_CNT => Some(self.sys.reset_count),
                regs::HOLD_SYS_RESET => Some(if self.sys.reset_request { 0xA5A5 } else { 0 }),
                regs::HOLD_SYS_RESET_REASON => Some(self.sys.reset_reason as u16),
                regs::HOLD_SYS_TASK_HEALTH => Some(0), // TODO: 接入 health 模块位图
                regs::HOLD_SYS_LOG_LEVEL => Some(self.sys.log_level as u16),
                // OTA 升级区 (RO)
                regs::HOLD_OTA_STATUS => Some(crate::ota::status().as_u16()),
                regs::HOLD_OTA_WRITTEN_LO => {
                    Some((crate::ota::written_bytes() & 0xFFFF) as u16)
                }
                regs::HOLD_OTA_WRITTEN_HI => {
                    Some((crate::ota::written_bytes() >> 16) as u16)
                }
                // 协议元数据
                regs::PROTO_COMMIT => Some(0),
                regs::PROTO_RELOAD => Some(0),
                regs::PROTO_VERSION => Some(self.proto.version),
                regs::PROTO_LENGTH => Some(self.proto.length),
                regs::PROTO_STATUS => Some(self.proto.status as u16),
                regs::PROTO_MAGIC => Some(crate::device::PROTO_MAGIC),
                _ => None,
            }
        }
    }

    /// 写入保持寄存器 (FC=0x06/0x10)
    pub fn write_hold_reg(&mut self, addr: u16, value: u16) -> bool {
        if addr < regs::HOLD_AO_COUNT {
            self.ao.scaled[addr as usize] = value;
            true
        } else if addr >= regs::CFG_BASE && addr < regs::CFG_END {
            // 系统配置区
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
                WriteResult::NotFound => false,
            }
        } else if addr >= regs::PROTO_BASE && addr < regs::PROTO_END {
            // 协议存储区数据
            let idx = (addr - regs::PROTO_BASE) as usize;
            self.proto.data[idx] = value;
            self.proto.dirty = true;
            true
        } else {
            match addr {
                regs::HOLD_SYS_RESET => {
                    if value == 0xA5A5 {
                        self.sys.reset_request = true;
                    }
                    true
                }
                regs::HOLD_SYS_LOG_LEVEL => {
                    // 运行时日志级别调节 (0=Err 1=Warn 2=Info 3=Debug 4=Trace)
                    if value <= 4 {
                        self.sys.log_level = value as u8;
                        // 立即应用日志级别
                        let level = match value {
                            0 => log::LevelFilter::Error,
                            1 => log::LevelFilter::Warn,
                            2 => log::LevelFilter::Info,
                            3 => log::LevelFilter::Debug,
                            4 => log::LevelFilter::Trace,
                            _ => log::LevelFilter::Info,
                        };
                        log::set_max_level(level);
                        log::info!("[bus] log level set to {}", value);
                    }
                    true
                }
                // OTA 升级区
                regs::HOLD_OTA_TOTAL_LO => {
                    let total = crate::ota::pending_total();
                    crate::ota::set_pending_total((total & 0xFFFF_0000) | value as u32);
                    true
                }
                regs::HOLD_OTA_TOTAL_HI => {
                    let total = crate::ota::pending_total();
                    crate::ota::set_pending_total((total & 0x0000_FFFF) | ((value as u32) << 16));
                    true
                }
                regs::HOLD_OTA_BEGIN => {
                    if value == 0x0B0A {
                        let total = crate::ota::pending_total();
                        log::info!("[bus] OTA begin triggered, total={} bytes", total);
                        if let Err(e) = crate::ota::begin(total) {
                            log::error!("[bus] OTA begin failed: {}", e);
                        }
                    }
                    true
                }
                regs::HOLD_OTA_END => {
                    if value == 0x0E0D {
                        log::info!("[bus] OTA end triggered");
                        match crate::ota::end() {
                            Ok(()) => log::info!("[bus] OTA end ok, ready to reboot"),
                            Err(e) => log::error!("[bus] OTA end failed: {}", e),
                        }
                    }
                    true
                }
                regs::HOLD_OTA_ABORT => {
                    if value == 0x0AB0 {
                        log::info!("[bus] OTA abort triggered");
                        let _ = crate::ota::abort();
                    }
                    true
                }
                regs::HOLD_OTA_REBOOT => {
                    if value == 0x0F0E {
                        log::info!("[bus] OTA reboot triggered");
                        // 在新线程中延时重启, 给 Modbus 响应发送留时间
                        std::thread::spawn(|| {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            unsafe { esp_idf_sys::esp_restart() };
                        });
                    }
                    true
                }
                regs::PROTO_COMMIT => {
                    if value == 0xC5C5 {
                        self.proto.status = 1; // 写入中
                        crate::device::request_commit();
                    }
                    true
                }
                regs::PROTO_RELOAD => {
                    if value == 0xA5A5 {
                        self.proto.status = 2; // 加载中
                        crate::device::request_reload();
                    }
                    true
                }
                regs::PROTO_VERSION => {
                    self.proto.version = value;
                    self.proto.dirty = true;
                    true
                }
                regs::PROTO_LENGTH => {
                    self.proto.length = value;
                    self.proto.dirty = true;
                    true
                }
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
