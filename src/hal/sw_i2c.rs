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

/// Timing: half-period in microseconds (~100 kHz)
const T_HALF_US: u32 = 2; // 2μs → ~250kHz (reference firmware uses 4-6μs; we tune for speed)

pub struct SwI2c {
    sda: i32,
    scl: i32,
}

impl SwI2c {
    pub fn init(sda: i32, scl: i32) -> AppResult<Self> {
        // Configure both pins as push-pull OUTPUT initially
        // Matching reference: pinMode(pin, OUTPUT)
        unsafe {
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
            let ret = esp_idf_sys::gpio_config(&cfg);
            if ret != 0 {
                return Err(AppError::Io(format!(
                    "sw_i2c gpio{sda}/gpio{scl} config: esp_err=0x{ret:08X}"
                )));
            }
        }

        let this = Self { sda, scl };

        // Idle state: both HIGH
        this.sda_write(true);
        this.scl_write(true);

        log::info!("[sw_i2c] init: sda={sda} scl={scl}");
        Ok(this)
    }

    // ---- low-level pin access ----

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
            esp_idf_sys::gpio_set_direction(
                self.sda,
                esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT,
            );
        }
    }

    /// Set SDA as INPUT (matching `IIC_SDA_set_input()`)
    fn sda_set_input(&self) {
        unsafe {
            esp_idf_sys::gpio_set_direction(
                self.sda,
                esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT,
            );
        }
    }

    // ---- I2C protocol (exact match with reference firmware) ----

    /// START: SDA ↓ while SCL=H, then SCL ↓
    pub fn start(&self) {
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
