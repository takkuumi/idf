//! NFC ST25DV16KC 配置备份/恢复模块
//!
//! 移植自参考固件 MCA_F16V2_1_F48_BLE.ino 的 `RFID_Init` 函数.
//!
//! ## 硬件
//!
//! NFC 硬件位支持 ST25DV16K (IC_REF=0x26) 与 ST25DV16KC (IC_REF=0x51)，为 NFC Type 5
//! 标签 + I2C 双接口芯片。未焊接时本模块保持 Idle 并降级运行，不阻断主服务。
//! 参考固件通过 I2C (Arduino Wire) 访问, 本实现使用 `SwI2c` (软件 I2C).
//!
//! - I2C 7-bit 地址: 0x53 (8-bit: 写 0xA6, 读 0xA7)
//! - 引脚: 先尝试 LED 总线 (SDA=38, SCL=37), 失败则 IO 总线 (SDA=35, SCL=36)
//!   (对齐参考固件 `Wire.setPins(38,37)` → `Wire.setPins(35,36)` 回退)
//!
//! ## 功能 (对齐参考固件 RFID_Init)
//!
//! 1. **NDEF 文本记录** (Area 1, 0x0000..=0x011F):
//!    - 记录 1: 设备 IP 链接 (http://192.168.x.x)
//!    - 记录 2: 子网掩码
//!    - 记录 3: 网关
//!    - 记录 4: 蓝牙地址 (8 字节)
//!    - 记录 5: 位置/桩号 (16 字节)
//!    - 记录 8: 设备型号
//!    - 记录 9: 固件版本 (V2.2.1.1557)
//!
//! 2. **二进制配置快照** (从 0x0120 开始，固定 1760 字节):
//!    - `isModify=1` (backup): holding_buf → NFC EEPROM (原 C++ 原始布局)
//!    - `isModify=2` (restore): NFC EEPROM → holding_buf
//!
//! ## 启动时机
//!
//! 对齐参考固件: `RFID_Init(0)` 在 `setup()` 中、`nca9555_init()` 之前调用.
//! 本模块在 `main()` 中启动后台任务，并与 PCA9555 通过物理总线仲裁器共享 I2C。
//! 避免 I2C 总线冲突 (NFC 和 PCA9555 共用 LED 总线引脚 38/37).
//!
//! 后台线程按配置变化检查维护请求；RF Mailbox 在标签存在时快速轮询，失败后指数退避，避免写入风暴。
//!
//! ## 可靠性
//!
//! - 启动失败仅记日志, 不阻断主流程
//! - I2C 通信失败时退避重试
//! - WDT 喂狗 + 任务心跳防止线程假死
//! - NFC 仅作为冗余备份, 不破坏 NVS 数据

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::Duration;
#[cfg(feature = "ble-at")]
use std::time::Instant;
use std::{ops::Deref, ops::DerefMut, ptr::NonNull};

use crate::bus::storage_state::storage_read;
use crate::error::AppResult;
use crate::hal::sw_i2c::SwI2c;
use crate::health::{self, TaskHb};
use crate::sync::Spin;

/// NFC 模块启动标志 (一次性, 防重复)
static STARTED: AtomicBool = AtomicBool::new(false);

/// NFC 模块任务心跳 (阈值 30s)
static TASK_HB: TaskHb = TaskHb::new_with_stall("nfc-st25", 30);

/// LOOP14: NFC EEPROM 磨损均衡
/// 上次成功写入 NFC 的 holding_buf CRC32 (用于跳过冗余写).
/// 通过 RAM CRC 去重与失败退避，避免配置无变化时重复擦写 EEPROM。
static LAST_NFC_CRC: AtomicU32 = AtomicU32::new(0xFFFF_FFFF);

const NFC_CMD_AUTO: u8 = 0;
const NFC_CMD_BACKUP: u8 = 1;
const NFC_CMD_RESTORE: u8 = 2;
/// 手动请求在检测到可用标签且安全会话打开后才消费，标签暂时离线不会丢命令。
static NFC_COMMAND: AtomicU8 = AtomicU8::new(NFC_CMD_AUTO);

/// 全局 NFC 状态 (供 Modbus / HTTP API 读取)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NfcState {
    /// 未启动 / 空闲
    Idle,
    /// I2C 初始化中
    Init,
    /// 检测到标签, 读取中
    Detected,
    /// 备份成功 (写入 NFC)
    BackedUp,
    /// 恢复成功 (从 NFC 读取)
    Restored,
    /// 错误 (I2C 失败 / 标签无响应)
    Error,
}

impl NfcState {
    /// 稳定的维护 API 状态名称，避免把 Debug 表示暴露为外部协议。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Init => "Init",
            Self::Detected => "Detected",
            Self::BackedUp => "BackedUp",
            Self::Restored => "Restored",
            Self::Error => "Error",
        }
    }
}

static NFC_STATE: LazyLock<Spin<NfcState>> = LazyLock::new(|| Spin::new(NfcState::Idle));

/// 读取当前 NFC 状态
pub fn state() -> NfcState {
    *NFC_STATE.lock()
}

pub fn is_started() -> bool {
    STARTED.load(Ordering::Acquire)
}

// ============================================================================
// ST25DV I2C 协议常量
// ============================================================================

/// ST25DV 出厂设备码 0b1010；I2C_CFG.E0 可选 0/1。
/// E0=1: user/system=0x53/0x57；E0=0: user/system=0x52/0x56。
const ST25DV_DEVICE_CODE: u8 = 0x0A;
const ST25DV_SYSTEM_ADDR_CANDIDATES: [u8; 2] = st25dv_system_addr_candidates(ST25DV_DEVICE_CODE);

const fn st25dv_system_addr_candidates(device_code: u8) -> [u8; 2] {
    let base = (device_code & 0x0F) << 3;
    [base | 0b111, base | 0b110]
}

/// IC_REF 位于系统地址空间 0x0017（SparkFun 官方驱动常量）。
const REG_IC_REF: u16 = 0x0017;
/// Datasheet DS13519 Rev 8, Table 84: ST25DV16KC-IE IC_REF is 0x51.
/// ST25DV16K uses 0x26 (DS10925); both require an independent 2-KiB check.
const ST25DV16KC_IC_REF: u8 = 0x51;
const ST25DV16K_IC_REF: u8 = 0x26;
const REG_ENDA1: u16 = 0x0005;
const REG_I2CSS: u16 = 0x000B;
const REG_MEM_SIZE_BASE: u16 = 0x0014;
const REG_BLOCK_SIZE: u16 = 0x0016;
/// I2C 密码寄存器 (datasheet §3.3.2, 地址 0x0900, 8 字节)
const REG_I2C_PASSWD: u16 = 0x0900;
/// I2C 安全会话状态位于数据地址的动态寄存器 0x2004。
/// bit 0: I2C 安全会话开启标志
const REG_I2C_SSO: u16 = 0x2004;
/// ST25DV FTM (Fast Transfer Mode) static configuration register.
/// Bit 0 enables the RF/I2C mailbox bridge.
const REG_FTM: u16 = 0x000D;
/// DS10925 / AN5633: K has a separate MB_WDG; KC packs it into FTM bits 3:1.
const REG_K_MB_WDG: u16 = 0x000E;
const K_MB_WDG_MASK: u8 = 0x07;
const FTM_MB_MODE: u8 = 1 << 0;
const FTM_MB_WDG_MASK: u8 = 0b0000_1110;
/// ST25DV dynamic mailbox registers (data I2C address, 0x53).
const REG_EH_CTRL_DYN: u16 = 0x2002;
const REG_MB_CTRL_DYN: u16 = 0x2006;
const REG_MB_LEN_DYN: u16 = 0x2007;
const REG_MB_DATA_DYN: u16 = 0x2008;
const EH_CTRL_VCC_ON: u8 = 1 << 3;
const MB_CTRL_MB_EN: u8 = 1 << 0;
const MB_CTRL_HOST_PUT_MSG: u8 = 1 << 1;
const MB_CTRL_RF_PUT_MSG: u8 = 1 << 2;
const MB_CTRL_HOST_MISS_MSG: u8 = 1 << 4;
const MB_CTRL_RF_MISS_MSG: u8 = 1 << 5;
const MB_CTRL_HOST_CURRENT_MSG: u8 = 1 << 6;
const MB_CTRL_RF_CURRENT_MSG: u8 = 1 << 7;
/// ST25DV mailbox holds 256 payload bytes; its length lives in a separate
/// register. The IMCN fragment header/trailer is included in that payload.
const MAILBOX_MAX_BYTES: usize = 256;

const fn mailbox_is_free(ctrl: u8) -> bool {
    ctrl & (MB_CTRL_HOST_PUT_MSG | MB_CTRL_RF_PUT_MSG) == 0
}

const fn mailbox_has_rf_message(ctrl: u8) -> bool {
    ctrl & MB_CTRL_RF_PUT_MSG != 0
}

#[cfg(feature = "ble-at")]
const fn mailbox_response_released(ctrl: Option<u8>, deadline_elapsed: bool) -> bool {
    deadline_elapsed || matches!(ctrl, Some(value) if value & MB_CTRL_HOST_PUT_MSG == 0)
}

#[cfg(feature = "ble-at")]
fn take_deferred_mailbox_reset(pending: &mut bool, response_released: bool) -> bool {
    if response_released {
        std::mem::take(pending)
    } else {
        false
    }
}

/// 用户内存 NDEF 区结束地址 (Area 1)
const NDEF_TEXT_END: u16 = 0x011F;
/// ST25DV16KC 用户 EEPROM 最后一个地址. Datasheet capacity is 2048 bytes;
/// retain the legacy 0x07FF end address and do not probe beyond the part.
const MEMORY_END: u16 = 0x07FF;
/// 每次 I2C 读写最大字节数 (对齐参考固件 RFID_READ_NUMBER=30)
const RFID_CHUNK: usize = 30;

/// 默认 I2C 密码 (8 字节零, datasheet 出厂默认)
const DEFAULT_PASSWORD: [u8; 8] = [0; 8];

/// ST25DV I2C password presentation: password MSB first, verification byte,
/// then repeat the password. Kept pure so the wire vector is host-testable.
fn i2c_password_presentation(password: &[u8; 8]) -> [u8; 17] {
    let mut presentation = [0u8; 17];
    for (index, byte) in password.iter().rev().enumerate() {
        presentation[index] = *byte;
        presentation[index + 9] = *byte;
    }
    presentation[8] = 0x09;
    presentation
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St25dvGeneration {
    K,
    Kc,
}

fn ftm_config_value(current: u8, enabled: bool, generation: St25dvGeneration) -> u8 {
    let mask = match generation {
        St25dvGeneration::K => FTM_MB_MODE,
        St25dvGeneration::Kc => FTM_MB_MODE | FTM_MB_WDG_MASK,
    };
    (current & !mask) | if enabled { FTM_MB_MODE } else { 0 }
}

const fn st25dv16k_generation(
    ic_ref: u8,
    mem_size_lo: u8,
    mem_size_hi: u8,
    block_size: u8,
) -> Option<St25dvGeneration> {
    if mem_size_lo != 0xFF || mem_size_hi != 0x01 || block_size != 0x03 {
        return None;
    }
    match ic_ref {
        ST25DV16K_IC_REF => Some(St25dvGeneration::K),
        ST25DV16KC_IC_REF => Some(St25dvGeneration::Kc),
        _ => None,
    }
}

#[cfg(test)]
const fn is_st25dv16kc_identity(
    ic_ref: u8,
    mem_size_lo: u8,
    mem_size_hi: u8,
    block_size: u8,
) -> bool {
    matches!(
        st25dv16k_generation(ic_ref, mem_size_lo, mem_size_hi, block_size),
        Some(St25dvGeneration::Kc)
    )
}

/// 原 C++ 固件从 0x0120 起直接存放 PRegBuf，直到 0x07FF（含）。
/// 该区域没有头部或 CRC；增加元数据会覆盖旧业务数据或越过 2KB 物理容量。
const NFC_BLOB_DATA_ADDR: u16 = NDEF_TEXT_END + 1;
const NFC_BLOB_DATA_BYTES: usize = (MEMORY_END - NDEF_TEXT_END) as usize;

// ============================================================================
// ST25DV I2C 驱动
// ============================================================================

/// ST25DV I2C 驱动 (封装 SwI2c)
struct St25dv {
    i2c: SwI2c,
    data_addr_7bit: u8,
    system_addr_7bit: u8,
    generation: St25dvGeneration,
}

#[derive(Debug)]
enum St25dvProbeError {
    GpioInit,
    NoSystemAddressAck,
    UnexpectedIcRef(u8),
    SignatureRead,
    WrongSignature { mem_lo: u8, mem_hi: u8, block: u8 },
}

impl St25dv {
    /// 尝试在指定引脚上初始化 ST25DV，并识别 I2C_CFG.E0 两种地址。
    fn try_init(sda: i32, scl: i32) -> Result<Self, St25dvProbeError> {
        let i2c = SwI2c::init(sda, scl).map_err(|_| St25dvProbeError::GpioInit)?;
        let mut dev = Self {
            i2c,
            data_addr_7bit: 0,
            system_addr_7bit: 0,
            generation: St25dvGeneration::Kc,
        };
        let mut unexpected_ic_ref = None;
        for system_addr in ST25DV_SYSTEM_ADDR_CANDIDATES {
            dev.system_addr_7bit = system_addr;
            dev.data_addr_7bit = system_addr - 4;
            let ic_ref = match dev.read_reg16_at(system_addr, REG_IC_REF) {
                Ok(value) => value,
                Err(()) => continue,
            };
            if !matches!(ic_ref, ST25DV16K_IC_REF | ST25DV16KC_IC_REF) {
                unexpected_ic_ref = Some(ic_ref);
                continue;
            }
            let mem_lo = dev
                .read_reg16_at(system_addr, REG_MEM_SIZE_BASE)
                .map_err(|_| St25dvProbeError::SignatureRead)?;
            let mem_hi = dev
                .read_reg16_at(system_addr, REG_MEM_SIZE_BASE + 1)
                .map_err(|_| St25dvProbeError::SignatureRead)?;
            let block = dev
                .read_reg16_at(system_addr, REG_BLOCK_SIZE)
                .map_err(|_| St25dvProbeError::SignatureRead)?;
            dev.generation = st25dv16k_generation(ic_ref, mem_lo, mem_hi, block).ok_or(
                St25dvProbeError::WrongSignature {
                    mem_lo,
                    mem_hi,
                    block,
                },
            )?;
            log::info!(
                "[nfc] ST25DV16{:?} detected on SDA={} SCL={} (system=0x{:02X}, data=0x{:02X}, IC_REF=0x{:02X}, MEM_SIZE=0x{:02X}{:02X}, BLOCK=0x{:02X})",
                dev.generation,
                sda,
                scl,
                system_addr,
                dev.data_addr_7bit,
                ic_ref,
                mem_hi,
                mem_lo,
                block
            );
            return Ok(dev);
        }
        if let Some(ic_ref) = unexpected_ic_ref {
            return Err(St25dvProbeError::UnexpectedIcRef(ic_ref));
        }
        let (sda_high, scl_high) = dev.i2c.line_levels();
        log::info!(
            "[nfc] no ST25DV system-address ACK on SDA={sda}/SCL={scl}; released line levels SDA={} SCL={} (high=1)",
            sda_high as u8,
            scl_high as u8
        );
        Err(St25dvProbeError::NoSystemAddressAck)
    }

    /// 读 16-bit 地址寄存器 (返回 1 字节)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, START, addr_r, data, NACK, STOP
    fn read_reg16_at(&self, addr_7bit: u8, reg: u16) -> Result<u8, ()> {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(addr_7bit << 1) {
            i2c.stop();
            return Err(());
        }
        if !i2c.write_byte((reg >> 8) as u8) {
            i2c.stop();
            return Err(());
        }
        if !i2c.write_byte((reg & 0xFF) as u8) {
            i2c.stop();
            return Err(());
        }
        // Repeated START for read
        i2c.start();
        if !i2c.write_byte((addr_7bit << 1) | 1) {
            i2c.stop();
            return Err(());
        }
        let val = i2c.read_byte(false); // NACK (single byte read)
        i2c.stop();
        Ok(val)
    }

    fn write_bytes_at(
        &self,
        addr_7bit: u8,
        reg: u16,
        data: &[u8],
        wait_write: bool,
    ) -> Result<(), ()> {
        // ST25 写周期内会 NACK。官方驱动为每块提供 6 次、5ms 间隔重试。
        for attempt in 0..6 {
            if self.write_bytes_once(addr_7bit, reg, data) {
                if wait_write {
                    // EEPROM 写周期不是固定 5ms。通过 ACK polling 等待器件真正
                    // ready，避免固定 6ms 后立即读回导致偶发 NACK。
                    for ready_attempt in 0..10 {
                        std::thread::sleep(Duration::from_millis(5));
                        if self.probe_address(addr_7bit) {
                            return Ok(());
                        }
                        if ready_attempt == 9 {
                            return Err(());
                        }
                    }
                }
                return Ok(());
            }
            if attempt != 5 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Err(())
    }

    fn probe_address(&self, addr_7bit: u8) -> bool {
        let i2c = &self.i2c;
        i2c.start();
        let ack = i2c.write_byte(addr_7bit << 1);
        i2c.stop();
        ack
    }

    /// Read a dynamic/system register window without EEPROM write-cycle polling.
    /// The ST25DV mailbox is a volatile register window, so the ordinary EEPROM
    /// write helper must not be used for it.
    fn read_bytes_at(&self, addr_7bit: u8, reg: u16, buf: &mut [u8]) -> Result<(), ()> {
        if buf.is_empty() {
            return Ok(());
        }
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(addr_7bit << 1)
            || !i2c.write_byte((reg >> 8) as u8)
            || !i2c.write_byte((reg & 0xFF) as u8)
        {
            i2c.stop();
            return Err(());
        }
        i2c.start();
        if !i2c.write_byte((addr_7bit << 1) | 1) {
            i2c.stop();
            return Err(());
        }
        let last = buf.len() - 1;
        for (index, byte) in buf.iter_mut().enumerate() {
            *byte = i2c.read_byte(index != last);
        }
        i2c.stop();
        Ok(())
    }

    fn write_dynamic_at(&self, addr_7bit: u8, reg: u16, data: &[u8]) -> Result<(), ()> {
        if data.is_empty() || data.len() > MAILBOX_MAX_BYTES {
            return Err(());
        }
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(addr_7bit << 1)
            || !i2c.write_byte((reg >> 8) as u8)
            || !i2c.write_byte((reg & 0xFF) as u8)
        {
            i2c.stop();
            return Err(());
        }
        for &byte in data {
            if !i2c.write_byte(byte) {
                i2c.stop();
                return Err(());
            }
        }
        i2c.stop();
        Ok(())
    }

    fn set_ftm_enabled(&self, enabled: bool) -> Result<(), ()> {
        let current = self.read_reg16_at(self.system_addr_7bit, REG_FTM)?;
        // The worker services RF messages every 100 ms. Disable the chip's
        // optional mailbox watchdog so a nonzero value inherited from a
        // previous configuration cannot expire a request before it is read.
        // Reserved bits are preserved; MB_WDG=0 means watchdog disabled.
        let desired = ftm_config_value(current, enabled, self.generation);
        if desired != current {
            self.write_bytes_at(self.system_addr_7bit, REG_FTM, &[desired], true)?;
            let verified = self.read_reg16_at(self.system_addr_7bit, REG_FTM)?;
            let managed_bits = match self.generation {
                St25dvGeneration::K => FTM_MB_MODE,
                St25dvGeneration::Kc => FTM_MB_MODE | FTM_MB_WDG_MASK,
            };
            if verified & managed_bits != desired & managed_bits {
                log::warn!(
                    "[nfc] FTM config verify mismatch: expected=0x{:02X} actual=0x{:02X}",
                    desired & managed_bits,
                    verified & managed_bits
                );
                return Err(());
            }
        }
        if self.generation == St25dvGeneration::K {
            let watchdog = self.read_reg16_at(self.system_addr_7bit, REG_K_MB_WDG)?;
            let desired_watchdog = watchdog & !K_MB_WDG_MASK;
            if desired_watchdog != watchdog {
                self.write_bytes_at(
                    self.system_addr_7bit,
                    REG_K_MB_WDG,
                    &[desired_watchdog],
                    true,
                )?;
                let verified = self.read_reg16_at(self.system_addr_7bit, REG_K_MB_WDG)?;
                if verified & K_MB_WDG_MASK != 0 {
                    log::warn!(
                        "[nfc] K Mailbox watchdog verify mismatch: 0x{:02X}",
                        verified
                    );
                    return Err(());
                }
            }
        }
        Ok(())
    }

    /// Enable FTM and the RF/I2C mailbox with the optional mailbox watchdog off.
    fn enable_mailbox(&self) -> Result<(), ()> {
        // ST25DV requires VCC to be present for RF Mailbox access. A board
        // powered only from the NFC field can still expose NDEF, but must not
        // advertise a half-working IMC business channel.
        let eh_ctrl = self.read_reg16_at(self.data_addr_7bit, REG_EH_CTRL_DYN)?;
        if eh_ctrl & EH_CTRL_VCC_ON == 0 {
            return Err(());
        }
        self.set_ftm_enabled(true)?;
        self.set_mailbox_enabled(true)
    }

    fn set_mailbox_enabled(&self, enabled: bool) -> Result<(), ()> {
        let ctrl = self.read_reg16_at(self.data_addr_7bit, REG_MB_CTRL_DYN)?;
        let current = ctrl & MB_CTRL_MB_EN != 0;
        if current == enabled {
            return Ok(());
        }
        let desired = if enabled { MB_CTRL_MB_EN } else { 0 };
        self.write_dynamic_at(self.data_addr_7bit, REG_MB_CTRL_DYN, &[desired])?;
        let verified = self.read_reg16_at(self.data_addr_7bit, REG_MB_CTRL_DYN)?;
        if (verified & MB_CTRL_MB_EN != 0) != enabled {
            log::warn!(
                "[nfc] Mailbox state verify mismatch: expected_enabled={} MB_CTRL_Dyn=0x{:02X}",
                enabled,
                verified
            );
            return Err(());
        }
        Ok(())
    }

    /// EEPROM/system configuration writes require MB_EN=0. The static FTM
    /// authorization remains enabled, so the mailbox can be restored without
    /// a security-session transition.
    fn with_mailbox_temporarily_disabled<F>(&self, operation: F) -> Result<(), ()>
    where
        F: FnOnce(&Self) -> Result<(), ()>,
    {
        let ctrl = self.read_reg16_at(self.data_addr_7bit, REG_MB_CTRL_DYN)?;
        let was_enabled = ctrl & MB_CTRL_MB_EN != 0;
        if was_enabled {
            self.set_mailbox_enabled(false)?;
        }
        let result = operation(self);
        let restore = if was_enabled {
            self.set_mailbox_enabled(true)
        } else {
            Ok(())
        };
        match (result, restore) {
            (Err(operation_error), Err(_)) => {
                log::error!("[nfc] failed to restore Mailbox after maintenance error");
                Err(operation_error)
            }
            (Err(operation_error), Ok(())) => Err(operation_error),
            (Ok(()), Err(restore_error)) => Err(restore_error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    /// Read one RF-originated mailbox message. Reading is bounded and does not
    /// block on the RF side; a message is acknowledged only after its bytes
    /// have been copied successfully.
    fn read_rf_mailbox(&self, buf: &mut [u8; MAILBOX_MAX_BYTES]) -> Result<Option<usize>, ()> {
        let ctrl = self.read_reg16_at(self.data_addr_7bit, REG_MB_CTRL_DYN)?;
        if !mailbox_has_rf_message(ctrl) {
            return Ok(None);
        }
        // MB_LEN_Dyn stores message length minus one. Reading through the last
        // byte and issuing STOP is what releases RF_PUT_MSG; the status bit is
        // not writable from I2C.
        let len = self.read_reg16_at(self.data_addr_7bit, REG_MB_LEN_DYN)? as usize + 1;
        if len == 0 || len > MAILBOX_MAX_BYTES {
            log::warn!("[nfc] RF mailbox length invalid: {}", len);
            return Ok(None);
        }
        self.read_bytes_at(self.data_addr_7bit, REG_MB_DATA_DYN, &mut buf[..len])?;
        Ok(Some(len))
    }

    /// Publish one response for the RF reader. The payload is a complete IMC
    /// BLE frame; callers must split larger responses at the protocol layer.
    fn write_host_mailbox(&self, data: &[u8]) -> Result<(), ()> {
        if data.is_empty() || data.len() > MAILBOX_MAX_BYTES {
            return Err(());
        }
        let ctrl = self.read_reg16_at(self.data_addr_7bit, REG_MB_CTRL_DYN)?;
        if !mailbox_is_free(ctrl) {
            return Err(());
        }
        // An I2C mailbox write must start at 0x2008. The chip updates
        // MB_LEN_Dyn (length minus one) and HOST_PUT_MSG at STOP; neither is
        // software-written here because all status bits except MB_EN are RO.
        self.write_dynamic_at(self.data_addr_7bit, REG_MB_DATA_DYN, data)?;
        // DS13519 Rev.8 Appendix B.3.2 requires I2C ACK polling after a
        // mailbox sequential write before the host starts another command.
        // Mailbox writes are immediate (unlike EEPROM writes), so poll without
        // replaying the payload: replaying could publish the same message twice.
        for attempt in 0..10 {
            if self.probe_address(self.data_addr_7bit) {
                return Ok(());
            }
            if attempt != 9 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        log::warn!("[nfc] ST25DV mailbox did not ACK after host message write");
        Err(())
    }

    fn write_bytes_once(&self, addr_7bit: u8, reg: u16, data: &[u8]) -> bool {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(addr_7bit << 1) {
            i2c.stop();
            return false;
        }
        if !i2c.write_byte((reg >> 8) as u8) {
            i2c.stop();
            return false;
        }
        if !i2c.write_byte((reg & 0xFF) as u8) {
            i2c.stop();
            return false;
        }
        for &byte in data {
            if !i2c.write_byte(byte) {
                i2c.stop();
                return false;
            }
        }
        i2c.stop();
        true
    }

    /// 打开 I2C 安全会话 (写 8 字节密码到 I2C_PASSWD 寄存器)
    ///
    /// 对齐参考固件: `tag.openI2CSession(password)`
    fn open_i2c_session(&self) -> Result<(), ()> {
        self.with_mailbox_temporarily_disabled(|this| {
            let presentation = i2c_password_presentation(&DEFAULT_PASSWORD);
            this.write_bytes_at(this.system_addr_7bit, REG_I2C_PASSWD, &presentation, false)?;
            match this.read_reg16_at(this.data_addr_7bit, REG_I2C_SSO) {
                Ok(v) if v & 0x01 != 0 => {
                    log::debug!("[nfc] I2C security session opened");
                    Ok(())
                }
                Ok(v) => {
                    log::warn!("[nfc] I2C session open failed (SSO=0x{:02X})", v);
                    Err(())
                }
                Err(_) => Err(()),
            }
        })
    }

    /// 对齐原 C++ 的 EEPROM 区域策略：Area 1 (NDEF) 可直接写，Area 2
    /// (配置快照) 仅在安全会话打开时可写。
    fn ensure_write_protection(&self) -> Result<(), ()> {
        const AREA1_WRITE_SECURED: u8 = 1 << 0;
        const AREA2_WRITE_SECURED: u8 = 1 << 2;

        let current = self.read_reg16_at(self.system_addr_7bit, REG_I2CSS)?;
        let desired = (current & !AREA1_WRITE_SECURED) | AREA2_WRITE_SECURED;
        if desired != current {
            self.write_bytes_at(self.system_addr_7bit, REG_I2CSS, &[desired], true)?;
            let verified = self.read_reg16_at(self.system_addr_7bit, REG_I2CSS)?;
            if verified != desired {
                log::error!(
                    "[nfc] I2CSS verify mismatch: expected=0x{:02X} actual=0x{:02X}",
                    desired,
                    verified
                );
                return Err(());
            }
            log::info!(
                "[nfc] I2CSS aligned: 0x{:02X} -> 0x{:02X} (Area1 open, Area2 secured)",
                current,
                desired
            );
        }
        Ok(())
    }

    fn log_device_security(&self) {
        let enda1 = self.read_reg16_at(self.system_addr_7bit, REG_ENDA1);
        let i2css = self.read_reg16_at(self.system_addr_7bit, REG_I2CSS);
        let sso = self.read_reg16_at(self.data_addr_7bit, REG_I2C_SSO);
        let mem_lo = self.read_reg16_at(self.system_addr_7bit, REG_MEM_SIZE_BASE);
        let mem_hi = self.read_reg16_at(self.system_addr_7bit, REG_MEM_SIZE_BASE + 1);
        let block = self.read_reg16_at(self.system_addr_7bit, REG_BLOCK_SIZE);
        log::info!(
            "[nfc] registers: ENDA1={:?} I2CSS={:?} I2C_SSO={:?} MEM_SIZE={:?}/{:?} BLOCK={:?}",
            enda1,
            i2css,
            sso,
            mem_lo,
            mem_hi,
            block
        );
    }

    fn ensure_ndef_area(&self) -> Result<(), ()> {
        // Area 1 end = value * 32 + 31；0x08 对应 0x011F，与原固件一致。
        let current = self.read_reg16_at(self.system_addr_7bit, REG_ENDA1)?;
        if current != 0x08 {
            self.write_bytes_at(self.system_addr_7bit, REG_ENDA1, &[0x08], true)?;
            let verified = self.read_reg16_at(self.system_addr_7bit, REG_ENDA1)?;
            if verified != 0x08 {
                return Err(());
            }
            log::info!("[nfc] NDEF Area 1 end configured to 0x011F");
        }
        Ok(())
    }

    /// 从用户 EEPROM 读取数据 (16-bit 地址, 多字节)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, START, addr_r, data[0..n-1] ACK, data[n] NACK, STOP
    fn read_eeprom(&self, addr: u16, buf: &mut [u8]) -> Result<usize, ()> {
        let mut offset = 0usize;
        while offset < buf.len() {
            let end = (offset + RFID_CHUNK).min(buf.len());
            self.read_eeprom_chunk(addr + offset as u16, &mut buf[offset..end])?;
            offset = end;
            std::thread::yield_now();
        }
        Ok(buf.len())
    }

    fn read_eeprom_chunk(&self, addr: u16, buf: &mut [u8]) -> Result<(), ()> {
        for attempt in 0..6 {
            if self.read_eeprom_chunk_once(addr, buf).is_ok() {
                return Ok(());
            }
            if attempt != 5 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Err(())
    }

    fn read_eeprom_chunk_once(&self, addr: u16, buf: &mut [u8]) -> Result<(), ()> {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(self.data_addr_7bit << 1) {
            i2c.stop();
            return Err(());
        }
        if !i2c.write_byte((addr >> 8) as u8) {
            i2c.stop();
            return Err(());
        }
        if !i2c.write_byte((addr & 0xFF) as u8) {
            i2c.stop();
            return Err(());
        }
        // Repeated START for read
        i2c.start();
        if !i2c.write_byte((self.data_addr_7bit << 1) | 1) {
            i2c.stop();
            return Err(());
        }
        let n = buf.len();
        for (i, byte) in buf.iter_mut().enumerate() {
            let ack = i < n - 1; // ACK all but last byte
            *byte = i2c.read_byte(ack);
        }
        i2c.stop();
        Ok(())
    }

    /// 向用户 EEPROM 写入数据 (16-bit 地址, 多字节)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, data[0..n], STOP
    /// 注意: ST25DV 内部写周期约 5ms。每块写后读回比较，拒绝静默损坏。
    fn write_eeprom(&self, addr: u16, data: &[u8]) -> Result<(), ()> {
        if data.is_empty() || data.len() > RFID_CHUNK {
            return Err(());
        }
        let mut verify = [0u8; RFID_CHUNK];
        let mut transfer_failures = 0u8;
        let mut read_failures = 0u8;
        let mut mismatch = None;
        let result = self.with_mailbox_temporarily_disabled(|this| {
            for _ in 0..3 {
                if this
                    .write_bytes_at(self.data_addr_7bit, addr, data, true)
                    .is_err()
                {
                    transfer_failures += 1;
                    continue;
                }
                if this
                    .read_eeprom_chunk(addr, &mut verify[..data.len()])
                    .is_err()
                {
                    read_failures += 1;
                    continue;
                }
                if verify[..data.len()] == *data {
                    return Ok(());
                }
                mismatch = data
                    .iter()
                    .zip(verify.iter())
                    .position(|(expected, actual)| expected != actual)
                    .map(|index| (index, data[index], verify[index]));
            }
            Err(())
        });
        if result.is_ok() {
            return Ok(());
        }
        log::error!(
            "[nfc] EEPROM write failed: addr=0x{:04X} len={} tx_fail={} read_fail={} mismatch={:?}",
            addr,
            data.len(),
            transfer_failures,
            read_failures,
            mismatch
        );
        Err(())
    }
}

/// CRC32 (IEEE 802.3, 与 device/mod.rs 中 crc32 相同)
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// NFC 备份数据字数 (NFC_BLOB_DATA_BYTES / 2)
const NFC_BLOB_DATA_WORDS: usize = NFC_BLOB_DATA_BYTES / 2;

/// NFC RF Mailbox transport envelope used only when an IMC BLE frame does
/// not fit in the 256-byte ST25DV mailbox. Direct BLE frames remain accepted
/// for all requests that fit, preserving the handheld protocol byte-for-byte.
#[cfg(feature = "ble-at")]
const MAILBOX_ENVELOPE_MAGIC: [u8; 4] = *b"IMCN";
#[cfg(feature = "ble-at")]
const MAILBOX_ENVELOPE_VERSION: u8 = 1;
#[cfg(feature = "ble-at")]
const MAILBOX_ENVELOPE_HEADER: usize = 12;
#[cfg(feature = "ble-at")]
const MAILBOX_ENVELOPE_OVERHEAD: usize = MAILBOX_ENVELOPE_HEADER + 2;
#[cfg(feature = "ble-at")]
const MAILBOX_CHUNK_MAX: usize = MAILBOX_MAX_BYTES - MAILBOX_ENVELOPE_OVERHEAD;
#[cfg(feature = "ble-at")]
const MAILBOX_REASSEMBLY_MAX: usize = 512;

#[cfg(feature = "ble-at")]
struct MailboxAssembler {
    data: [u8; MAILBOX_REASSEMBLY_MAX],
    total: usize,
    received: usize,
    next_seq: u8,
    active: bool,
}

#[cfg(feature = "ble-at")]
impl MailboxAssembler {
    const fn new() -> Self {
        Self {
            data: [0; MAILBOX_REASSEMBLY_MAX],
            total: 0,
            received: 0,
            next_seq: 0,
            active: false,
        }
    }

    fn reset(&mut self) {
        self.total = 0;
        self.received = 0;
        self.next_seq = 0;
        self.active = false;
    }

    /// Decode either a direct IMC BLE frame or one IMCN fragment. The returned
    /// frame owns a bounded heapless buffer and is ready for the BLE dispatcher.
    fn accept(&mut self, message: &[u8]) -> Option<heapless::Vec<u8, MAILBOX_REASSEMBLY_MAX>> {
        if message.len() < 4 || message[..4] != MAILBOX_ENVELOPE_MAGIC {
            // A direct request starts a new exchange. Never retain fragments
            // from an interrupted prior request across an unrelated frame.
            self.reset();
            if message.len() > MAILBOX_REASSEMBLY_MAX {
                return None;
            }
            let mut frame = heapless::Vec::new();
            frame.extend_from_slice(message).ok()?;
            return Some(frame);
        }
        if message.len() < MAILBOX_ENVELOPE_OVERHEAD || message[4] != MAILBOX_ENVELOPE_VERSION {
            self.reset();
            return None;
        }
        let flags = message[5];
        let seq = message[6];
        let total = u16::from_be_bytes([message[7], message[8]]) as usize;
        let offset = u16::from_be_bytes([message[9], message[10]]) as usize;
        let payload_len = message.len() - MAILBOX_ENVELOPE_OVERHEAD;
        if message[11] as usize != payload_len
            || total == 0
            || total > MAILBOX_REASSEMBLY_MAX
            || payload_len == 0
            || payload_len > MAILBOX_CHUNK_MAX
            || flags & !0x03 != 0
            || (flags & 0x01 != 0 && (seq != 0 || offset != 0))
            || offset.checked_add(payload_len)? > total
        {
            self.reset();
            return None;
        }
        let crc_at = MAILBOX_ENVELOPE_HEADER + payload_len;
        let expected = u16::from_le_bytes([message[crc_at], message[crc_at + 1]]);
        let actual = crate::modbus::shared::modbus_crc16(&message[..crc_at]);
        if expected != actual {
            self.reset();
            return None;
        }
        if flags & 0x01 != 0 {
            self.reset();
            self.total = total;
            self.active = true;
        }
        if !self.active || self.total != total || seq != self.next_seq || offset != self.received {
            self.reset();
            return None;
        }
        self.data[offset..offset + payload_len]
            .copy_from_slice(&message[MAILBOX_ENVELOPE_HEADER..crc_at]);
        self.received += payload_len;
        self.next_seq = self.next_seq.wrapping_add(1);
        if flags & 0x02 == 0 || self.received != self.total {
            return None;
        }
        let mut frame = heapless::Vec::new();
        frame.extend_from_slice(&self.data[..self.total]).ok()?;
        self.reset();
        Some(frame)
    }
}

#[cfg(feature = "ble-at")]
fn mailbox_fragment(
    data: &[u8],
    offset: usize,
    seq: u8,
    out: &mut heapless::Vec<u8, MAILBOX_MAX_BYTES>,
) -> Option<bool> {
    if data.is_empty() || data.len() > MAILBOX_REASSEMBLY_MAX || offset >= data.len() {
        return None;
    }
    let length = MAILBOX_CHUNK_MAX.min(data.len() - offset);
    let first = offset == 0;
    let last = offset + length == data.len();
    out.clear();
    out.extend_from_slice(&MAILBOX_ENVELOPE_MAGIC).ok()?;
    out.push(MAILBOX_ENVELOPE_VERSION).ok()?;
    out.push((if first { 0x01 } else { 0 }) | if last { 0x02 } else { 0 })
        .ok()?;
    out.push(seq).ok()?;
    out.extend_from_slice(&(data.len() as u16).to_be_bytes())
        .ok()?;
    out.extend_from_slice(&(offset as u16).to_be_bytes()).ok()?;
    out.push(length as u8).ok()?;
    out.extend_from_slice(&data[offset..offset + length]).ok()?;
    let crc = crate::modbus::shared::modbus_crc16(out.as_slice());
    out.extend_from_slice(&crc.to_le_bytes()).ok()?;
    Some(last)
}

/// 固定驻留 PSRAM 的零初始化工作区，避免 NFC 缓冲占用 pthread 所需 internal SRAM。
struct PsramBuffer<T> {
    ptr: NonNull<T>,
    len: usize,
}

impl<T> PsramBuffer<T> {
    /// 分配 `len` 个全零元素。
    ///
    /// # Safety
    ///
    /// `T` 的全零位模式必须是有效值。
    unsafe fn zeroed(len: usize) -> Option<Self> {
        if len == 0 || core::mem::size_of::<T>() == 0 {
            return None;
        }
        let ptr = unsafe {
            // SAFETY: 参数经上方检查；返回值立即判空，成功时拥有 len 个 T 的空间。
            esp_idf_sys::heap_caps_calloc(
                len,
                core::mem::size_of::<T>(),
                esp_idf_sys::MALLOC_CAP_SPIRAM | esp_idf_sys::MALLOC_CAP_8BIT,
            )
        } as *mut T;
        Some(Self {
            ptr: NonNull::new(ptr)?,
            len,
        })
    }
}

impl<T> Deref for PsramBuffer<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        unsafe {
            // SAFETY: ptr 来自 len 个 T 的 calloc，直到 Drop 前保持有效且此处仅共享访问。
            core::slice::from_raw_parts(self.ptr.as_ptr(), self.len)
        }
    }
}

impl<T> DerefMut for PsramBuffer<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        unsafe {
            // SAFETY: &mut self 保证该切片在本次借用期间独占。
            core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len)
        }
    }
}

impl<T> Drop for PsramBuffer<T> {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: ptr 由 heap_caps_calloc 返回且只在这里释放一次。
            esp_idf_sys::heap_caps_free(self.ptr.as_ptr().cast());
        }
    }
}

// SAFETY: 缓冲区拥有其分配，跨线程移动不会产生别名；T: Send 保持元素约束。
unsafe impl<T: Send> Send for PsramBuffer<T> {}

// ============================================================================
// 启动入口
// ============================================================================

/// 启动 ST25DV16KC NDEF、配置备份和 RF Mailbox/FTM 模块。
///
/// Reference-firmware EEPROM/NDEF layout remains compatible. This function
/// starts the worker before PCA9555 initialization; the physical I2C arbiter
/// serializes access to the shared software-I2C bus.
///
/// EEPROM/NDEF work is change-driven. RF Mailbox is polled at 100 ms while the
/// tag is present; transport failures use bounded exponential retry backoff.
pub fn start() -> AppResult<()> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    // SAFETY: `u8` 的所有位模式均有效。
    let data_buf = match unsafe { PsramBuffer::<u8>::zeroed(NFC_BLOB_DATA_BYTES) } {
        Some(buffer) => buffer,
        None => {
            STARTED.store(false, Ordering::Release);
            return Err(crate::error::AppError::Sys(
                "allocate NFC data workspace in PSRAM".into(),
            ));
        }
    };
    // SAFETY: `u16` 的所有位模式均有效。
    let nfc_words = match unsafe { PsramBuffer::<u16>::zeroed(NFC_BLOB_DATA_WORDS) } {
        Some(buffer) => buffer,
        None => {
            STARTED.store(false, Ordering::Release);
            return Err(crate::error::AppError::Sys(
                "allocate NFC word workspace in PSRAM".into(),
            ));
        }
    };
    let spawn_result = std::thread::Builder::new()
        .name("nfc-st25".into())
        .stack_size(crate::safety::stack_budget::NFC)
        .spawn(move || nfc_loop(data_buf, nfc_words));
    if let Err(e) = spawn_result {
        STARTED.store(false, Ordering::Release);
        return Err(crate::error::AppError::Sys(format!("spawn nfc: {e}")));
    }
    health::register_with_stack(&TASK_HB, crate::safety::stack_budget::NFC);
    log::info!(
        "[nfc] background thread spawned (FTM mailbox 100ms, change-driven EEPROM maintenance, bounded retry backoff)"
    );
    Ok(())
}

/// NFC 后台轮询循环
fn nfc_loop(mut data_buf: PsramBuffer<u8>, mut nfc_words: PsramBuffer<u16>) {
    let mut present = false;
    let mut diagnostics_logged = false;
    let mut i2c_session_open = false;
    let mut ndef_initialized = false;
    let mut empty_snapshot_logged = false;
    let mut snapshot_examined = false;
    let mut probe_unavailable_logged = false;
    // GPIO38/37 is shared with the status expander.  Keep the successfully
    // probed software-I2C instance for the lifetime of a present tag: tearing
    // it down and reconfiguring GPIOs on every poll creates needless bus
    // traffic and floods the serial log on an empty, healthy tag.
    let mut dev: Option<St25dv> = None;
    let mut retry_cooldown_ticks = 0u16;
    let mut retry_level = 0usize;
    #[cfg(feature = "ble-at")]
    let mut mailbox_rx = MailboxAssembler::new();
    #[cfg(feature = "ble-at")]
    let mut mailbox_buf = [0u8; MAILBOX_MAX_BYTES];
    #[cfg(feature = "ble-at")]
    let mut mailbox_pending = heapless::Vec::<u8, MAILBOX_MAX_BYTES>::new();
    #[cfg(feature = "ble-at")]
    let mut mailbox_response = [0u8; MAILBOX_REASSEMBLY_MAX];
    #[cfg(feature = "ble-at")]
    let mut mailbox_response_len = 0usize;
    #[cfg(feature = "ble-at")]
    let mut mailbox_response_offset = 0usize;
    #[cfg(feature = "ble-at")]
    let mut mailbox_response_seq = 0u8;
    #[cfg(feature = "ble-at")]
    let mut mailbox_reset_pending = false;
    #[cfg(feature = "ble-at")]
    let mut mailbox_response_waiting_rf = false;
    #[cfg(feature = "ble-at")]
    let mut mailbox_response_deadline = None;
    #[cfg(feature = "ble-at")]
    let mut mailbox_ready_logged = false;
    const RETRY_TICKS: [u16; 3] = [6, 24, 60]; // 30s, 120s, 300s
    // 两个有界工作区由 start() 显式分配到 PSRAM 并移交本任务，循环内持续复用。
    // LOOP9: 订阅硬件 WDT, 否则 feed_wdt() 高频报 "task not found" 刷屏
    health::subscribe_wdt();

    loop {
        TASK_HB.tick();
        health::feed_wdt();

        // 手动命令可立即绕过自动退避；自动失败不会持续冲击 I2C 总线。
        if retry_cooldown_ticks > 0 && NFC_COMMAND.load(Ordering::Acquire) == NFC_CMD_AUTO {
            retry_cooldown_ticks -= 1;
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }

        if dev.is_none() {
            *NFC_STATE.lock() = NfcState::Init;

            // 1. 尝试初始化 ST25DV (先 LED 总线 38/37, 再 IO 总线 35/36)
            //    对齐参考固件: Wire.setPins(38,37) → Wire.setPins(35,36)
            let led_probe = St25dv::try_init(
                crate::config::pins::NCA9555_LED_SDA as i32,
                crate::config::pins::NCA9555_LED_SCL as i32,
            );
            dev = match led_probe {
                Ok(device) => Some(device),
                Err(led_error) => match St25dv::try_init(
                    crate::config::pins::NCA9555_IIC_SDA as i32,
                    crate::config::pins::NCA9555_IIC_SCL as i32,
                ) {
                    Ok(device) => Some(device),
                    Err(io_error) => {
                        if !probe_unavailable_logged {
                            log::info!(
                                "[nfc] ST25DV probe failed: SDA={}/SCL={}={:?}; SDA={}/SCL={}={:?}; main services continue",
                                crate::config::pins::NCA9555_LED_SDA,
                                crate::config::pins::NCA9555_LED_SCL,
                                led_error,
                                crate::config::pins::NCA9555_IIC_SDA,
                                crate::config::pins::NCA9555_IIC_SCL,
                                io_error
                            );
                            probe_unavailable_logged = true;
                        }
                        None
                    }
                },
            };

            if dev.is_none() {
                if present {
                    log::info!("[nfc] tag removed");
                    present = false;
                }
                *NFC_STATE.lock() = NfcState::Idle;
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }

            diagnostics_logged = false;
            probe_unavailable_logged = false;
            i2c_session_open = false;
            ndef_initialized = false;
            empty_snapshot_logged = false;
            snapshot_examined = false;
            #[cfg(feature = "ble-at")]
            {
                mailbox_ready_logged = false;
            }
            log::info!("[nfc] tag detected");
            present = true;
        }
        let Some(active_dev) = dev.as_ref() else {
            // The tag may be removed between probing and this iteration. Keep
            // the worker alive and let the bounded retry path probe again.
            *NFC_STATE.lock() = NfcState::Idle;
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        // Volatile mailbox contents can survive an MCU reset while the tag
        // stays powered. Discard stale messages once before starting a fresh
        // security session, otherwise HOST_PUT_MSG can block initialization.
        if !i2c_session_open && active_dev.set_mailbox_enabled(false).is_err() {
            *NFC_STATE.lock() = NfcState::Error;
        }
        {
            let mut state = NFC_STATE.lock();
            if matches!(*state, NfcState::Idle | NfcState::Init) {
                *state = NfcState::Detected;
            }
        }

        // EEPROM/NDEF access only when initialization, a manual request, or a
        // settled local mutation requires it. Never interrupt a live RF message.
        let command_pending = NFC_COMMAND.load(Ordering::Acquire) != NFC_CMD_AUTO;
        let local_dirty = crate::bus::storage_state::HOLDING_NFC_DIRTY
            .load(std::sync::atomic::Ordering::Acquire)
            && (command_pending || crate::bus::storage_state::storage_mutation_quiet_for(2_000));
        let maintenance_due =
            !ndef_initialized || !snapshot_examined || local_dirty || command_pending;
        let mailbox_free_for_maintenance = !maintenance_due
            || active_dev
                .read_reg16_at(active_dev.data_addr_7bit, REG_MB_CTRL_DYN)
                .is_ok_and(mailbox_is_free);
        if maintenance_due && mailbox_free_for_maintenance && *NFC_STATE.lock() != NfcState::Error {
            // 2. Open the I2C security session once per tag presence. Keeping
            // it open avoids disabling FTM every five seconds and prevents an
            // in-flight RF mailbox message from being discarded.
            if !i2c_session_open {
                if active_dev.open_i2c_session().is_err() {
                    log::warn!("[nfc] I2C session open failed");
                    *NFC_STATE.lock() = NfcState::Error;
                } else {
                    i2c_session_open = true;
                }
            }
            if *NFC_STATE.lock() != NfcState::Error
                && active_dev
                    .with_mailbox_temporarily_disabled(|dev| {
                        dev.ensure_write_protection()?;
                        dev.ensure_ndef_area()
                    })
                    .is_err()
            {
                log::warn!("[nfc] EEPROM/NDEF configuration alignment failed");
                *NFC_STATE.lock() = NfcState::Error;
            }

            if *NFC_STATE.lock() != NfcState::Error {
                if !diagnostics_logged {
                    active_dev.log_device_security();
                    diagnostics_logged = true;
                }

                let command = NFC_COMMAND.swap(NFC_CMD_AUTO, Ordering::AcqRel);
                let update_ndef = !ndef_initialized || command == NFC_CMD_BACKUP || local_dirty;
                let mut ndef_failed = false;
                if update_ndef {
                    if write_ndef_records(active_dev).is_ok() {
                        ndef_initialized = true;
                    } else {
                        log::warn!("[nfc] NDEF update failed; raw snapshot handling continues");
                        ndef_failed = true;
                    }
                }

                // 原 C++ 的 0x0120..0x07FF 是无头原始 PRegBuf。自动流程绝不把 NFC
                // 覆盖到本机；只有显式 restore 才恢复，dirty/显式 backup 才写入。
                let backup_requested =
                    command == NFC_CMD_BACKUP || (command == NFC_CMD_AUTO && local_dirty);
                if backup_requested {
                    // 在取得快照前消费 dirty。备份期间的新写入会重新置位，成功后
                    // 不再清零，避免长 EEPROM 写周期吞掉并发配置更新。
                    crate::bus::storage_state::HOLDING_NFC_DIRTY
                        .swap(false, std::sync::atomic::Ordering::AcqRel);
                }
                let regbuf_arc = storage_read();
                let regbuf = regbuf_arc.as_ref().map(|s| s.holding_buf.as_ref());
                match (command, regbuf) {
                    (_, None) => {
                        if backup_requested {
                            crate::bus::storage_state::HOLDING_NFC_DIRTY
                                .store(true, std::sync::atomic::Ordering::Release);
                        }
                        *NFC_STATE.lock() = NfcState::Error;
                    }
                    (NFC_CMD_BACKUP, Some(rb)) => {
                        log::info!("[nfc] manual backup executing");
                        finish_backup(backup_to_nfc(active_dev, rb, &mut data_buf));
                        snapshot_examined = true;
                    }
                    (NFC_CMD_RESTORE, Some(_)) => {
                        log::info!("[nfc] manual restore executing");
                        match active_dev.read_eeprom(NFC_BLOB_DATA_ADDR, &mut data_buf) {
                            Ok(_) if snapshot_valid_for_restore(&data_buf) => {
                                snapshot_examined = true;
                                bytes_to_words(&data_buf, &mut nfc_words);
                                restore_from_nfc(&nfc_words);
                                LAST_NFC_CRC.store(crc32(&data_buf), Ordering::Relaxed);
                                *NFC_STATE.lock() = NfcState::Restored;
                            }
                            _ => {
                                snapshot_examined = true;
                                log::error!(
                                    "[nfc] manual restore rejected: snapshot empty/unreadable"
                                );
                                *NFC_STATE.lock() = NfcState::Error;
                            }
                        }
                    }
                    (NFC_CMD_AUTO, Some(rb)) if local_dirty => {
                        log::info!("[nfc] local configuration dirty, backing up");
                        finish_backup(backup_to_nfc(active_dev, rb, &mut data_buf));
                        snapshot_examined = true;
                    }
                    // A full snapshot is only needed once after tag detection.  On
                    // a healthy idle device the session probe above is sufficient
                    // to detect removal; repeatedly reading 1760 bytes would
                    // contend with PCA9555 on the shared GPIO I2C bus.
                    (NFC_CMD_AUTO, Some(rb)) if !snapshot_examined => {
                        match active_dev.read_eeprom(NFC_BLOB_DATA_ADDR, &mut data_buf) {
                            Ok(_) if legacy_snapshot_valid(&data_buf) => {
                                snapshot_examined = true;
                                empty_snapshot_logged = false;
                                LAST_NFC_CRC.store(crc32(&data_buf), Ordering::Relaxed);
                                bytes_to_words(&data_buf, &mut nfc_words);
                                if regbuf_equal(rb, &nfc_words) {
                                    log::debug!("[nfc] legacy snapshot matches local prefix");
                                } else {
                                    log::info!(
                                        "[nfc] legacy snapshot differs; waiting for explicit restore"
                                    );
                                }
                                if *NFC_STATE.lock() != NfcState::Error {
                                    *NFC_STATE.lock() = NfcState::Detected;
                                }
                            }
                            Ok(_) => {
                                snapshot_examined = true;
                                if !empty_snapshot_logged {
                                    log::info!(
                                        "[nfc] legacy snapshot empty; waiting for backup request"
                                    );
                                    empty_snapshot_logged = true;
                                }
                            }
                            Err(_) => {
                                snapshot_examined = true;
                                log::warn!("[nfc] legacy snapshot read failed");
                                *NFC_STATE.lock() = NfcState::Error;
                            }
                        }
                    }
                    (NFC_CMD_AUTO, Some(_)) => {}
                    _ => unreachable!(),
                }
                if ndef_failed {
                    *NFC_STATE.lock() = NfcState::Error;
                }
            }

            // Keep the security session open while this tag remains present;
            // it is volatile and is discarded naturally after tag removal.
        }

        // FTM mailbox servicing is deliberately kept in this worker so the
        // NFC driver instance stays in one task and uses the shared bus lock.
        // It runs at 100 ms while a tag is present, independent of the slower EEPROM backup cadence above.
        #[cfg(feature = "ble-at")]
        {
            if *NFC_STATE.lock() != NfcState::Error && active_dev.enable_mailbox().is_err() {
                log::warn!("[nfc] Mailbox enable/readback failed; reprobe required");
                *NFC_STATE.lock() = NfcState::Error;
            }
            if *NFC_STATE.lock() != NfcState::Error {
                if !mailbox_ready_logged {
                    log::info!(
                        "[nfc] RF Mailbox ready (generation={:?}, MB_EN=1, watchdog=off)",
                        active_dev.generation
                    );
                    mailbox_ready_logged = true;
                }
                if mailbox_response_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    // A phone can leave without consuming a fragment. With
                    // the hardware watchdog off, recover explicitly instead
                    // of leaving the mailbox permanently full.
                    log::warn!("[nfc] RF response expired; clearing stale mailbox");
                    *NFC_STATE.lock() = NfcState::Error;
                }
                // A network/MAC write is persisted while dispatching the RF
                // request, but reset is deferred until the final response has
                // been consumed by the phone.  This closes the response-loss
                // race that is harmless on BLE notifications but fatal on a
                // request/response RF mailbox.
                if mailbox_response_waiting_rf {
                    let ctrl = active_dev
                        .read_reg16_at(active_dev.data_addr_7bit, REG_MB_CTRL_DYN)
                        .ok();
                    let deadline_elapsed = mailbox_response_deadline
                        .is_some_and(|deadline| Instant::now() >= deadline);
                    let response_released = mailbox_response_released(ctrl, deadline_elapsed);
                    if response_released {
                        mailbox_response_waiting_rf = false;
                        mailbox_response_deadline = None;
                    }
                    if take_deferred_mailbox_reset(&mut mailbox_reset_pending, response_released) {
                        crate::bus::IO
                            .sys
                            .request_reset(crate::bus::io_state::ResetSource::NFC);
                        log::info!(
                            "[nfc] mailbox configuration response released; device reset queued"
                        );
                    }
                }
                if *NFC_STATE.lock() != NfcState::Error
                    && mailbox_pending.is_empty()
                    && mailbox_response_offset < mailbox_response_len
                {
                    let _ = mailbox_fragment(
                        &mailbox_response[..mailbox_response_len],
                        mailbox_response_offset,
                        mailbox_response_seq,
                        &mut mailbox_pending,
                    );
                }
                if *NFC_STATE.lock() != NfcState::Error
                    && !mailbox_pending.is_empty()
                    && active_dev
                        .write_host_mailbox(mailbox_pending.as_slice())
                        .is_ok()
                {
                    mailbox_response_deadline = Some(Instant::now() + Duration::from_secs(10));
                    if mailbox_response_len == 0 {
                        mailbox_pending.clear();
                    } else {
                        let sent = (mailbox_response_offset + MAILBOX_CHUNK_MAX)
                            .min(mailbox_response_len)
                            - mailbox_response_offset;
                        mailbox_response_offset += sent;
                        mailbox_response_seq = mailbox_response_seq.wrapping_add(1);
                        mailbox_pending.clear();
                        if mailbox_response_offset >= mailbox_response_len {
                            mailbox_response_len = 0;
                            mailbox_response_offset = 0;
                            mailbox_response_seq = 0;
                            mailbox_response_waiting_rf = true;
                            mailbox_response_deadline =
                                Some(Instant::now() + Duration::from_secs(10));
                        }
                    }
                }
                if *NFC_STATE.lock() != NfcState::Error
                    && mailbox_pending.is_empty()
                    && mailbox_response_len == 0
                    && !mailbox_response_waiting_rf
                {
                    match active_dev.read_rf_mailbox(&mut mailbox_buf) {
                        Ok(Some(len)) => {
                            if let Some(frame) = mailbox_rx.accept(&mailbox_buf[..len])
                                && let Some(response) =
                                    crate::ble_at::process_mailbox_binary(&frame)
                                && response.len() <= MAILBOX_REASSEMBLY_MAX
                            {
                                mailbox_response[..response.len()].copy_from_slice(&response);
                                mailbox_response_len = response.len();
                                mailbox_response_offset = 0;
                                mailbox_response_seq = 0;
                                mailbox_reset_pending = crate::ble_at::take_mailbox_reset_request();
                                mailbox_response_deadline =
                                    Some(Instant::now() + Duration::from_secs(10));
                            }
                        }
                        Ok(None) => {}
                        Err(()) => {
                            log::warn!("[nfc] RF Mailbox read failed; reprobe required");
                            *NFC_STATE.lock() = NfcState::Error;
                        }
                    }
                }
            }
        }

        if *NFC_STATE.lock() == NfcState::Error {
            // A failed transfer may indicate tag removal or a bus reset.  Drop
            // the instance so the next bounded retry reprobes and reinitializes
            // GPIOs only when that recovery is actually needed.
            dev = None;
            i2c_session_open = false;
            #[cfg(feature = "ble-at")]
            {
                // Never carry an RF response or deferred reset across tag
                // removal.  A later tag presentation must start a fresh
                // request/response exchange and must not reboot unexpectedly.
                if mailbox_reset_pending {
                    crate::bus::IO
                        .sys
                        .request_reset(crate::bus::io_state::ResetSource::NFC);
                    log::warn!(
                        "[nfc] tag lost before configuration response was read; persisted changes will be applied by reset"
                    );
                }
                mailbox_rx.reset();
                mailbox_pending.clear();
                mailbox_response_len = 0;
                mailbox_response_offset = 0;
                mailbox_response_seq = 0;
                mailbox_reset_pending = false;
                mailbox_response_waiting_rf = false;
                mailbox_response_deadline = None;
            }
            if present {
                log::info!("[nfc] tag unavailable; reprobe deferred");
                present = false;
            }
            retry_cooldown_ticks = RETRY_TICKS[retry_level];
            retry_level = (retry_level + 1).min(RETRY_TICKS.len() - 1);
            log::warn!(
                "[nfc] automatic retry deferred for {}s (manual request can retry immediately)",
                retry_cooldown_ticks * 5
            );
        } else {
            retry_cooldown_ticks = 0;
            retry_level = 0;
        }

        #[cfg(feature = "ble-at")]
        let idle_sleep = if dev.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        };
        #[cfg(not(feature = "ble-at"))]
        let idle_sleep = Duration::from_secs(5);
        std::thread::sleep(idle_sleep);
    }
}

// ============================================================================
// 备份/恢复逻辑
// ============================================================================

/// 备份 holding_buf 到 NFC EEPROM
///
/// 对齐参考固件 `isModify=1` 路径:
/// `memcpy(tagWrite, g_tVar.PRegBuf, ADDRESS_MEMORY_END - ADDRESS_NDEFTEXT_END)`
/// 然后分批写入 (RFID_READ_NUMBER=30 字节/次)
fn backup_to_nfc(dev: &St25dv, holding_buf: &[u16], scratch: &mut [u8]) -> Result<(), ()> {
    dev.with_mailbox_temporarily_disabled(|dev| {
        backup_to_nfc_unprotected(dev, holding_buf, scratch)
    })
}

fn backup_to_nfc_unprotected(
    dev: &St25dv,
    holding_buf: &[u16],
    scratch: &mut [u8],
) -> Result<(), ()> {
    // 1. 把 holding_buf 转为字节 (LE, 与 MCA PRegBuf 内存布局一致)
    //    1760B 工作区驻留 PSRAM，不占用 8KB pthread 栈。
    let words = NFC_BLOB_DATA_WORDS.min(holding_buf.len());
    let data_len = words * 2;
    let data = scratch.get_mut(..data_len).ok_or(())?;
    for i in 0..words {
        data[i * 2] = (holding_buf[i] & 0xFF) as u8;
        data[i * 2 + 1] = (holding_buf[i] >> 8) as u8;
    }

    // 2. 计算 CRC32
    let crc = crc32(data);

    // NFC 磨损均衡：CRC 仅保存在 RAM 中，不写入 EEPROM，避免破坏旧格式。
    // 启动读到有效旧快照时也会初始化该值，因此未变化的显式备份不会重复擦写。
    if crc == LAST_NFC_CRC.load(Ordering::Relaxed) {
        log::debug!("[nfc] backup skipped: CRC unchanged (0x{:08X})", crc);
        return Ok(());
    }

    // 分批写原始数据 (RFID_CHUNK=30 字节/次)，地址和长度与原 C++ 完全一致。
    let base = NFC_BLOB_DATA_ADDR;
    for chunk_start in (0..data_len).step_by(RFID_CHUNK) {
        let chunk_end = (chunk_start + RFID_CHUNK).min(data_len);
        let chunk = &data[chunk_start..chunk_end];
        if dev.write_eeprom(base + chunk_start as u16, chunk).is_err() {
            log::error!("[nfc] backup: data write failed at offset {}", chunk_start);
            return Err(());
        }
        TASK_HB.tick();
        health::feed_wdt();
    }

    log::info!(
        "[nfc] backup complete: {} bytes, CRC=0x{:08X}",
        data_len,
        crc
    );
    // LOOP14: 磨损均衡 — 记录本次成功写入的 CRC, 用于下次跳过未变化的写
    LAST_NFC_CRC.store(crc, Ordering::Relaxed);
    Ok(())
}

fn finish_backup(result: Result<(), ()>) {
    if result.is_ok() {
        *NFC_STATE.lock() = NfcState::BackedUp;
    } else {
        crate::bus::storage_state::HOLDING_NFC_DIRTY
            .store(true, std::sync::atomic::Ordering::Release);
        *NFC_STATE.lock() = NfcState::Error;
    }
}

/// 写入与原 C++ 固件相同顺序的 9 条 Type-5 NDEF Text 记录。
/// 手机端按记录序号读取 IP/掩码/网关/BLE ID/位置/型号/版本，因此顺序不可改变。
fn write_ndef_records(dev: &St25dv) -> Result<(), ()> {
    dev.with_mailbox_temporarily_disabled(write_ndef_records_unprotected)
}

fn write_ndef_records_unprotected(dev: &St25dv) -> Result<(), ()> {
    use core::fmt::Write as _;

    let image = crate::bus::config_state::config_read_with(|cs| {
        let cfg = &cs.cfg;
        let mut ip_url = heapless::String::<32>::new();
        let mut mask = heapless::String::<16>::new();
        let mut gateway = heapless::String::<16>::new();
        let mut version = heapless::String::<24>::new();
        write!(
            ip_url,
            "http://{}.{}.{}.{}",
            cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3]
        )
        .map_err(|_| ())?;
        write!(
            mask,
            "{}.{}.{}.{}",
            cfg.mask[0], cfg.mask[1], cfg.mask[2], cfg.mask[3]
        )
        .map_err(|_| ())?;
        write!(
            gateway,
            "{}.{}.{}.{}",
            cfg.gateway[0], cfg.gateway[1], cfg.gateway[2], cfg.gateway[3]
        )
        .map_err(|_| ())?;
        write!(
            version,
            "V{}.{}.{}.{}",
            crate::device::system_config::SystemConfig::decode_fw_version(cfg.fw_version).0,
            crate::device::system_config::SystemConfig::decode_fw_version(cfg.fw_version).1,
            crate::device::system_config::SystemConfig::decode_fw_version(cfg.fw_version).2,
            cfg.fw_date
        )
        .map_err(|_| ())?;

        let ble_end = cfg
            .ble_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(cfg.ble_name.len());
        let name_end = cfg
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(cfg.name.len());
        let records: [(&[u8], &[u8]); 9] = [
            (ip_url.as_bytes(), b"en"),
            (mask.as_bytes(), b"en"),
            (gateway.as_bytes(), b"en"),
            (&cfg.ble_name[..ble_end], b"en"),
            (&cfg.name[..name_end], b"en"),
            (b"Metuory", b"zh"),
            (b"", b"zh"),
            (b"F16", b"en"),
            (version.as_bytes(), b"en"),
        ];

        let mut out = heapless::Vec::<u8, 288>::new();
        out.push(0x03).map_err(|_| ())?; // Type 5 NDEF Message TLV
        out.push(0).map_err(|_| ())?; // 1-byte TLV length, 回填
        for (index, (text, language)) in records.iter().enumerate() {
            append_ndef_text_record(
                &mut out,
                text,
                language,
                index == 0,
                index + 1 == records.len(),
            )?;
        }
        let ndef_len = out.len().checked_sub(2).ok_or(())?;
        if ndef_len > 0xFE {
            return Err(());
        }
        out[1] = ndef_len as u8;
        out.push(0xFE).map_err(|_| ())?; // Terminator TLV
        Ok(out)
    })
    .ok_or(())??;

    // SparkFun writeCCFile8Byte 默认值：E2 40 00 01 00 00 03 FF。
    const CC_FILE: [u8; 8] = [0xE2, 0x40, 0x00, 0x01, 0x00, 0x00, 0x03, 0xFF];
    // 原 MCA 已写入并可能永久锁定 CC 文件。锁定后的正确 CC 无需重写，
    // 否则每次备份都会因地址 0x0000 NACK 而错误中止整个 NFC 同步。
    let mut current_cc = [0u8; CC_FILE.len()];
    let cc_read = dev.read_eeprom(0, &mut current_cc).is_ok();
    // 兼容原 MCA 已写入的扩展 CC：容量字段可能由旧版库写成不同值，
    // 只要 Type-5/访问字段有效且容量非零，就保留，避免触碰可能锁定的 CC。
    let cc_valid = cc_read
        && current_cc[..6] == CC_FILE[..6]
        && u16::from_be_bytes([current_cc[6], current_cc[7]]) != 0;
    if !cc_valid {
        if dev.write_eeprom(0, &CC_FILE).is_err() {
            log::error!("[nfc] NDEF CC unavailable: current={:02X?}", current_cc);
            return Err(());
        }
    } else {
        log::debug!("[nfc] existing CC file is valid, keeping it");
    }
    for start in (0..image.len()).step_by(RFID_CHUNK) {
        let end = (start + RFID_CHUNK).min(image.len());
        if dev
            .write_eeprom(8 + start as u16, &image[start..end])
            .is_err()
        {
            log::error!(
                "[nfc] NDEF payload write/verify failed at EEPROM 0x{:04X}, len={}",
                8 + start as u16,
                end - start
            );
            return Err(());
        }
        TASK_HB.tick();
        health::feed_wdt();
    }
    Ok(())
}

fn append_ndef_text_record<const N: usize>(
    out: &mut heapless::Vec<u8, N>,
    text: &[u8],
    language: &[u8],
    message_begin: bool,
    message_end: bool,
) -> Result<(), ()> {
    if language.len() > 0x3F {
        return Err(());
    }
    let payload_len = 1usize
        .checked_add(language.len())
        .and_then(|n| n.checked_add(text.len()))
        .ok_or(())?;
    if payload_len > u8::MAX as usize {
        return Err(());
    }
    let header = (if message_begin { 0x80 } else { 0 })
        | (if message_end { 0x40 } else { 0 })
        | 0x10 // SR
        | 0x01; // TNF Well Known
    out.push(header).map_err(|_| ())?;
    out.push(1).map_err(|_| ())?; // Type length
    out.push(payload_len as u8).map_err(|_| ())?;
    out.push(b'T').map_err(|_| ())?;
    out.push(language.len() as u8).map_err(|_| ())?;
    out.extend_from_slice(language).map_err(|_| ())?;
    out.extend_from_slice(text).map_err(|_| ())?;
    Ok(())
}

/// 从 NFC EEPROM 恢复 holding_buf
///
/// 对齐参考固件 `isModify=2` 路径:
/// `memcpy((uint16_t *)g_tVar.PRegBuf, (uint16_t *)tagRead, ...)`
/// 然后 `save_config_to_file(...)` 持久化
///
/// restore 后 NFC dirty=false（两端已同步），但 NVS dirty 必须保持 true 直到真正落盘。
fn restore_from_nfc(nfc_words: &[u16]) {
    use crate::bus::backends::storage_modify_holding;

    storage_modify_holding(|buf| {
        let n = nfc_words.len().min(buf.len());
        buf[..n].copy_from_slice(&nfc_words[..n]);
    });

    crate::control_logic::mark_config_changed();

    crate::bus::storage_state::HOLDING_NFC_DIRTY.store(false, std::sync::atomic::Ordering::Release);
    crate::device::request_persist_holding();

    log::info!(
        "[nfc] restore complete: {} words written to holding_buf + NVS queued",
        nfc_words.len()
    );
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 字节数组 → U16 数组 (LE, 与 MCA PRegBuf 内存布局一致)
///
/// LOOP9: 写入调用方提供的栈缓冲 `out`, 避免 pthread 中 `Vec` 堆分配。
/// `out` 至少需容纳 `(data.len() / 2)` 个 u16; 多余字节填 0。
fn bytes_to_words(data: &[u8], out: &mut [u16]) {
    let n = (data.len() / 2).min(out.len());
    for i in 0..n {
        out[i] = u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
    }
    // 剩余清零 (调用方传入固定大小数组时, 未填充部分保持 0)
    for v in &mut out[n..] {
        *v = 0;
    }
}

/// 原 C++ 用快照偏移 50..55 排除全零和 ASCII "000000"；同时排除全 0xFF
/// 的出厂 EEPROM。保持相同判定，避免无元数据旧格式被误恢复。
fn legacy_snapshot_valid(data: &[u8]) -> bool {
    let Some(marker) = data.get(50..56) else {
        return false;
    };
    marker != [0; 6] && marker != [b'0'; 6] && data.iter().any(|&b| b != 0xFF)
}

/// 显式恢复允许原 C++ 的无头原始快照，但拒绝出厂空白数据。
/// 原 C++ 快照用 50..55 的非零标记辅助识别；真实 holding_buf 的这些字节
/// 可能合法地全为零，而且 NFC 需要支持掉电后、跨设备恢复，不能依赖 RAM CRC。
/// 恢复只由认证 POST + 用户确认触发，自动流程仍不会用 NFC 覆盖本机。
fn snapshot_valid_for_restore(data: &[u8]) -> bool {
    legacy_snapshot_valid(data)
        || (data.iter().any(|&b| b != 0x00) && data.iter().any(|&b| b != 0xFF))
}

/// 比较 holding_buf 与 NFC 备份数据是否一致 (仅比较 NFC 覆盖的前 N words)
///
/// LOOP9: 旧实现 `a.len() == b.len()` 恒 false (holding_buf=2048 vs nfc=880),
/// 改为比较前 min(len) 个 word。
fn regbuf_equal(a: &[u16], b: &[u16]) -> bool {
    let n = a.len().min(b.len());
    a[..n].iter().zip(b[..n].iter()).all(|(x, y)| x == y)
}

// ============================================================================
// 公开 API (供 Modbus / HTTP 调用)
// ============================================================================

/// 手动触发备份 (供 Modbus / HTTP 调用)
pub fn backup_now() -> AppResult<()> {
    log::info!("[nfc] manual backup triggered");
    NFC_COMMAND.store(NFC_CMD_BACKUP, Ordering::Release);
    *NFC_STATE.lock() = NfcState::Init;
    Ok(())
}

/// 手动触发恢复 (供 Modbus / HTTP 调用)
pub fn restore_now() -> AppResult<()> {
    log::info!("[nfc] manual restore triggered");
    NFC_COMMAND.store(NFC_CMD_RESTORE, Ordering::Release);
    *NFC_STATE.lock() = NfcState::Init;
    Ok(())
}

// ============================================================================
// 测试
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nfc_blob_constants() {
        assert_eq!(NDEF_TEXT_END, 0x011F);
        assert_eq!(MEMORY_END, 0x07FF);
        assert_eq!(NFC_BLOB_DATA_ADDR, 0x0120);
        assert_eq!(NFC_BLOB_DATA_BYTES, 1760);
        assert_eq!(NFC_BLOB_DATA_WORDS, 880);
        assert_eq!(NFC_BLOB_DATA_ADDR as usize + NFC_BLOB_DATA_BYTES, 0x0800);
    }

    #[test]
    fn test_nfc_state_api_names_are_stable() {
        assert_eq!(NfcState::Idle.as_str(), "Idle");
        assert_eq!(NfcState::Init.as_str(), "Init");
        assert_eq!(NfcState::Detected.as_str(), "Detected");
        assert_eq!(NfcState::BackedUp.as_str(), "BackedUp");
        assert_eq!(NfcState::Restored.as_str(), "Restored");
        assert_eq!(NfcState::Error.as_str(), "Error");
    }

    #[cfg(feature = "ble-at")]
    #[test]
    fn test_mailbox_envelope_round_trip_direct_and_fragmented() {
        let direct = [0x01u8, 0x02, 0x03, 0x04];
        let mut assembler = MailboxAssembler::new();
        assert_eq!(assembler.accept(&direct).unwrap().as_slice(), &direct);

        let payload = [0x5Au8; MAILBOX_CHUNK_MAX + 17];
        let mut fragments = heapless::Vec::<heapless::Vec<u8, MAILBOX_MAX_BYTES>, 4>::new();
        let mut offset = 0;
        let mut seq = 0;
        while offset < payload.len() {
            let mut fragment = heapless::Vec::new();
            let _ = mailbox_fragment(&payload, offset, seq, &mut fragment).unwrap();
            let chunk = fragment.len() - MAILBOX_ENVELOPE_OVERHEAD;
            offset += chunk;
            seq = seq.wrapping_add(1);
            fragments.push(fragment).unwrap();
        }
        let mut rebuilt = None;
        for fragment in fragments {
            rebuilt = assembler.accept(&fragment).or(rebuilt);
        }
        assert_eq!(rebuilt.unwrap().as_slice(), &payload);
    }

    #[cfg(feature = "ble-at")]
    #[test]
    fn test_mailbox_fragments_match_flutter_golden_wire_vector() {
        // This vector is independently asserted by test/nfc_mailbox_codec_test.dart.
        // Keep the bytes and CRC literal here so the Rust decoder is checked
        // against the mobile encoder instead of only against itself.
        let payload = [0x5Au8; MAILBOX_CHUNK_MAX + 1];
        let mut first = heapless::Vec::<u8, MAILBOX_MAX_BYTES>::new();
        first
            .extend_from_slice(&[
                0x49, 0x4D, 0x43, 0x4E, 0x01, 0x01, 0x00, 0x00, 0xF3, 0x00, 0x00, 0xF2,
            ])
            .unwrap();
        first
            .extend_from_slice(&payload[..MAILBOX_CHUNK_MAX])
            .unwrap();
        first.extend_from_slice(&[0xF9, 0x85]).unwrap();

        let last = [
            0x49, 0x4D, 0x43, 0x4E, 0x01, 0x02, 0x01, 0x00, 0xF3, 0x00, 0xF2, 0x01, 0x5A, 0x8D,
            0x6E,
        ];

        let mut encoded_first = heapless::Vec::new();
        assert!(!mailbox_fragment(&payload, 0, 0, &mut encoded_first).unwrap());
        assert_eq!(encoded_first.as_slice(), first.as_slice());
        let mut encoded_last = heapless::Vec::new();
        assert!(mailbox_fragment(&payload, MAILBOX_CHUNK_MAX, 1, &mut encoded_last).unwrap());
        assert_eq!(encoded_last.as_slice(), last);

        let mut assembler = MailboxAssembler::new();
        assert!(assembler.accept(&first).is_none());
        assert_eq!(assembler.accept(&last).unwrap().as_slice(), &payload);
    }

    #[cfg(feature = "ble-at")]
    #[test]
    fn test_mailbox_rejects_corruption_and_recovers_after_out_of_order_data() {
        let payload = [0xA5u8; MAILBOX_CHUNK_MAX + 1];
        let mut first = heapless::Vec::<u8, MAILBOX_MAX_BYTES>::new();
        let mut last = heapless::Vec::<u8, MAILBOX_MAX_BYTES>::new();
        assert!(!mailbox_fragment(&payload, 0, 0, &mut first).unwrap());
        assert!(mailbox_fragment(&payload, MAILBOX_CHUNK_MAX, 1, &mut last).unwrap());

        let mut assembler = MailboxAssembler::new();
        assert!(assembler.accept(first.as_slice()).is_none());
        let mut corrupt = last.as_slice().to_vec();
        *corrupt.last_mut().unwrap() ^= 0x01;
        assert!(assembler.accept(&corrupt).is_none());

        assert!(assembler.accept(first.as_slice()).is_none());
        assert_eq!(
            assembler.accept(last.as_slice()).unwrap().as_slice(),
            &payload
        );

        let mut out_of_order = MailboxAssembler::new();
        assert!(out_of_order.accept(last.as_slice()).is_none());
        assert!(out_of_order.accept(first.as_slice()).is_none());
        assert_eq!(
            out_of_order.accept(last.as_slice()).unwrap().as_slice(),
            &payload
        );
    }

    #[cfg(feature = "ble-at")]
    #[test]
    fn test_mailbox_maximum_frame_round_trip_and_length_rejection() {
        let payload = [0x3Cu8; MAILBOX_REASSEMBLY_MAX];
        let mut assembler = MailboxAssembler::new();
        let mut offset = 0;
        let mut sequence = 0u8;
        let mut rebuilt = None;
        while offset < payload.len() {
            let mut fragment = heapless::Vec::<u8, MAILBOX_MAX_BYTES>::new();
            let last = mailbox_fragment(&payload, offset, sequence, &mut fragment).unwrap();
            let chunk = fragment.len() - MAILBOX_ENVELOPE_OVERHEAD;
            offset += chunk;
            sequence = sequence.wrapping_add(1);
            rebuilt = assembler.accept(fragment.as_slice()).or(rebuilt);
            assert_eq!(last, offset == payload.len());
        }
        assert_eq!(rebuilt.unwrap().as_slice(), &payload);

        let mut output = heapless::Vec::<u8, MAILBOX_MAX_BYTES>::new();
        assert!(mailbox_fragment(&[], 0, 0, &mut output).is_none());
        let oversized = [0u8; MAILBOX_REASSEMBLY_MAX + 1];
        assert!(mailbox_fragment(&oversized, 0, 0, &mut output).is_none());
        assert!(assembler.accept(&oversized).is_none());
    }

    #[cfg(feature = "ble-at")]
    #[test]
    fn test_mailbox_status_helpers_cover_busy_message_and_release_states() {
        assert!(mailbox_is_free(0));
        assert!(!mailbox_is_free(MB_CTRL_HOST_PUT_MSG));
        assert!(!mailbox_is_free(MB_CTRL_RF_PUT_MSG));
        assert!(mailbox_has_rf_message(MB_CTRL_RF_PUT_MSG));
        assert!(!mailbox_has_rf_message(MB_CTRL_HOST_PUT_MSG));
        assert!(!mailbox_response_released(
            Some(MB_CTRL_HOST_PUT_MSG),
            false
        ));
        assert!(mailbox_response_released(Some(0), false));
        assert!(mailbox_response_released(None, true));

        let mut pending = true;
        assert!(!take_deferred_mailbox_reset(&mut pending, false));
        assert!(pending);
        assert!(take_deferred_mailbox_reset(&mut pending, true));
        assert!(!pending);
    }

    #[test]
    fn test_st25_i2c_addr() {
        assert_eq!(ST25DV_SYSTEM_ADDR_CANDIDATES, [0x57, 0x56]);
        assert_eq!(ST25DV_SYSTEM_ADDR_CANDIDATES[0] - 4, 0x53);
        assert_eq!(ST25DV_SYSTEM_ADDR_CANDIDATES[1] - 4, 0x52);
        assert_eq!(st25dv_system_addr_candidates(0x03), [0x1F, 0x1E]);
        assert_eq!(REG_IC_REF, 0x0017);
        assert_eq!(REG_I2C_SSO, 0x2004);
        assert_eq!(ST25DV16KC_IC_REF, 0x51);
        assert!(is_st25dv16kc_identity(0x51, 0xFF, 0x01, 0x03));
        assert!(!is_st25dv16kc_identity(0x26, 0xFF, 0x01, 0x03));
        assert!(!is_st25dv16kc_identity(0x51, 0xFF, 0x00, 0x03));
    }

    #[test]
    fn test_st25dv_system_address_candidates_cover_i2c_cfg_e0() {
        assert_eq!(st25dv_system_addr_candidates(0x0A), [0x57, 0x56]);
        assert_eq!(st25dv_system_addr_candidates(0x03), [0x1F, 0x1E]);
    }

    #[test]
    fn test_st25dv16kc_identity_matches_official_datasheet() {
        assert!(is_st25dv16kc_identity(0x51, 0xFF, 0x01, 0x03));
        assert!(!is_st25dv16kc_identity(0x26, 0xFF, 0x01, 0x03));
        assert!(!is_st25dv16kc_identity(0x51, 0x00, 0x01, 0x03));
        assert!(!is_st25dv16kc_identity(0x51, 0xFF, 0x01, 0x04));
    }

    #[test]
    fn test_i2c_password_presentation_matches_st25dv_wire_order() {
        assert_eq!(
            i2c_password_presentation(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]),
            [
                0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01, 0x09, 0xEF, 0xCD, 0xAB, 0x89, 0x67,
                0x45, 0x23, 0x01,
            ]
        );
    }

    #[test]
    fn test_ftm_enable_disables_mailbox_watchdog_and_preserves_reserved_bits() {
        let current = 0b1011_1110;
        assert_eq!(
            ftm_config_value(current, true, St25dvGeneration::Kc),
            0b1011_0001,
            "enabling FTM clears MB_WDG and sets MB_MODE while preserving reserved bits"
        );
        assert_eq!(
            ftm_config_value(current, false, St25dvGeneration::Kc),
            0b1011_0000,
            "disabling FTM also clears MB_WDG while preserving reserved bits"
        );
    }

    #[test]
    fn test_direct_mailbox_request_cancels_interrupted_fragment_assembly() {
        let data = [0x55; MAILBOX_CHUNK_MAX + 1];
        let mut message = heapless::Vec::new();
        let mut assembler = MailboxAssembler::new();
        mailbox_fragment(&data, 0, 0, &mut message).unwrap();
        assert!(assembler.accept(&message).is_none());
        assert_eq!(assembler.accept(&[1, 2, 3]).unwrap().as_slice(), &[1, 2, 3]);
        mailbox_fragment(&data, MAILBOX_CHUNK_MAX, 1, &mut message).unwrap();
        assert!(
            assembler.accept(&message).is_none(),
            "old fragments cannot resume after a new direct request"
        );
    }

    #[test]
    fn test_st25dv16k_generation_rejects_unknown_or_wrong_capacity() {
        assert_eq!(
            st25dv16k_generation(0x26, 0xFF, 0x01, 0x03),
            Some(St25dvGeneration::K)
        );
        assert_eq!(
            st25dv16k_generation(0x51, 0xFF, 0x01, 0x03),
            Some(St25dvGeneration::Kc)
        );
        for reference in [0x00, 0x24, 0x50, 0xFF] {
            assert_eq!(st25dv16k_generation(reference, 0xFF, 0x01, 0x03), None);
        }
        for reference in [0x26, 0x51] {
            for capacity in [(0x7F, 0x00), (0xFF, 0x07), (0x00, 0x01)] {
                assert_eq!(
                    st25dv16k_generation(reference, capacity.0, capacity.1, 0x03),
                    None
                );
            }
            assert_eq!(st25dv16k_generation(reference, 0xFF, 0x01, 0x04), None);
        }
    }

    #[test]
    fn test_k_mailbox_mode_preserves_separate_watchdog_register_layout() {
        for current in 0..=u8::MAX {
            assert_eq!(
                ftm_config_value(current, true, St25dvGeneration::K),
                current | 1
            );
            assert_eq!(
                ftm_config_value(current, false, St25dvGeneration::K),
                current & !1
            );
            assert_eq!(
                ftm_config_value(current, true, St25dvGeneration::Kc) & 0x0F,
                1
            );
            assert_eq!(
                ftm_config_value(current, false, St25dvGeneration::Kc) & 0x0F,
                0
            );
        }
    }

    #[test]
    fn test_rfid_chunk() {
        assert_eq!(RFID_CHUNK, 30);
    }

    #[test]
    fn test_bytes_to_words() {
        let data = [0x01, 0x02, 0x03, 0x04];
        let mut words = [0u16; 2];
        bytes_to_words(&data, &mut words);
        assert_eq!(words[0], 0x0201);
        assert_eq!(words[1], 0x0403);
    }

    #[test]
    fn test_crc32() {
        // 已知 CRC32 值
        let data = b"123456789";
        assert_eq!(crc32(data), 0xCBF43926);
    }

    #[test]
    fn test_legacy_snapshot_validation_matches_mca_marker() {
        let mut data = [0u8; 64];
        assert!(!legacy_snapshot_valid(&data));
        data[50..56].copy_from_slice(b"000000");
        assert!(!legacy_snapshot_valid(&data));
        data[50..56].copy_from_slice(b"ABC123");
        assert!(legacy_snapshot_valid(&data));
        data.fill(0xFF);
        assert!(!legacy_snapshot_valid(&data));
    }

    #[test]
    fn test_restore_snapshot_rejects_only_blank_eeprom() {
        assert!(!snapshot_valid_for_restore(&[0u8; 64]));
        assert!(!snapshot_valid_for_restore(&[0xFFu8; 64]));
        let mut data = [0u8; 64];
        data[0] = 1;
        assert!(snapshot_valid_for_restore(&data));
    }

    #[test]
    fn test_ndef_text_record_wire_format() {
        let mut out = heapless::Vec::<u8, 32>::new();
        append_ndef_text_record(&mut out, b"F16", b"en", true, true).unwrap();
        assert_eq!(
            out.as_slice(),
            &[0xD1, 0x01, 0x06, b'T', 0x02, b'e', b'n', b'F', b'1', b'6']
        );
    }

    #[test]
    fn test_ndef_text_record_rejects_oversized_language() {
        let mut out = heapless::Vec::<u8, 128>::new();
        let language = [b'x'; 64];
        assert!(append_ndef_text_record(&mut out, b"F16", &language, true, true).is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn test_regbuf_equal() {
        // LOOP9: regbuf_equal 比较前 min(len) 个 word (不再要求长度完全一致)
        assert!(regbuf_equal(&[1, 2, 3], &[1, 2, 3]));
        assert!(!regbuf_equal(&[1, 2, 3], &[1, 2, 4]));
        // 短数组是长数组前缀 → 相等 (NFC backing up holding_buf[0..2048], regbuf is holding_buf)
        assert!(regbuf_equal(&[1, 2], &[1, 2, 3]));
        // 完全不同长度
        assert!(!regbuf_equal(&[1, 2, 3], &[9, 2, 3]));
        assert!(!regbuf_equal(&[], &[1, 2, 3]));
        // 空数组 vs 空数组
        assert!(regbuf_equal(&[], &[]));
    }
}
