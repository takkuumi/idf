//! NFC ST25DV64KC 配置备份/恢复模块
//!
//! 移植自参考固件 MCA_F16V2_1_F48_BLE.ino 的 `RFID_Init` 函数.
//!
//! ## 硬件
//!
//! ST25DV64KC 是 ST 的 NFC Type 5 标签 + I2C 从设备双接口芯片.
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
//! 2. **二进制配置快照** (从 0x0120 开始，固定 4096 字节):
//!    - `isModify=1` (backup): holding_buf → NFC EEPROM (实际写 4096B)
//!    - `isModify=2` (restore): NFC EEPROM → holding_buf
//!
//! ## 启动时机
//!
//! 对齐参考固件: `RFID_Init(0)` 在 `setup()` 中、`nca9555_init()` 之前调用.
//! 本模块在 `main()` 中启动后台任务，并与 PCA9555 通过物理总线仲裁器共享 I2C。
//! 避免 I2C 总线冲突 (NFC 和 PCA9555 共用 LED 总线引脚 38/37).
//!
//! 后台线程每 5 秒检测标签是否在场, 一旦检测到即执行一次同步.
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
/// ST25DV64KC 1M 写循环 ÷ 5s 轮询 × 24h = 17280 写/天 → ~58 天寿命.
/// 加 CRC 去重: 数据无变化时跳过写, 典型场景 (重启后无配置变更) 寿命延长至 10+ 年.
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

static NFC_STATE: LazyLock<Spin<NfcState>> = LazyLock::new(|| Spin::new(NfcState::Idle));

/// 读取当前 NFC 状态
pub fn state() -> NfcState {
    *NFC_STATE.lock()
}

pub fn is_started() -> bool {
    STARTED.load(Ordering::Acquire)
}

// ============================================================================
// ST25DV64KC I2C 协议常量
// ============================================================================

/// ST25DV64KC 有两个 I2C 地址：用户 EEPROM/动态寄存器 0x53，系统寄存器 0x57。
const ST25DV_DATA_ADDR_7BIT: u8 = 0x53;
const ST25DV_SYSTEM_ADDR_7BIT: u8 = 0x57;

/// IC_REF 位于系统地址空间 0x0017（SparkFun 官方驱动常量）。
const REG_IC_REF: u16 = 0x0017;
const REG_ENDA1: u16 = 0x0005;
/// I2C 密码寄存器 (datasheet §3.3.2, 地址 0x0900, 8 字节)
const REG_I2C_PASSWD: u16 = 0x0900;
/// I2C 安全会话状态位于数据地址的动态寄存器 0x2004。
/// bit 0: I2C 安全会话开启标志
const REG_I2C_SSO: u16 = 0x2004;

/// 用户内存 NDEF 区结束地址 (Area 1)
const NDEF_TEXT_END: u16 = 0x011F;
/// 用户内存结束地址 (ST25DV64KC: 0x1FFF, 8KB 全用户区)
///
/// 原 MCA 遗留值 0x07FF 仅覆盖 2KB；ST25DV64KC 用户区实际到 0x1FFF。
/// 快照固定为 holding_buf 的 2048 words，剩余空间留给提交头和扩展。
/// 写入耗时: 4096B / 30B-chunk / 6ms ≈ 0.82s, 在 5s 轮询窗口内.
const MEMORY_END: u16 = 0x1FFF;
/// 每次 I2C 读写最大字节数 (对齐参考固件 RFID_READ_NUMBER=30)
const RFID_CHUNK: usize = 30;

/// 默认 I2C 密码 (8 字节零, datasheet 出厂默认)
const DEFAULT_PASSWORD: [u8; 8] = [0; 8];

/// NFC 备份数据 magic ("NFC Data" 标识)
const NFC_BLOB_MAGIC: u16 = 0xDEED;
/// NFC 备份数据 version
const NFC_BLOB_VERSION: u16 = 2;
/// 原 C++ 固件从 0x0120 起直接存放 PRegBuf。保持原始数据起点和 LE word 布局，
/// 使旧固件/旧工装仍可读取前 0x6E0 字节；CRC 元数据放在完整 4096B 数据之后。
const NFC_BLOB_DATA_ADDR: u16 = NDEF_TEXT_END + 1;
const NFC_BLOB_DATA_BYTES: usize = 4096;
const NFC_BLOB_HEADER_ADDR: u16 = NFC_BLOB_DATA_ADDR + NFC_BLOB_DATA_BYTES as u16;
/// NFC 备份头部大小: magic(2) + version(2) + crc32(4) = 8 字节
const NFC_HEADER_BYTES: usize = 8;

// ============================================================================
// ST25DV64KC I2C 驱动
// ============================================================================

/// ST25DV64KC I2C 驱动 (封装 SwI2c)
struct St25dv {
    i2c: SwI2c,
}

impl St25dv {
    /// 尝试在指定引脚上初始化 ST25DV64KC
    ///
    /// 返回 `Some(St25dv)` 表示标签在线, `None` 表示无响应.
    fn try_init(sda: i32, scl: i32) -> Option<Self> {
        let i2c = SwI2c::init(sda, scl).ok()?;
        let dev = Self { i2c };
        // 读 IC_REF 寄存器验证标签在线
        match dev.read_reg16_at(ST25DV_SYSTEM_ADDR_7BIT, REG_IC_REF) {
            Ok(ic_ref) => {
                log::info!(
                    "[nfc] ST25DV detected on SDA={} SCL={} (IC_REF=0x{:02X})",
                    sda,
                    scl,
                    ic_ref
                );
                Some(dev)
            }
            Err(_) => None,
        }
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
                    std::thread::sleep(Duration::from_millis(6));
                }
                return Ok(());
            }
            if attempt != 5 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
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
        // 密码展示格式必须是 password(MSB first) + 0x09 + password(MSB first)，
        // 共 17 字节，并写到 SYSTEM 地址。旧实现只向 DATA 地址写 8 字节，
        // 安全会话实际上从未打开。
        let mut presentation = [0u8; 17];
        for i in 0..8 {
            presentation[i] = DEFAULT_PASSWORD[7 - i];
            presentation[i + 9] = DEFAULT_PASSWORD[7 - i];
        }
        presentation[8] = 0x09;
        self.write_bytes_at(
            ST25DV_SYSTEM_ADDR_7BIT,
            REG_I2C_PASSWD,
            &presentation,
            false,
        )?;
        // 验证会话已开启 (读 I2C_SSO bit 0)
        match self.read_reg16_at(ST25DV_DATA_ADDR_7BIT, REG_I2C_SSO) {
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
    }

    /// 关闭 I2C 安全会话 (写错误密码, 对齐参考固件)
    fn close_i2c_session(&self) {
        let mut wrong = DEFAULT_PASSWORD;
        wrong[0] = 0x10;
        let mut presentation = [0u8; 17];
        for i in 0..8 {
            presentation[i] = wrong[7 - i];
            presentation[i + 9] = wrong[7 - i];
        }
        presentation[8] = 0x09;
        let _ = self.write_bytes_at(
            ST25DV_SYSTEM_ADDR_7BIT,
            REG_I2C_PASSWD,
            &presentation,
            false,
        );
        log::debug!("[nfc] I2C security session closed");
    }

    fn ensure_ndef_area(&self) -> Result<(), ()> {
        // Area 1 end = value * 32 + 31；0x08 对应 0x011F，与原固件一致。
        let current = self.read_reg16_at(ST25DV_SYSTEM_ADDR_7BIT, REG_ENDA1)?;
        if current != 0x08 {
            self.write_bytes_at(ST25DV_SYSTEM_ADDR_7BIT, REG_ENDA1, &[0x08], true)?;
            let verified = self.read_reg16_at(ST25DV_SYSTEM_ADDR_7BIT, REG_ENDA1)?;
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
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(ST25DV_DATA_ADDR_7BIT << 1) {
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
        if !i2c.write_byte((ST25DV_DATA_ADDR_7BIT << 1) | 1) {
            i2c.stop();
            return Err(());
        }
        let n = buf.len();
        for i in 0..n {
            let ack = i < n - 1; // ACK all but last byte
            buf[i] = i2c.read_byte(ack);
        }
        i2c.stop();
        Ok(())
    }

    /// 向用户 EEPROM 写入数据 (16-bit 地址, 多字节)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, data[0..n], STOP
    /// 注意: ST25DV64KC 内部写周期 ~5ms。每块写后读回比较，静默损坏不会提交 CRC 头。
    fn write_eeprom(&self, addr: u16, data: &[u8]) -> Result<(), ()> {
        if data.is_empty() || data.len() > RFID_CHUNK {
            return Err(());
        }
        let mut verify = [0u8; RFID_CHUNK];
        for _ in 0..3 {
            if self
                .write_bytes_at(ST25DV_DATA_ADDR_7BIT, addr, data, true)
                .is_ok()
                && self
                    .read_eeprom_chunk(addr, &mut verify[..data.len()])
                    .is_ok()
                && verify[..data.len()] == *data
            {
                return Ok(());
            }
        }
        Err(())
    }
}

// ============================================================================
// NFC 数据格式
// ============================================================================

/// NFC 备份头部 (8 字节, 存储在完整原始 PRegBuf 数据之后)
///
/// 布局: [magic:2 LE][version:2 LE][crc32:4 LE]
/// CRC32 覆盖后续 NFC_BLOB_DATA_BYTES 字节的 holding_buf 数据
#[derive(Clone, Copy)]
struct NfcHeader {
    magic: u16,
    version: u16,
    crc32: u32,
}

impl NfcHeader {
    fn is_valid(&self) -> bool {
        self.magic == NFC_BLOB_MAGIC && self.version == NFC_BLOB_VERSION
    }

    fn to_bytes(&self) -> [u8; NFC_HEADER_BYTES] {
        let mut b = [0u8; NFC_HEADER_BYTES];
        b[0..2].copy_from_slice(&self.magic.to_le_bytes());
        b[2..4].copy_from_slice(&self.version.to_le_bytes());
        b[4..8].copy_from_slice(&self.crc32.to_le_bytes());
        b
    }

    fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < NFC_HEADER_BYTES {
            return None;
        }
        Some(Self {
            magic: u16::from_le_bytes([b[0], b[1]]),
            version: u16::from_le_bytes([b[2], b[3]]),
            crc32: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        })
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

/// 固定驻留 PSRAM 的零初始化工作区，避免两个 4KB 缓冲占用 pthread 所需 internal SRAM。
struct PsramBuffer<T> {
    ptr: NonNull<T>,
    len: usize,
}

impl<T> PsramBuffer<T> {
    fn zeroed(len: usize) -> Option<Self> {
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

/// 启动 NFC 备份/恢复模块
///
/// 对齐参考固件: `RFID_Init(0)` 在 `setup()` 中、`nca9555_init()` 之前调用.
/// 本函数在 `main()` 中、PCA9555 初始化之前调用, 避免 I2C 总线冲突.
///
/// 启动后台线程, 每 5 秒检测标签是否在场, 一旦检测到即执行一次同步.
pub fn start() -> AppResult<()> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let data_buf = match PsramBuffer::<u8>::zeroed(NFC_BLOB_DATA_BYTES) {
        Some(buffer) => buffer,
        None => {
            STARTED.store(false, Ordering::Release);
            return Err(crate::error::AppError::Sys(
                "allocate NFC data workspace in PSRAM".into(),
            ));
        }
    };
    let nfc_words = match PsramBuffer::<u16>::zeroed(NFC_BLOB_DATA_WORDS) {
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
    log::info!("[nfc] background thread spawned (5s polling)");
    Ok(())
}

/// NFC 后台轮询循环
fn nfc_loop(mut data_buf: PsramBuffer<u8>, mut nfc_words: PsramBuffer<u16>) {
    let mut present = false;
    // 两个 4KB 工作区由 start() 显式分配到 PSRAM 并移交本任务，循环内持续复用。
    // LOOP9: 订阅硬件 WDT, 否则 feed_wdt() 高频报 "task not found" 刷屏
    health::subscribe_wdt();

    loop {
        TASK_HB.tick();
        health::feed_wdt();

        *NFC_STATE.lock() = NfcState::Init;

        // 1. 尝试初始化 ST25DV64KC (先 LED 总线 38/37, 再 IO 总线 35/36)
        //    对齐参考固件: Wire.setPins(38,37) → Wire.setPins(35,36)
        let dev = St25dv::try_init(
            crate::config::pins::NCA9555_LED_SDA as i32,
            crate::config::pins::NCA9555_LED_SCL as i32,
        )
        .or_else(|| {
            St25dv::try_init(
                crate::config::pins::NCA9555_IIC_SDA as i32,
                crate::config::pins::NCA9555_IIC_SCL as i32,
            )
        });

        let dev = match dev {
            Some(d) => d,
            None => {
                if present {
                    log::info!("[nfc] tag removed");
                    present = false;
                }
                *NFC_STATE.lock() = NfcState::Idle;
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        if !present {
            log::info!("[nfc] tag detected");
            present = true;
        }
        *NFC_STATE.lock() = NfcState::Detected;

        // 2. 打开 I2C 安全会话
        if dev.open_i2c_session().is_err() {
            log::warn!("[nfc] I2C session open failed");
            *NFC_STATE.lock() = NfcState::Error;
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        if dev.ensure_ndef_area().is_err() {
            log::warn!("[nfc] Area 1 configuration failed");
            *NFC_STATE.lock() = NfcState::Error;
            dev.close_i2c_session();
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }

        // 3. 读 EEPROM 头部, 判断是否需要备份/恢复
        let mut hdr_buf = [0u8; NFC_HEADER_BYTES];
        let header = match dev.read_eeprom(NFC_BLOB_HEADER_ADDR, &mut hdr_buf) {
            Ok(_) => NfcHeader::from_bytes(&hdr_buf),
            Err(_) => None,
        };

        // LOOP9: 避免 clone 整个 holding_buf (~4KB heap), 持 Arc 引用即可
        let regbuf_arc = storage_read();
        let regbuf: Option<&[u16]> = regbuf_arc.as_ref().map(|s| s.holding_buf.as_ref());
        let command = NFC_COMMAND.swap(NFC_CMD_AUTO, Ordering::AcqRel);

        match (header, regbuf) {
            (Some(h), Some(rb)) if h.is_valid() => {
                // 校验 CRC
                // 固定读取 4096B 快照；大缓冲放堆上，避免占用 8KB NFC 任务栈。
                let max_bytes = NFC_BLOB_DATA_BYTES.min(rb.len() * 2);
                match dev.read_eeprom(NFC_BLOB_DATA_ADDR, &mut data_buf[..max_bytes]) {
                    Ok(_) => {
                        let calc_crc = crc32(&data_buf[..max_bytes]);
                        if calc_crc == h.crc32 {
                            let n_words = max_bytes / 2;
                            bytes_to_words(&data_buf[..max_bytes], &mut nfc_words[..n_words]);
                            if command == NFC_CMD_BACKUP {
                                log::info!("[nfc] manual backup executing");
                                finish_backup(backup_to_nfc(&dev, rb, &mut data_buf));
                            } else if command == NFC_CMD_RESTORE {
                                log::info!("[nfc] manual restore executing");
                                restore_from_nfc(&nfc_words[..n_words]);
                                *NFC_STATE.lock() = NfcState::Restored;
                            } else if regbuf_equal(rb, &nfc_words[..n_words]) {
                                log::debug!("[nfc] snapshot matches, no sync needed");
                            } else {
                                // LOOP13: NFC 数据有效但与本地不一致 → 用本地 dirty 决策
                                // - HOLDING_DIRTY=true  → 本地有 Modbus/AT 写入，本地更新，备份到 NFC
                                // - HOLDING_DIRTY=false → 本地已同步上次 NFC backup，NFC 数据更新，从 NFC 恢复
                                let local_dirty = crate::bus::storage_state::HOLDING_DIRTY
                                    .load(std::sync::atomic::Ordering::Acquire);
                                if local_dirty {
                                    log::info!(
                                        "[nfc] local dirty + NFC differs, backing up local to NFC"
                                    );
                                    finish_backup(backup_to_nfc(&dev, rb, &mut data_buf));
                                } else {
                                    log::info!(
                                        "[nfc] local clean + NFC differs, restoring from NFC"
                                    );
                                    restore_from_nfc(&nfc_words[..n_words]);
                                    *NFC_STATE.lock() = NfcState::Restored;
                                }
                            }
                        } else {
                            if command == NFC_CMD_RESTORE {
                                log::error!("[nfc] manual restore rejected: CRC mismatch");
                                *NFC_STATE.lock() = NfcState::Error;
                            } else {
                                // CRC 不匹配 → 备份
                                log::info!("[nfc] NFC CRC mismatch, backing up");
                                finish_backup(backup_to_nfc(&dev, rb, &mut data_buf));
                            }
                        }
                    }
                    Err(_) => {
                        if command == NFC_CMD_RESTORE {
                            log::error!("[nfc] manual restore rejected: EEPROM read failed");
                            *NFC_STATE.lock() = NfcState::Error;
                        } else {
                            log::warn!("[nfc] EEPROM read failed, backing up");
                            finish_backup(backup_to_nfc(&dev, rb, &mut data_buf));
                        }
                    }
                }
            }
            (_, Some(_)) if command == NFC_CMD_RESTORE => {
                // 兼容原 C++ 标签：旧格式没有 CRC 头，但从 0x0120 开始就是原始
                // PRegBuf。只有用户显式请求恢复时才接受无头快照，自动流程不会用
                // 未校验数据覆盖本机。
                match dev.read_eeprom(NFC_BLOB_DATA_ADDR, &mut data_buf) {
                    Ok(_) if data_buf.iter().any(|&b| b != 0 && b != 0xFF) => {
                        bytes_to_words(&data_buf, &mut nfc_words);
                        restore_from_nfc(&nfc_words);
                        *NFC_STATE.lock() = NfcState::Restored;
                        log::warn!("[nfc] restored explicit legacy raw snapshot (no CRC metadata)");
                    }
                    _ => {
                        log::error!("[nfc] manual restore rejected: snapshot empty/unreadable");
                        *NFC_STATE.lock() = NfcState::Error;
                    }
                }
            }
            (_, Some(rb)) => {
                // NFC 数据无效/空 → 备份
                log::info!("[nfc] NFC empty or invalid, backing up current config");
                finish_backup(backup_to_nfc(&dev, rb, &mut data_buf));
            }
            _ => {
                *NFC_STATE.lock() = NfcState::Error;
            }
        }

        // 4. 关闭 I2C 会话
        dev.close_i2c_session();

        // 5. 等待下次轮询
        std::thread::sleep(Duration::from_secs(5));
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
    if let Err(()) = write_ndef_records(dev) {
        log::error!("[nfc] backup: NDEF record update failed");
        return Err(());
    }
    // 1. 把 holding_buf 转为字节 (LE, 与 MCA PRegBuf 内存布局一致)
    //    4096B 不放 8KB 任务栈，使用有界堆缓冲。
    let words = NFC_BLOB_DATA_WORDS.min(holding_buf.len());
    let data_len = words * 2;
    let data = scratch.get_mut(..data_len).ok_or(())?;
    for i in 0..words {
        data[i * 2] = (holding_buf[i] & 0xFF) as u8;
        data[i * 2 + 1] = (holding_buf[i] >> 8) as u8;
    }

    // 2. 计算 CRC32
    let crc = crc32(&data);

    // LOOP14: NFC 磨损均衡 — CRC 去重
    // 若本次 holding_buf 的 CRC 与上次成功写入 NFC 的 CRC 相同, 说明内容未变,
    // 直接跳过 4KB EEPROM 写入. ST25DV64KC 1M 写循环寿命从 ~58 天延长至 >10 年.
    if crc == LAST_NFC_CRC.load(Ordering::Relaxed) {
        log::debug!("[nfc] backup skipped: CRC unchanged (0x{:08X})", crc);
        return Ok(());
    }

    // 3. 先写原始数据，最后写 CRC 头作为原子提交标记。掉电时旧头与新数据
    // CRC 不匹配，下次会安全重写，不会把半包当成有效快照。
    let hdr = NfcHeader {
        magic: NFC_BLOB_MAGIC,
        version: NFC_BLOB_VERSION,
        crc32: crc,
    };
    let hdr_bytes = hdr.to_bytes();

    // 4. 分批写数据 (RFID_CHUNK=30 字节/次, 对齐参考固件)
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

    if dev.write_eeprom(NFC_BLOB_HEADER_ADDR, &hdr_bytes).is_err() {
        log::error!("[nfc] backup: commit header write failed");
        return Err(());
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
        crate::bus::storage_state::HOLDING_DIRTY.store(false, std::sync::atomic::Ordering::Release);
        *NFC_STATE.lock() = NfcState::BackedUp;
    } else {
        *NFC_STATE.lock() = NfcState::Error;
    }
}

/// 写入与原 C++ 固件相同顺序的 9 条 Type-5 NDEF Text 记录。
/// 手机端按记录序号读取 IP/掩码/网关/BLE ID/位置/型号/版本，因此顺序不可改变。
fn write_ndef_records(dev: &St25dv) -> Result<(), ()> {
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
            cfg.fw_version / 100,
            (cfg.fw_version % 100) / 10,
            cfg.fw_version % 10,
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
    dev.write_eeprom(0, &CC_FILE)?;
    for start in (0..image.len()).step_by(RFID_CHUNK) {
        let end = (start + RFID_CHUNK).min(image.len());
        dev.write_eeprom(8 + start as u16, &image[start..end])?;
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
/// LOOP13: restore 完成后 HOLDING_DIRTY=false (本地 = NFC 快照, 已同步).
/// 同时触发 holding NVS 持久化 (而非仅 device_text), 对齐 holding_store 路径.
fn restore_from_nfc(nfc_words: &[u16]) {
    use crate::bus::backends::storage_modify_holding;

    // storage_modify_holding 内部置 HOLDING_DIRTY=true, 但 restore 后
    // 本地 holding_buf = NFC 数据, 状态已同步, 需立即清 dirty.
    storage_modify_holding(|buf| {
        let n = nfc_words.len().min(buf.len());
        buf[..n].copy_from_slice(&nfc_words[..n]);
    });

    // LOOP13: restore 后立即清 dirty (与 NFC 同步, 不会再被 NFC 视为"本地更新")
    // 同时触发 NVS holding 持久化, 确保恢复的数据也落盘.
    crate::bus::storage_state::HOLDING_DIRTY.store(false, std::sync::atomic::Ordering::Release);
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

/// 比较 holding_buf 与 NFC 备份数据是否一致 (仅比较 NFC 覆盖的前 N words)
///
/// LOOP9: 旧实现 `a.len() == b.len()` 恒 false (holding_buf=2048 vs nfc=880),
/// 改为比较前 min(len) 个 word。
/// LOOP12: NFC 扩容后 holding_buf=2048 == nfc 备份前 2048 words, 行为不变但不再需要保留长度差异.
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
        assert_eq!(NFC_BLOB_MAGIC, 0xDEED);
        assert_eq!(NFC_BLOB_VERSION, 2);
        assert_eq!(NDEF_TEXT_END, 0x011F);
        assert_eq!(MEMORY_END, 0x1FFF);
        assert_eq!(NFC_BLOB_DATA_ADDR, 0x0120);
        assert_eq!(NFC_BLOB_DATA_BYTES, 4096);
        assert_eq!(NFC_BLOB_DATA_WORDS, 2048);
        assert_eq!(NFC_BLOB_HEADER_ADDR, 0x1120);
        assert_eq!(NFC_HEADER_BYTES, 8);
    }

    #[test]
    fn test_st25_i2c_addr() {
        assert_eq!(ST25DV_DATA_ADDR_7BIT, 0x53);
        assert_eq!(ST25DV_SYSTEM_ADDR_7BIT, 0x57);
        assert_eq!(REG_IC_REF, 0x0017);
        assert_eq!(REG_I2C_SSO, 0x2004);
    }

    #[test]
    fn test_rfid_chunk() {
        assert_eq!(RFID_CHUNK, 30);
    }

    #[test]
    fn test_nfc_header_roundtrip() {
        let hdr = NfcHeader {
            magic: NFC_BLOB_MAGIC,
            version: NFC_BLOB_VERSION,
            crc32: 0x12345678,
        };
        let bytes = hdr.to_bytes();
        let restored = NfcHeader::from_bytes(&bytes).unwrap();
        assert_eq!(restored.magic, NFC_BLOB_MAGIC);
        assert_eq!(restored.version, NFC_BLOB_VERSION);
        assert_eq!(restored.crc32, 0x12345678);
        assert!(restored.is_valid());
    }

    #[test]
    fn test_nfc_header_invalid() {
        let hdr = NfcHeader {
            magic: 0x0000,
            version: 0,
            crc32: 0,
        };
        assert!(!hdr.is_valid());
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
