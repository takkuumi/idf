//! PCA9555 (NCA9555 compatible) driver — matching reference firmware nca9555.cpp.
//!
//! Hardware model: **F16** (16 DI + 16 DO), same as `#define NCA9555F16`.
//!
//! ## IO Bus (SDA=GPIO35, SCL=GPIO36)
//! | Chip | Addr | Purpose |
//! |------|------|---------|
//! | DI   | 0x40 | 16 inputs (ports 0+1), pull-up → LOW = active |
//! | DO   | 0x42 | 16 outputs (ports 0+1), bit-reversed per EXchg_ByteHl |
//!
//! ## LED Bus (SDA=GPIO38, SCL=GPIO37)
//! | Chip | Addr | Purpose |
//! |------|------|---------|
//! | DI LED | 0x40 | Mirror DI state (raw, low-active: 0=ON) |
//! | Other  | 0x44 | Status LEDs (PWR/RUN/ERR/BT/RS485/FIB) |
//! | DO LED | 0x48 | Mirror DO state (inverted, low-active: 0=ON) |
//!
//! ## Data transforms (matching reference firmware)
//! - DO IO: `bit_reverse(byte)` — EXchg_ByteHl
//! - DO LED: `!byte` — inverted (low-active)
//! - DI read: `!raw` — inverted (active-low input)
//! - DI LED: `raw` — direct (low-active LED, 0=input-active=LED-ON)

use std::sync::atomic::{AtomicU16, Ordering};

use crate::error::{AppError, AppResult};
use crate::sync::Spin;

use super::digital_io::DigitalIo;
use super::sw_i2c::SwI2c;

// PCA9555 registers
const REG_INPUT_0: u8 = 0x00;
const REG_INPUT_1: u8 = 0x01;
const REG_OUTPUT_0: u8 = 0x02;
const REG_OUTPUT_1: u8 = 0x03;
const REG_CONFIG_0: u8 = 0x06;
const REG_CONFIG_1: u8 = 0x07;

// I2C addresses (8-bit: 7-bit << 1 | 0)
const ADDR_DI_IO: u8 = 0x40;
const ADDR_DO_IO: u8 = 0x42;
const ADDR_DI_LED: u8 = 0x40;
const ADDR_STATUS_LED: u8 = 0x44;
const ADDR_DO_LED: u8 = 0x48;

// F16 status LEDs are active-low. These masks match nca9555.h in the
// production MCA firmware; clearing a bit turns the corresponding LED on.
const STATUS_LED_BT: u8 = 0xFD;

/// Bit-reverse a byte (matching reference `EXchg_ByteHl`).
fn bit_reverse(b: u8) -> u8 {
    let mut result: u8 = 0;
    let mut x = b;
    for _ in 0..8 {
        result = (result << 1) | (x & 1);
        x >>= 1;
    }
    result
}

// ============================================================================
// Bus-level I2C access (shared between chips on the same physical bus)
// ============================================================================
struct BusI2c {
    i2c: Spin<SwI2c>,
}

impl BusI2c {
    fn new(i2c: SwI2c) -> Self {
        Self {
            i2c: Spin::new(i2c),
        }
    }

    fn write_reg(&self, addr: u8, reg: u8, data: u8) -> AppResult<()> {
        let i2c = self.i2c.lock();
        i2c.start();
        if !i2c.write_byte(addr) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} wr 0x{reg:02X}: NACK addr"
            )));
        }
        if !i2c.write_byte(reg) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} wr 0x{reg:02X}: NACK reg"
            )));
        }
        if !i2c.write_byte(data) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} wr 0x{reg:02X}: NACK data"
            )));
        }
        i2c.stop();
        Ok(())
    }

    /// PCA9555 寄存器指针会自动递增；一次事务连续写两个端口，减少软件 I2C
    /// START/地址/寄存器开销，并保证双端口更新属于同一总线事务。
    fn write_regs2(&self, addr: u8, reg: u8, data: [u8; 2]) -> AppResult<()> {
        let i2c = self.i2c.lock();
        i2c.start();
        if !i2c.write_byte(addr) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} wr2 0x{reg:02X}: NACK addr"
            )));
        }
        if !i2c.write_byte(reg) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} wr2 0x{reg:02X}: NACK reg"
            )));
        }
        for byte in data {
            if !i2c.write_byte(byte) {
                i2c.stop();
                return Err(AppError::Io(format!(
                    "I2C 0x{addr:02X} wr2 0x{reg:02X}: NACK data"
                )));
            }
        }
        i2c.stop();
        Ok(())
    }

    fn read_reg(&self, addr: u8, reg: u8) -> AppResult<u8> {
        let i2c = self.i2c.lock();
        i2c.start();
        if !i2c.write_byte(addr) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} rd 0x{reg:02X}: NACK addr"
            )));
        }
        if !i2c.write_byte(reg) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} rd 0x{reg:02X}: NACK reg"
            )));
        }
        i2c.start();
        if !i2c.write_byte(addr | 1) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} rd 0x{reg:02X}: NACK read"
            )));
        }
        let val = i2c.read_byte(false);
        i2c.stop();
        Ok(val)
    }

    /// 连续读取 input port 0/1。第一个字节回 ACK，第二个字节回 NACK 后 STOP。
    fn read_regs2(&self, addr: u8, reg: u8) -> AppResult<[u8; 2]> {
        let i2c = self.i2c.lock();
        i2c.start();
        if !i2c.write_byte(addr) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} rd2 0x{reg:02X}: NACK addr"
            )));
        }
        if !i2c.write_byte(reg) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} rd2 0x{reg:02X}: NACK reg"
            )));
        }
        i2c.start();
        if !i2c.write_byte(addr | 1) {
            i2c.stop();
            return Err(AppError::Io(format!(
                "I2C 0x{addr:02X} rd2 0x{reg:02X}: NACK read"
            )));
        }
        let data = [i2c.read_byte(true), i2c.read_byte(false)];
        i2c.stop();
        Ok(data)
    }
}

// ============================================================================
// Pca9555Duo — F16 DO+DI+LED driver implementing DigitalIo
// ============================================================================
pub struct Pca9555Duo {
    io_bus: BusI2c,
    led_bus: BusI2c,
    do_cache: AtomicU16,
    /// Last written IO port values (bit-reversed), for incremental write
    last_io: Spin<(u8, u8)>,
    /// Last written LED port values (inverted), for incremental write
    last_led: Spin<(u8, u8)>,
    /// Last raw DI values mirrored to the DI LED expander.
    last_di_led: Spin<(u8, u8)>,
    status_led: Spin<u8>,
}

impl Pca9555Duo {
    pub fn init(io_sda: i32, io_scl: i32, led_sda: i32, led_scl: i32) -> AppResult<Self> {
        // Power sequence matching reference: init chips with power OFF,
        // then power ON after init.
        // Note: GPIO21/33 are already set HIGH by hal::init before this call,
        // which is opposite of reference. This is OK for F16 — boards get power
        // before I2C init without issue.

        let io_i2c = SwI2c::init(io_sda, io_scl)?;
        let led_i2c = SwI2c::init(led_sda, led_scl)?;
        let io_bus = BusI2c::new(io_i2c);
        let led_bus = BusI2c::new(led_i2c);

        // ---- IO Bus init (matching nca9555_init for NCA9555F16) ----
        // Clear DO chip outputs first
        io_bus.write_reg(ADDR_DO_IO, REG_OUTPUT_0, 0x00)?;
        io_bus.write_reg(ADDR_DO_IO, REG_OUTPUT_1, 0x00)?;
        // DI chip: all inputs
        io_bus.write_reg(ADDR_DI_IO, REG_CONFIG_0, 0xFF)?;
        io_bus.write_reg(ADDR_DI_IO, REG_CONFIG_1, 0xFF)?;
        // DO chip: all outputs
        io_bus.write_reg(ADDR_DO_IO, REG_CONFIG_0, 0x00)?;
        io_bus.write_reg(ADDR_DO_IO, REG_CONFIG_1, 0x00)?;
        log::info!("[pca9555] io bus: DI@0x40(input) DO@0x42(output)");

        // ---- LED Bus init (all outputs, all LEDs OFF) ----
        for &(addr, _name) in &[
            (ADDR_DI_LED, "DI_LED@0x40"),
            (ADDR_STATUS_LED, "STATUS@0x44"),
            (ADDR_DO_LED, "DO_LED@0x48"),
        ] {
            led_bus.write_reg(addr, REG_CONFIG_0, 0x00)?;
            led_bus.write_reg(addr, REG_CONFIG_1, 0x00)?;
            led_bus.write_reg(addr, REG_OUTPUT_0, 0xFF)?;
            led_bus.write_reg(addr, REG_OUTPUT_1, 0xFF)?;
        }

        // Status LEDs: PWR+RUN ON (matching reference: gucOtherLedState = 0xFF & PWRLED & RUNLED)
        // PWRLED=0xFE, RUNLED=0xDF → gucOtherLedState = 0xDE (bits for PWR+RUN cleared = ON)
        let other_led: u8 = 0xFE & 0xDF; // = 0xDE
        led_bus.write_reg(ADDR_STATUS_LED, REG_OUTPUT_0, other_led)?;
        log::info!("[pca9555] status LEDs: PWR+RUN on (0x{other_led:02X})");

        log::info!("[pca9555] led bus: all chips ready, all LEDs off except PWR/RUN");
        log::info!("[pca9555] F16 (16DI+16DO) init complete");

        Ok(Self {
            io_bus,
            led_bus,
            do_cache: AtomicU16::new(0x0000),
            last_io: Spin::new((0x00, 0x00)),
            last_led: Spin::new((0xFF, 0xFF)), // init=off matches LED init above
            last_di_led: Spin::new((0xFF, 0xFF)),
            status_led: Spin::new(other_led),
        })
    }

    /// Write DO to IO bus (bit-reversed) + LED bus (inverted).
    /// Only writes ports that actually changed (incremental update).
    fn apply_do(&self, val: u16) -> AppResult<()> {
        let lo = (val & 0xFF) as u8;
        let hi = ((val >> 8) & 0xFF) as u8;
        let io_lo = bit_reverse(lo);
        let io_hi = bit_reverse(hi);
        let led_lo = !lo;
        let led_hi = !bit_reverse(hi);

        // IO bus: only write changed ports
        {
            let mut last = self.last_io.lock();
            if last.0 != io_lo {
                self.io_bus.write_reg(ADDR_DO_IO, REG_OUTPUT_0, io_lo)?;
                last.0 = io_lo;
            }
            if last.1 != io_hi {
                self.io_bus.write_reg(ADDR_DO_IO, REG_OUTPUT_1, io_hi)?;
                last.1 = io_hi;
            }
        }

        // LED bus: only write changed ports
        {
            let mut last = self.last_led.lock();
            if last.0 != led_lo {
                self.led_bus.write_reg(ADDR_DO_LED, REG_OUTPUT_0, led_lo)?;
                last.0 = led_lo;
            }
            if last.1 != led_hi {
                self.led_bus.write_reg(ADDR_DO_LED, REG_OUTPUT_1, led_hi)?;
                last.1 = led_hi;
            }
        }

        Ok(())
    }

    /// Update DI LEDs from raw input values.
    fn update_di_leds(&self, port0: u8, port1: u8) -> AppResult<()> {
        let mut last = self.last_di_led.lock();
        if *last == (port0, port1) {
            return Ok(());
        }
        self.led_bus
            .write_regs2(ADDR_DI_LED, REG_OUTPUT_0, [port0, port1])?;
        *last = (port0, port1);
        Ok(())
    }

    /// Keep the physical BT indicator aligned with the GATT connection state.
    /// This is called from the main task, never from a Bluedroid callback.
    pub fn set_bt_led(&self, connected: bool) -> AppResult<()> {
        let mut current = self.status_led.lock();
        let next = if connected {
            *current & STATUS_LED_BT
        } else {
            *current | !STATUS_LED_BT
        };
        if next == *current {
            return Ok(());
        }
        self.led_bus
            .write_reg(ADDR_STATUS_LED, REG_OUTPUT_0, next)?;
        *current = next;
        Ok(())
    }
}

impl DigitalIo for Pca9555Duo {
    fn di_count(&self) -> usize {
        16
    }
    fn do_count(&self) -> usize {
        16
    }

    fn read_di_all(&self) -> AppResult<u64> {
        let [raw0, raw1] = self.io_bus.read_regs2(ADDR_DI_IO, REG_INPUT_0)?;

        // Update DI LEDs with raw value (matching reference: savedata = backdata)
        let _ = self.update_di_leds(raw0, raw1);

        // Invert: input is active-low (pull-up → LOW=active → 1 in Modbus)
        let di0 = (!raw0) as u64;
        // Reference bit-reverses port1: EXchg_ByteHl then invert
        let di1_rev = bit_reverse(raw1) as u64;
        Ok(((di1_rev ^ 0xFF) << 8) | di0)
    }

    fn read_di(&self, idx: usize) -> AppResult<bool> {
        if idx >= 16 {
            return Err(AppError::Io(format!("di idx {idx} out of range")));
        }
        let all = self.read_di_all()?;
        Ok(all & (1u64 << idx) != 0)
    }

    fn write_do_all(&self, value: u64) -> AppResult<()> {
        let bits = (value & 0xFFFF) as u16;
        if self.do_cache.load(Ordering::Acquire) == bits {
            return Ok(());
        }
        self.apply_do(bits)?;
        // 缓存表示已确认的硬件状态，不能在 I2C 写入成功前发布。
        self.do_cache.store(bits, Ordering::Release);
        Ok(())
    }

    fn write_do(&self, idx: usize, on: bool) -> AppResult<()> {
        if idx >= 16 {
            return Err(AppError::Io(format!("do idx {idx} out of range")));
        }
        let mask = 1u16 << idx;
        let current = self.do_cache.load(Ordering::Acquire);
        let bits = if on { current | mask } else { current & !mask };
        if current == bits {
            return Ok(());
        }
        self.apply_do(bits)?;
        self.do_cache.store(bits, Ordering::Release);
        Ok(())
    }

    fn read_do_cached(&self) -> u64 {
        self.do_cache.load(Ordering::Acquire) as u64
    }

    fn read_do_actual(&self) -> AppResult<u64> {
        let lo = self.io_bus.read_reg(ADDR_DO_IO, REG_OUTPUT_0)?;
        let hi = self.io_bus.read_reg(ADDR_DO_IO, REG_OUTPUT_1)?;
        // Reverse the bit-reversal
        let d0 = bit_reverse(lo) as u64;
        let d1 = bit_reverse(hi) as u64;
        Ok(d0 | (d1 << 8))
    }
}
