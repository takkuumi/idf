//! Software I2C (bit-banged) — exactly matching reference firmware nca9555.cpp.
//!
//! Key difference from standard I2C: uses push-pull OUTPUT mode on SDA
//! (not open-drain), switching to INPUT mode only when reading.
//! This matches the Arduino `pinMode(pin, OUTPUT)` / `pinMode(pin, INPUT)`
//! pattern used in the reference firmware.
//!
//! # Bus wiring
//! - IO bus:  SDA=GPIO35, SCL=GPIO36 (DI chip 0x40, DO chip 0x42)
//! - LED bus: SDA=GPIO38, SCL=GPIO37 (DI LED 0x40, DO LED 0x48)

use crate::error::{AppError, AppResult};
use core::cell::Cell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};

/// 保守的标准模式时序。GPIO 方向切换另有少量开销，实际总线频率不高于 100kHz。
/// 原 C++ 固件半周期为 4-6us；ST25DV 长 EEPROM 写入优先保证 ACK 稳定性。
const T_HALF_US: u32 = 5;

// PCA9555 与 ST25DV 会创建不同 SwI2c 实例，但物理上共用两组引脚。实例内的
// Spin 只能保护同一个驱动对象，无法阻止 NFC 与 IO 同时翻转同一 GPIO。
static IO_BUS_LOCK: AtomicBool = AtomicBool::new(false);
static LED_BUS_LOCK: AtomicBool = AtomicBool::new(false);
static OTHER_BUS_LOCK: AtomicBool = AtomicBool::new(false);

pub struct SwI2c {
    sda: i32,
    scl: i32,
    in_transaction: Cell<bool>,
}

impl SwI2c {
    pub fn init(sda: i32, scl: i32) -> AppResult<Self> {
        let this = Self {
            sda,
            scl,
            in_transaction: Cell::new(false),
        };
        this.acquire_bus();

        // Configure both pins as push-pull OUTPUT initially
        // Matching reference: pinMode(pin, OUTPUT)
        let ret = unsafe {
            // SAFETY: sda/scl 来自 config.rs::pins 编译期常量, 与其它驱动互斥.
            // mask 是 u64 GPIO 位掩码, cfg 是栈局部 &, 无别名访问, 调用后立即 ret 检查.
            let mask = (1u64 << sda) | (1u64 << scl);
            let cfg = esp_idf_sys::gpio_config_t {
                pin_bit_mask: mask,
                mode: esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT, // push-pull, not open-drain!
                pull_up_en: 0u32,
                pull_down_en: 0u32,
                intr_type: 0,
            };
            esp_idf_sys::gpio_config(&cfg)
        };
        if ret != 0 {
            this.release_bus();
            return Err(AppError::Io(format!(
                "sw_i2c gpio{sda}/gpio{scl} config: esp_err=0x{ret:08X}"
            )));
        }

        // Idle state: both HIGH
        this.sda_write(true);
        this.scl_write(true);
        this.release_bus();

        log::info!("[sw_i2c] init: sda={sda} scl={scl}");
        Ok(this)
    }

    // ---- low-level pin access ----

    fn physical_bus_lock(&self) -> &'static AtomicBool {
        match (self.sda, self.scl) {
            (35, 36) => &IO_BUS_LOCK,
            (38, 37) => &LED_BUS_LOCK,
            _ => &OTHER_BUS_LOCK,
        }
    }

    fn acquire_bus(&self) {
        if self.in_transaction.replace(true) {
            return; // repeated START belongs to the same transaction
        }
        let lock = self.physical_bus_lock();
        let mut contention = 0u8;
        while lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while lock.load(Ordering::Relaxed) {
                contention = contention.wrapping_add(1);
                if contention == 0 {
                    // NFC and the PCA9555 use distinct driver instances on the
                    // same pins. Let a preempted lower-priority owner finish
                    // its bounded transaction instead of spinning forever.
                    #[cfg(target_os = "espidf")]
                    unsafe {
                        esp_idf_sys::vTaskDelay(1);
                    }
                    #[cfg(not(target_os = "espidf"))]
                    std::thread::yield_now();
                } else {
                    spin_loop();
                }
            }
        }
    }

    fn release_bus(&self) {
        if self.in_transaction.replace(false) {
            self.physical_bus_lock().store(false, Ordering::Release);
        }
    }

    fn delay_half(&self) {
        // SAFETY: ets_delay_us 是 ROM 提供的 CPU 空转, 无内存访问, 无副作用.
        unsafe { esp_idf_sys::ets_delay_us(T_HALF_US) };
    }

    fn delay_1us(&self) {
        // SAFETY: 同 delay_half.
        unsafe { esp_idf_sys::ets_delay_us(1) };
    }

    fn sda_write(&self, high: bool) {
        self.sda_set_output();
        // SAFETY: self.sda 是 init 时的 cfg 常量 GPIO 号, gpio_set_level 仅写该引脚.
        unsafe { esp_idf_sys::gpio_set_level(self.sda, high as u32) };
    }

    fn sda_read(&self) -> bool {
        self.sda_set_input();
        // SAFETY: 同 sda_write, 仅读 GPIO 电平寄存器.
        unsafe { esp_idf_sys::gpio_get_level(self.sda) != 0 }
    }

    fn scl_write(&self, high: bool) {
        // SAFETY: self.scl 同 self.sda.
        unsafe { esp_idf_sys::gpio_set_level(self.scl, high as u32) };
    }

    /// Set SDA as push-pull OUTPUT (matching `IIC_SDA_set_output()`)
    fn sda_set_output(&self) {
        unsafe {
            esp_idf_sys::gpio_set_direction(self.sda, esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT);
        }
    }

    /// Set SDA as INPUT (matching `IIC_SDA_set_input()`)
    fn sda_set_input(&self) {
        unsafe {
            esp_idf_sys::gpio_set_direction(self.sda, esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT);
        }
    }

    // ---- I2C protocol (exact match with reference firmware) ----

    /// START: SDA ↓ while SCL=H, then SCL ↓
    pub fn start(&self) {
        self.acquire_bus();
        self.sda_set_output();
        self.sda_write(true);
        self.delay_half();
        self.scl_write(true);
        self.delay_half();
        self.sda_write(false);
        self.delay_half();
        self.scl_write(false);
    }

    /// STOP: SCL=H then SDA ↑
    pub fn stop(&self) {
        self.sda_set_output();
        self.scl_write(false);
        self.delay_half();
        self.sda_write(false);
        self.delay_half();
        self.scl_write(true);
        self.delay_half();
        self.sda_write(true);
        self.delay_half();
        self.release_bus();
    }

    /// Write one byte; returns true if ACK received (slave pulls SDA LOW).
    pub fn write_byte(&self, byte: u8) -> bool {
        let mut txd = byte;
        for _ in 0..8 {
            self.sda_write(txd & 0x80 != 0);
            txd <<= 1;
            self.delay_half();
            self.scl_write(true);
            self.delay_half();
            self.scl_write(false);
        }
        // Release SDA, switch to input to read ACK
        self.sda_write(true);
        self.sda_set_input();
        self.delay_half();
        self.scl_write(true);
        self.delay_half();
        let ack = !self.sda_read();
        self.scl_write(false);
        self.sda_set_output();
        ack
    }

    /// Read one byte; sends ACK (LOW) or NACK (HIGH).
    pub fn read_byte(&self, ack: bool) -> u8 {
        let mut receive: u8 = 0;
        self.sda_set_input();
        for _ in 0..8 {
            self.delay_half();
            self.scl_write(true);
            self.delay_half();
            receive <<= 1;
            if self.sda_read() {
                receive |= 1;
            }
            self.scl_write(false);
        }
        // Send ACK/NACK
        self.sda_set_output();
        self.delay_half();
        self.sda_write(!ack);
        self.delay_half();
        self.scl_write(true);
        self.delay_half();
        self.scl_write(false);
        self.sda_write(true); // release
        receive
    }

    /// Wait for ACK with timeout. Returns true if ACK received.
    pub fn wait_ack(&self) -> bool {
        let mut timeout: u16 = 0;
        self.sda_set_input();
        self.delay_1us();
        self.scl_write(true);
        self.delay_1us();
        while self.sda_read() {
            timeout += 1;
            self.delay_1us();
            if timeout > 250 {
                self.scl_write(false);
                self.sda_set_output();
                return false;
            }
        }
        self.scl_write(false);
        self.sda_set_output();
        true
    }
}
