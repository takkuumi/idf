//! PCA9555 (NCA9555 compatible) driver — matching reference firmware nca9555.cpp.
//!
//! Hardware models: **F3** (16 DI + 16 DO) and **F4** (48 DI + 48 DO),
//! matching the reference NCA9555 F16/F48 address banks.
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

// Status LEDs are active-low. These masks match nca9555.h in the
// production MCA firmware; clearing a bit turns the corresponding LED on.
const STATUS_LED_BT: u8 = 0xFD;
const STATUS_LED_RS485: [u8; 2] = [0xFB, 0xF7];
const STATUS_LED_FIBER: [u8; 2] = [0xEF, 0x7F];

fn with_status_led(current: u8, mask: u8, on: bool) -> u8 {
    if on { current & mask } else { current | !mask }
}

fn with_rs485_leds(current: u8, active: [bool; 2]) -> u8 {
    with_status_led(
        with_status_led(current, STATUS_LED_RS485[0], active[0]),
        STATUS_LED_RS485[1],
        active[1],
    )
}

fn with_fiber_leds(current: u8, active: [bool; 2]) -> u8 {
    with_status_led(
        with_status_led(current, STATUS_LED_FIBER[0], active[0]),
        STATUS_LED_FIBER[1],
        active[1],
    )
}

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
// Pca9555Duo — F3/F4 PCA9555 banks implementing DigitalIo
// ============================================================================
const MAX_CHIPS: usize = 3;

#[cfg(feature = "f4")]
const DI_IO_ADDRS: &[u8] = &[0x40, 0x44, 0x42];
#[cfg(not(feature = "f4"))]
const DI_IO_ADDRS: &[u8] = &[ADDR_DI_IO];
#[cfg(feature = "f4")]
const DO_IO_ADDRS: &[u8] = &[0x48, 0x4C, 0x4E];
#[cfg(not(feature = "f4"))]
const DO_IO_ADDRS: &[u8] = &[ADDR_DO_IO];
#[cfg(feature = "f4")]
const DI_LED_ADDRS: &[u8] = &[0x40, 0x44, 0x42];
#[cfg(not(feature = "f4"))]
const DI_LED_ADDRS: &[u8] = &[ADDR_DI_LED];
#[cfg(feature = "f4")]
const DO_LED_ADDRS: &[u8] = &[0x48, 0x4C, 0x4E];
#[cfg(not(feature = "f4"))]
const DO_LED_ADDRS: &[u8] = &[ADDR_DO_LED];
#[cfg(feature = "f4")]
const STATUS_LED_ADDR: u8 = 0x46;
#[cfg(not(feature = "f4"))]
const STATUS_LED_ADDR: u8 = ADDR_STATUS_LED;

#[cfg(feature = "f4")]
const STATUS_LED_RS485_F4: [u8; 2] = [0xDF, 0xFB];
#[cfg(feature = "f4")]
const STATUS_LED_FIBER_F4: [u8; 2] = [0xF7, 0x7F];
#[cfg(feature = "f4")]
const STATUS_LED_BT_F4: u8 = 0xEF;

pub struct Pca9555Duo {
    io_bus: BusI2c,
    led_bus: BusI2c,
    do_cache: crate::sync::AtomicBits64,
    /// Last written IO port values (bit-reversed), indexed by DO chip.
    last_io: Spin<[(u8, u8); MAX_CHIPS]>,
    /// Last written LED port values (inverted), indexed by DO LED chip.
    last_led: Spin<[(u8, u8); MAX_CHIPS]>,
    /// Last raw DI values mirrored to DI LED chips.
    last_di_led: Spin<[(u8, u8); MAX_CHIPS]>,
    #[cfg(feature = "f4")]
    status_led: Spin<(u8, u8)>,
    #[cfg(not(feature = "f4"))]
    status_led: Spin<u8>,
}

impl Pca9555Duo {
    pub fn init(io_sda: i32, io_scl: i32, led_sda: i32, led_scl: i32) -> AppResult<Self> {
        let io_bus = BusI2c::new(SwI2c::init(io_sda, io_scl)?);
        let led_bus = BusI2c::new(SwI2c::init(led_sda, led_scl)?);

        // Inputs are high-impedance with the board's external pull-ups.
        for &addr in DI_IO_ADDRS {
            io_bus.write_reg(addr, REG_CONFIG_0, 0xFF)?;
            io_bus.write_reg(addr, REG_CONFIG_1, 0xFF)?;
        }
        // Set safe output latches before enabling outputs.
        for &addr in DO_IO_ADDRS {
            io_bus.write_regs2(addr, REG_OUTPUT_0, [0x00, 0x00])?;
            io_bus.write_regs2(addr, REG_CONFIG_0, [0x00, 0x00])?;
        }
        // DI/DO mirrors and status LEDs are all active-low outputs.
        for &addr in DI_LED_ADDRS
            .iter()
            .chain(DO_LED_ADDRS)
            .chain([STATUS_LED_ADDR].iter())
        {
            led_bus.write_regs2(addr, REG_CONFIG_0, [0x00, 0x00])?;
            led_bus.write_regs2(addr, REG_OUTPUT_0, [0xFF, 0xFF])?;
        }

        #[cfg(feature = "f4")]
        let status_led = (0xFC, 0xFF); // PWR=ON, RUN=ON; other indicators OFF
        #[cfg(not(feature = "f4"))]
        let status_led = 0xDE; // F3/F16-compatible PWR+RUN state
        #[cfg(feature = "f4")]
        led_bus.write_regs2(STATUS_LED_ADDR, REG_OUTPUT_0, [status_led.0, status_led.1])?;
        #[cfg(not(feature = "f4"))]
        led_bus.write_reg(STATUS_LED_ADDR, REG_OUTPUT_0, status_led)?;

        log::info!(
            "[pca9555] initialized: DI={:?} DO={:?} ({}DI/{}DO)",
            DI_IO_ADDRS,
            DO_IO_ADDRS,
            crate::config::hw_version::DI_COUNT,
            crate::config::hw_version::DO_COUNT
        );
        Ok(Self {
            io_bus,
            led_bus,
            do_cache: crate::sync::AtomicBits64::new(0),
            last_io: Spin::new([(0, 0); MAX_CHIPS]),
            last_led: Spin::new([(0xFF, 0xFF); MAX_CHIPS]),
            last_di_led: Spin::new([(0xFF, 0xFF); MAX_CHIPS]),
            status_led: Spin::new(status_led),
        })
    }

    fn apply_do(&self, value: u64) -> AppResult<()> {
        let mut last_io = self.last_io.lock();
        let mut last_led = self.last_led.lock();
        for (index, (&io_addr, &led_addr)) in DO_IO_ADDRS.iter().zip(DO_LED_ADDRS).enumerate() {
            let raw = ((value >> (index * 16)) & 0xFFFF) as u16;
            let lo = raw as u8;
            let hi = (raw >> 8) as u8;
            let io = [bit_reverse(lo), bit_reverse(hi)];
            let led = [!lo, !bit_reverse(hi)];
            if last_io[index] != (io[0], io[1]) {
                self.io_bus.write_regs2(io_addr, REG_OUTPUT_0, io)?;
                last_io[index] = (io[0], io[1]);
            }
            if last_led[index] != (led[0], led[1]) {
                self.led_bus.write_regs2(led_addr, REG_OUTPUT_0, led)?;
                last_led[index] = (led[0], led[1]);
            }
        }
        Ok(())
    }

    fn update_di_leds(&self, index: usize, addr: u8, raw: [u8; 2]) -> AppResult<()> {
        let mut last = self.last_di_led.lock();
        if last[index] == (raw[0], raw[1]) {
            return Ok(());
        }
        self.led_bus.write_regs2(addr, REG_OUTPUT_0, raw)?;
        last[index] = (raw[0], raw[1]);
        Ok(())
    }

    pub fn set_bt_led(&self, connected: bool) -> AppResult<()> {
        #[cfg(feature = "f4")]
        {
            let mut current = self.status_led.lock();
            let next = if connected {
                (current.0 & STATUS_LED_BT_F4, current.1)
            } else {
                (current.0 | !STATUS_LED_BT_F4, current.1)
            };
            if next != *current {
                self.led_bus
                    .write_regs2(STATUS_LED_ADDR, REG_OUTPUT_0, [next.0, next.1])?;
                *current = next;
            }
            Ok(())
        }
        #[cfg(not(feature = "f4"))]
        {
            let mut current = self.status_led.lock();
            let next = with_status_led(*current, STATUS_LED_BT, connected);
            if next != *current {
                self.led_bus
                    .write_reg(STATUS_LED_ADDR, REG_OUTPUT_0, next)?;
                *current = next;
            }
            Ok(())
        }
    }

    pub fn set_port_leds(&self, rs485: [bool; 2], fiber: [bool; 2]) -> AppResult<()> {
        #[cfg(feature = "f4")]
        {
            let mut current = self.status_led.lock();
            let mut next = *current;
            for (i, &active) in rs485.iter().enumerate() {
                next.0 = if active {
                    next.0 & STATUS_LED_RS485_F4[i]
                } else {
                    next.0 | !STATUS_LED_RS485_F4[i]
                };
            }
            next.0 = if fiber[0] {
                next.0 & STATUS_LED_FIBER_F4[0]
            } else {
                next.0 | !STATUS_LED_FIBER_F4[0]
            };
            next.1 = if fiber[1] {
                next.1 & STATUS_LED_FIBER_F4[1]
            } else {
                next.1 | !STATUS_LED_FIBER_F4[1]
            };
            if next != *current {
                self.led_bus
                    .write_regs2(STATUS_LED_ADDR, REG_OUTPUT_0, [next.0, next.1])?;
                *current = next;
            }
            Ok(())
        }
        #[cfg(not(feature = "f4"))]
        {
            let mut current = self.status_led.lock();
            let next = with_fiber_leds(with_rs485_leds(*current, rs485), fiber);
            if next != *current {
                self.led_bus
                    .write_reg(STATUS_LED_ADDR, REG_OUTPUT_0, next)?;
                *current = next;
            }
            Ok(())
        }
    }
}

impl DigitalIo for Pca9555Duo {
    fn di_count(&self) -> usize {
        crate::config::hw_version::DI_COUNT
    }
    fn do_count(&self) -> usize {
        crate::config::hw_version::DO_COUNT
    }

    fn read_di_all(&self) -> AppResult<u64> {
        let mut result = 0u64;
        for (index, (&io_addr, &led_addr)) in DI_IO_ADDRS.iter().zip(DI_LED_ADDRS).enumerate() {
            let raw = self.io_bus.read_regs2(io_addr, REG_INPUT_0)?;
            let _ = self.update_di_leds(index, led_addr, raw);
            let bits = (!raw[0] as u64) | ((bit_reverse(raw[1]) ^ 0xFF) as u64) << 8;
            result |= bits << (index * 16);
        }
        Ok(result)
    }

    fn read_di(&self, idx: usize) -> AppResult<bool> {
        if idx >= self.di_count() {
            return Err(AppError::Io(format!("di idx {idx} out of range")));
        }
        Ok(self.read_di_all()? & (1u64 << idx) != 0)
    }

    fn write_do_all(&self, value: u64) -> AppResult<()> {
        let mask = if self.do_count() >= 64 {
            u64::MAX
        } else {
            (1u64 << self.do_count()) - 1
        };
        let value = value & mask;
        if self.do_cache.load_bits() == value {
            return Ok(());
        }
        self.apply_do(value)?;
        self.do_cache.store_bits(value);
        Ok(())
    }

    fn write_do(&self, idx: usize, on: bool) -> AppResult<()> {
        if idx >= self.do_count() {
            return Err(AppError::Io(format!("do idx {idx} out of range")));
        }
        let current = self.do_cache.load_bits();
        let value = if on {
            current | (1u64 << idx)
        } else {
            current & !(1u64 << idx)
        };
        self.write_do_all(value)
    }

    fn read_do_cached(&self) -> u64 {
        self.do_cache.load_bits()
    }

    fn read_do_actual(&self) -> AppResult<u64> {
        let mut result = 0u64;
        for (index, &addr) in DO_IO_ADDRS.iter().enumerate() {
            let raw = self.io_bus.read_regs2(addr, REG_OUTPUT_0)?;
            result |= (bit_reverse(raw[0]) as u64) << (index * 16);
            result |= (bit_reverse(raw[1]) as u64) << (index * 16 + 8);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "f3")]
    fn test_f3_pca_bank_layout() {
        assert_eq!(DI_IO_ADDRS, &[0x40]);
        assert_eq!(DO_IO_ADDRS, &[0x42]);
        assert_eq!(crate::config::hw_version::DI_COUNT, 16);
        assert_eq!(crate::config::hw_version::DO_COUNT, 16);
    }

    #[test]
    #[cfg(feature = "f4")]
    fn test_f4_pca_bank_layout() {
        assert_eq!(DI_IO_ADDRS, &[0x40, 0x44, 0x42]);
        assert_eq!(DO_IO_ADDRS, &[0x48, 0x4C, 0x4E]);
        assert_eq!(crate::config::hw_version::DI_COUNT, 48);
        assert_eq!(crate::config::hw_version::DO_COUNT, 48);
    }

    #[test]
    fn test_rs485_leds_are_active_low_and_independent() {
        assert_eq!(with_rs485_leds(0xFF, [false, false]), 0xFF);
        assert_eq!(with_rs485_leds(0xFF, [true, false]), 0xFB);
        assert_eq!(with_rs485_leds(0xFF, [false, true]), 0xF7);
        assert_eq!(with_rs485_leds(0xFF, [true, true]), 0xF3);
    }

    #[test]
    fn test_rs485_leds_preserve_bt_power_run_and_unrelated_bits() {
        for current in 0..=u8::MAX {
            for active in [[false, false], [true, false], [false, true], [true, true]] {
                let next = with_rs485_leds(current, active);
                assert_eq!(next & 0xF3, current & 0xF3, "status={current:02X}");
                assert_eq!(with_rs485_leds(next, active), next);
            }
        }
        let bt_on = with_status_led(0xDE, STATUS_LED_BT, true);
        let rs485_on = with_rs485_leds(bt_on, [true, true]);
        assert_eq!(rs485_on, 0xD0);
        assert_eq!(with_rs485_leds(rs485_on, [false, false]), bt_on);
        assert_eq!(with_status_led(rs485_on, STATUS_LED_BT, false), 0xD2);
    }

    #[test]
    fn test_fiber_leds_are_active_low_and_independent() {
        assert_eq!(with_fiber_leds(0xFF, [false, false]), 0xFF);
        assert_eq!(with_fiber_leds(0xFF, [true, false]), 0xEF);
        assert_eq!(with_fiber_leds(0xFF, [false, true]), 0x7F);
        assert_eq!(with_fiber_leds(0xFF, [true, true]), 0x6F);
    }

    #[test]
    fn test_port_led_groups_preserve_bt_power_run_and_error_bits() {
        let states = [[false, false], [true, false], [false, true], [true, true]];
        for current in 0..=u8::MAX {
            for rs485 in states {
                for fiber in states {
                    let next = with_fiber_leds(with_rs485_leds(current, rs485), fiber);
                    assert_eq!(next & 0x63, current & 0x63, "status={current:02X}");
                    assert_eq!(
                        next,
                        with_rs485_leds(with_fiber_leds(current, fiber), rs485),
                        "LED groups must not overwrite each other"
                    );
                    assert_eq!(with_fiber_leds(with_rs485_leds(next, rs485), fiber), next);
                }
            }
        }
    }
}
