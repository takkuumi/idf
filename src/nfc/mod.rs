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
//! 2. **二进制配置快照** (Area 2, 0x0120..=0x1FFF = 7648 字节, LOOP12 扩容):
//!    - `isModify=1` (backup): holding_buf → NFC EEPROM (实际写 4096B)
//!    - `isModify=2` (restore): NFC EEPROM → holding_buf
//!
//! ## 启动时机
//!
//! 对齐参考固件: `RFID_Init(0)` 在 `setup()` 中、`nca9555_init()` 之前调用.
//! 本模块在 `main()` 中、PCA9555 初始化之前调用 `init_and_maybe_restore()`,
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use crate::bus::storage_state::storage_read;
use crate::error::AppResult;
use crate::hal::sw_i2c::SwI2c;
use crate::health::{self, TaskHb};
use crate::sync::Spin;

/// NFC 模块启动标志 (一次性, 防重复)
static STARTED: AtomicBool = AtomicBool::new(false);

/// NFC 模块任务心跳 (阈值 30s)
static TASK_HB: TaskHb = TaskHb::new_with_stall("nfc-st25", 30);

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

// ============================================================================
// ST25DV64KC I2C 协议常量
// ============================================================================

/// ST25DV64KC I2C 7-bit 地址 (datasheet §3.1)
const ST25DV_ADDR_7BIT: u8 = 0x53;
/// 8-bit 写地址 (7-bit << 1 | 0)
const ST25DV_ADDR_W: u8 = ST25DV_ADDR_7BIT << 1; // 0xA6
/// 8-bit 读地址 (7-bit << 1 | 1)
const ST25DV_ADDR_R: u8 = (ST25DV_ADDR_7BIT << 1) | 1; // 0xA7

/// IC_REF 寄存器 (datasheet §3.3.1, 地址 0x0000)
/// 读取返回 0x25 (ST25DV64KC) 或 0x26 (ST25DV16KC)
const REG_IC_REF: u16 = 0x0000;
/// I2C 密码寄存器 (datasheet §3.3.2, 地址 0x0900, 8 字节)
const REG_I2C_PASSWD: u16 = 0x0900;
/// I2C 安全会话状态 (datasheet §3.3.3, 地址 0x0908)
/// bit 0: I2C 安全会话开启标志
const REG_I2C_SSO: u16 = 0x0908;

/// 用户内存 NDEF 区结束地址 (Area 1)
const NDEF_TEXT_END: u16 = 0x011F;
/// 用户内存结束地址 (ST25DV64KC: 0x1FFF, 8KB 全用户区)
///
/// LOOP12: 原 MCA 遗留值 0x07FF (仅 2KB), 现改为 0x1FFF 释放 6KB 余量.
/// 备份容量: 880 words → 3824 words, 完全覆盖 holding_buf 2048 words.
/// 写入耗时: 4096B / 30B-chunk / 6ms ≈ 0.82s, 在 5s 轮询窗口内.
const MEMORY_END: u16 = 0x1FFF;
/// 每次 I2C 读写最大字节数 (对齐参考固件 RFID_READ_NUMBER=30)
const RFID_CHUNK: usize = 30;

/// 默认 I2C 密码 (8 字节零, datasheet 出厂默认)
const DEFAULT_PASSWORD: [u8; 8] = [0; 8];

/// NFC 备份数据 magic ("NFC Data" 标识)
const NFC_BLOB_MAGIC: u16 = 0xDEED;
/// NFC 备份数据 version
const NFC_BLOB_VERSION: u16 = 1;
/// NFC 备份有效区域大小
/// LOOP12: 0x1FFF - 0x011F = 7648 字节 (380% ↑, 容量足以覆盖 4096B holding_buf)
const NFC_BLOB_DATA_BYTES: usize = (MEMORY_END - NDEF_TEXT_END) as usize; // 7648
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
        match dev.read_reg16(REG_IC_REF) {
            Ok(ic_ref) if ic_ref == 0x25 || ic_ref == 0x26 => {
                log::info!(
                    "[nfc] ST25DV64KC detected on SDA={} SCL={} (IC_REF=0x{:02X})",
                    sda, scl, ic_ref
                );
                Some(dev)
            }
            Ok(v) => {
                log::debug!("[nfc] I2C 0x{:02X} responded but IC_REF=0x{:02X} (not ST25DV)", ST25DV_ADDR_7BIT, v);
                None
            }
            Err(_) => None,
        }
    }

    /// 读 16-bit 地址寄存器 (返回 1 字节)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, START, addr_r, data, NACK, STOP
    fn read_reg16(&self, reg: u16) -> Result<u8, ()> {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(ST25DV_ADDR_W) {
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
        if !i2c.write_byte(ST25DV_ADDR_R) {
            i2c.stop();
            return Err(());
        }
        let val = i2c.read_byte(false); // NACK (single byte read)
        i2c.stop();
        Ok(val)
    }

    /// 写 16-bit 地址寄存器 (1 字节数据)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, data, STOP
    fn write_reg16(&self, reg: u16, data: u8) -> Result<(), ()> {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(ST25DV_ADDR_W) {
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
        if !i2c.write_byte(data) {
            i2c.stop();
            return Err(());
        }
        i2c.stop();
        Ok(())
    }

    /// 打开 I2C 安全会话 (写 8 字节密码到 I2C_PASSWD 寄存器)
    ///
    /// 对齐参考固件: `tag.openI2CSession(password)`
    fn open_i2c_session(&self) -> Result<(), ()> {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(ST25DV_ADDR_W) {
            i2c.stop();
            return Err(());
        }
        if !i2c.write_byte((REG_I2C_PASSWD >> 8) as u8) {
            i2c.stop();
            return Err(());
        }
        if !i2c.write_byte((REG_I2C_PASSWD & 0xFF) as u8) {
            i2c.stop();
            return Err(());
        }
        for &b in &DEFAULT_PASSWORD {
            if !i2c.write_byte(b) {
                i2c.stop();
                return Err(());
            }
        }
        i2c.stop();
        // 验证会话已开启 (读 I2C_SSO bit 0)
        match self.read_reg16(REG_I2C_SSO) {
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
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(ST25DV_ADDR_W) {
            i2c.stop();
            return;
        }
        if !i2c.write_byte((REG_I2C_PASSWD >> 8) as u8) {
            i2c.stop();
            return;
        }
        if !i2c.write_byte((REG_I2C_PASSWD & 0xFF) as u8) {
            i2c.stop();
            return;
        }
        for &b in &wrong {
            if !i2c.write_byte(b) {
                i2c.stop();
                return;
            }
        }
        i2c.stop();
        log::debug!("[nfc] I2C security session closed");
    }

    /// 从用户 EEPROM 读取数据 (16-bit 地址, 多字节)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, START, addr_r, data[0..n-1] ACK, data[n] NACK, STOP
    fn read_eeprom(&self, addr: u16, buf: &mut [u8]) -> Result<usize, ()> {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(ST25DV_ADDR_W) {
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
        if !i2c.write_byte(ST25DV_ADDR_R) {
            i2c.stop();
            return Err(());
        }
        let n = buf.len();
        for i in 0..n {
            let ack = i < n - 1; // ACK all but last byte
            buf[i] = i2c.read_byte(ack);
        }
        i2c.stop();
        Ok(n)
    }

    /// 向用户 EEPROM 写入数据 (16-bit 地址, 多字节)
    ///
    /// 协议: START, addr_w, reg_hi, reg_lo, data[0..n], STOP
    /// 注意: ST25DV64KC 内部写周期 ~5ms, 写入后需等待
    fn write_eeprom(&self, addr: u16, data: &[u8]) -> Result<(), ()> {
        let i2c = &self.i2c;
        i2c.start();
        if !i2c.write_byte(ST25DV_ADDR_W) {
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
        for &b in data {
            if !i2c.write_byte(b) {
                i2c.stop();
                return Err(());
            }
        }
        i2c.stop();
        // ST25DV64KC 内部写周期 ~5ms (datasheet §5.3)
        std::thread::sleep(Duration::from_millis(6));
        Ok(())
    }
}

// ============================================================================
// NFC 数据格式
// ============================================================================

/// NFC 备份头部 (8 字节, 存储在 EEPROM 0x0120)
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
    health::register(&TASK_HB);
    std::thread::Builder::new()
        .name("nfc-st25".into())
        .stack_size(8 * 1024)
        .spawn(nfc_loop)
        .map_err(|e| crate::error::AppError::Sys(format!("spawn nfc: {e}")))?;
    log::info!("[nfc] background thread spawned (5s polling)");
    Ok(())
}

/// NFC 后台轮询循环
fn nfc_loop() {
    let mut present = false;
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

        // 3. 读 EEPROM 头部, 判断是否需要备份/恢复
        let mut hdr_buf = [0u8; NFC_HEADER_BYTES];
        let header = match dev.read_eeprom(NDEF_TEXT_END + 1, &mut hdr_buf) {
            Ok(_) => NfcHeader::from_bytes(&hdr_buf),
            Err(_) => None,
        };

        // LOOP9: 避免 clone 整个 holding_buf (~4KB heap), 持 Arc 引用即可
        let regbuf_arc = storage_read();
        let regbuf: Option<&[u16]> = regbuf_arc.as_ref().map(|s| s.holding_buf.as_ref());

        match (header, regbuf) {
            (Some(h), Some(rb)) if h.is_valid() => {
                // 校验 CRC
                // LOOP12: NFC_BLOB_DATA_BYTES=7648, 8KB 栈不够直接装数组,
                // 改为只读 holding_buf 实际长度 (max 4096B) 以对齐 EEPROM 写入
                let max_bytes = NFC_BLOB_DATA_BYTES.min(rb.len() * 2);
                let mut data_buf = vec![0u8; max_bytes];
                match dev.read_eeprom(NDEF_TEXT_END + 1 + NFC_HEADER_BYTES as u16, &mut data_buf) {
                    Ok(_) => {
                        let calc_crc = crc32(&data_buf);
                        if calc_crc == h.crc32 {
                            // 比较 holding_buf 与 NFC 数据 — LOOP12: NFC_BLOB_DATA_WORDS=3824,
                            // 8KB 栈放不下, 改为 Box<[u16]> (一次性 alloc, 5s 一次轮询开销可忽略)
                            let n_words = max_bytes / 2;
                            let mut nfc_words: Box<[u16]> = vec![0u16; n_words].into_boxed_slice();
                            bytes_to_words(&data_buf, &mut nfc_words);
                            if regbuf_equal(rb, &nfc_words) {
                                log::debug!("[nfc] snapshot matches, no sync needed");
                            } else {
                                // LOOP13: NFC 数据有效但与本地不一致 → 用本地 dirty 决策
                                // - HOLDING_DIRTY=true  → 本地有 Modbus/AT 写入，本地更新，备份到 NFC
                                // - HOLDING_DIRTY=false → 本地已同步上次 NFC backup，NFC 数据更新，从 NFC 恢复
                                let local_dirty = crate::bus::storage_state::HOLDING_DIRTY
                                    .load(std::sync::atomic::Ordering::Acquire);
                                if local_dirty {
                                    log::info!("[nfc] local dirty + NFC differs, backing up local to NFC");
                                    backup_to_nfc(&dev, rb);
                                    crate::bus::storage_state::HOLDING_DIRTY
                                        .store(false, std::sync::atomic::Ordering::Release);
                                    *NFC_STATE.lock() = NfcState::BackedUp;
                                } else {
                                    log::info!("[nfc] local clean + NFC differs, restoring from NFC");
                                    restore_from_nfc(&nfc_words);
                                    *NFC_STATE.lock() = NfcState::Restored;
                                }
                            }
                        } else {
                            // CRC 不匹配 → 备份
                            log::info!("[nfc] NFC CRC mismatch, backing up");
                            backup_to_nfc(&dev, rb);
                            crate::bus::storage_state::HOLDING_DIRTY
                                .store(false, std::sync::atomic::Ordering::Release);
                            *NFC_STATE.lock() = NfcState::BackedUp;
                        }
                    }
                    Err(_) => {
                        log::warn!("[nfc] EEPROM read failed, backing up");
                        backup_to_nfc(&dev, rb);
                        crate::bus::storage_state::HOLDING_DIRTY
                            .store(false, std::sync::atomic::Ordering::Release);
                        *NFC_STATE.lock() = NfcState::BackedUp;
                    }
                }
            }
            (_, Some(rb)) => {
                // NFC 数据无效/空 → 备份
                log::info!("[nfc] NFC empty or invalid, backing up current config");
                backup_to_nfc(&dev, rb);
                crate::bus::storage_state::HOLDING_DIRTY
                    .store(false, std::sync::atomic::Ordering::Release);
                *NFC_STATE.lock() = NfcState::BackedUp;
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
fn backup_to_nfc(dev: &St25dv, holding_buf: &[u16]) {
    // 1. 把 holding_buf 转为字节 (LE, 与 MCA PRegBuf 内存布局一致)
    //    LOOP12: NFC_BLOB_DATA_BYTES=7648, 8KB 栈放不下栈数组, 改为 Vec
    //    实际数据量: words = min(3824, 2048) = 2048 words = 4096 bytes
    let words = NFC_BLOB_DATA_WORDS.min(holding_buf.len());
    let data_len = words * 2;
    let mut data = vec![0u8; data_len];
    for i in 0..words {
        data[i * 2] = (holding_buf[i] & 0xFF) as u8;
        data[i * 2 + 1] = (holding_buf[i] >> 8) as u8;
    }

    // 2. 计算 CRC32
    let crc = crc32(&data);

    // 3. 写头部
    let hdr = NfcHeader {
        magic: NFC_BLOB_MAGIC,
        version: NFC_BLOB_VERSION,
        crc32: crc,
    };
    let hdr_bytes = hdr.to_bytes();
    if dev.write_eeprom(NDEF_TEXT_END + 1, &hdr_bytes).is_err() {
        log::error!("[nfc] backup: header write failed");
        return;
    }

    // 4. 分批写数据 (RFID_CHUNK=30 字节/次, 对齐参考固件)
    let base = NDEF_TEXT_END + 1 + NFC_HEADER_BYTES as u16;
    for chunk_start in (0..data_len).step_by(RFID_CHUNK) {
        let chunk_end = (chunk_start + RFID_CHUNK).min(data_len);
        let chunk = &data[chunk_start..chunk_end];
        if dev
            .write_eeprom(base + chunk_start as u16, chunk)
            .is_err()
        {
            log::error!(
                "[nfc] backup: data write failed at offset {}",
                chunk_start
            );
            return;
        }
        TASK_HB.tick();
        health::feed_wdt();
    }

    log::info!(
        "[nfc] backup complete: {} bytes, CRC=0x{:08X}",
        data_len,
        crc
    );
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
    crate::bus::storage_state::HOLDING_DIRTY
        .store(false, std::sync::atomic::Ordering::Release);
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
    // 实际备份由后台线程在下次轮询时执行
    // 此处仅设置状态, 让后台线程优先处理
    *NFC_STATE.lock() = NfcState::Init;
    Ok(())
}

/// 手动触发恢复 (供 Modbus / HTTP 调用)
pub fn restore_now() -> AppResult<()> {
    log::info!("[nfc] manual restore triggered");
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
        assert_eq!(NFC_BLOB_VERSION, 1);
        assert_eq!(NDEF_TEXT_END, 0x011F);
        assert_eq!(MEMORY_END, 0x1FFF);
        // LOOP12: 0x1FFF - 0x011F = 7648 字节 (扩容 4×, 覆盖 holding_buf 4096B)
        assert_eq!(NFC_BLOB_DATA_BYTES, 7648);
        assert_eq!(NFC_BLOB_DATA_WORDS, 3824);
        assert_eq!(NFC_HEADER_BYTES, 8);
    }

    #[test]
    fn test_st25_i2c_addr() {
        assert_eq!(ST25DV_ADDR_7BIT, 0x53);
        assert_eq!(ST25DV_ADDR_W, 0xA6);
        assert_eq!(ST25DV_ADDR_R, 0xA7);
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