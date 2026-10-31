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
use crate::sync::{AtomicBits64, Spin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::digital_io::DigitalIo;
use super::sw_i2c::SwI2c;

static PCA9555_TASK_HB: crate::health::TaskHb =
    crate::health::TaskHb::new_with_stall("pca9555-i2c", 10);

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
    worker: Arc<Pca9555WorkerState>,
}

struct Pca9555WorkerState {
    alive: AtomicBool,
    di_bits: AtomicBits64,
    do_desired: AtomicBits64,
    do_hardware: AtomicBits64,
    #[cfg(feature = "f4")]
    status_led: AtomicU16,
    #[cfg(not(feature = "f4"))]
    status_led: AtomicU16,
}

#[cfg(feature = "f4")]
fn pack_status_led(status: (u8, u8)) -> u16 {
    u16::from(status.0) | (u16::from(status.1) << 8)
}

#[cfg(not(feature = "f4"))]
fn pack_status_led(status: u8) -> u16 {
    u16::from(status)
}

#[cfg(feature = "f4")]
fn unpack_status_led(value: u16) -> (u8, u8) {
    (value as u8, (value >> 8) as u8)
}

#[cfg(not(feature = "f4"))]
fn unpack_status_led(value: u16) -> u8 {
    value as u8
}

fn pca9555_worker(mut io_bus: BusI2c, mut led_bus: BusI2c, state: Arc<Pca9555WorkerState>) {
    crate::health::subscribe_wdt();
    let mut last_io = [(0xFF, 0xFF); MAX_CHIPS];
    let mut last_led = [(0xFF, 0xFF); MAX_CHIPS];
    let mut last_di_led = [(0xFF, 0xFF); MAX_CHIPS];
    let mut last_do = u64::MAX;
    let mut last_status = u16::MAX;
    let mut last_warn = Instant::now() - Duration::from_secs(10);

    loop {
        PCA9555_TASK_HB.tick();
        crate::health::feed_wdt();

        if let Err(error) = worker_sample_di(&io_bus, &mut led_bus, &mut last_di_led, &state)
            && last_warn.elapsed() >= Duration::from_secs(10)
        {
            log::warn!("[pca9555] worker DI transaction failed: {error}");
            last_warn = Instant::now();
        }

        let desired_do = state.do_desired.load_bits();
        if desired_do != last_do {
            match worker_apply_do(
                &mut io_bus,
                &mut led_bus,
                desired_do,
                &mut last_io,
                &mut last_led,
            ) {
                Ok(()) => {
                    last_do = desired_do;
                    state.do_hardware.store_bits(desired_do);
                }
                Err(error) if last_warn.elapsed() >= Duration::from_secs(10) => {
                    log::warn!("[pca9555] worker DO transaction failed: {error}");
                    last_warn = Instant::now();
                }
                Err(_) => {}
            }
        }

        let desired_status = state.status_led.load(Ordering::Acquire);
        if desired_status != last_status {
            match worker_apply_status(&mut led_bus, desired_status) {
                Ok(()) => last_status = desired_status,
                Err(error) if last_warn.elapsed() >= Duration::from_secs(10) => {
                    log::warn!("[pca9555] worker status LED transaction failed: {error}");
                    last_warn = Instant::now();
                }
                Err(_) => {}
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn worker_sample_di(
    io_bus: &BusI2c,
    led_bus: &mut BusI2c,
    last_di_led: &mut [(u8, u8); MAX_CHIPS],
    state: &Pca9555WorkerState,
) -> AppResult<()> {
    let mut result = 0u64;
    for (index, (&io_addr, &led_addr)) in DI_IO_ADDRS.iter().zip(DI_LED_ADDRS).enumerate() {
        let raw = io_bus.read_regs2(io_addr, REG_INPUT_0)?;
        if last_di_led[index] != (raw[0], raw[1]) {
            led_bus.write_regs2(led_addr, REG_OUTPUT_0, raw)?;
            last_di_led[index] = (raw[0], raw[1]);
        }
        let bits = (!raw[0] as u64) | ((bit_reverse(raw[1]) ^ 0xFF) as u64) << 8;
        result |= bits << (index * 16);
    }
    state.di_bits.store_bits(result);
    Ok(())
}

fn worker_apply_do(
    io_bus: &mut BusI2c,
    led_bus: &mut BusI2c,
    value: u64,
    last_io: &mut [(u8, u8); MAX_CHIPS],
    last_led: &mut [(u8, u8); MAX_CHIPS],
) -> AppResult<()> {
    for (index, (&io_addr, &led_addr)) in DO_IO_ADDRS.iter().zip(DO_LED_ADDRS).enumerate() {
        let raw = ((value >> (index * 16)) & 0xFFFF) as u16;
        let lo = raw as u8;
        let hi = (raw >> 8) as u8;
        let io = [bit_reverse(lo), bit_reverse(hi)];
        let led = [!lo, !bit_reverse(hi)];
        if last_io[index] != (io[0], io[1]) {
            io_bus.write_regs2(io_addr, REG_OUTPUT_0, io)?;
            last_io[index] = (io[0], io[1]);
        }
        if last_led[index] != (led[0], led[1]) {
            led_bus.write_regs2(led_addr, REG_OUTPUT_0, led)?;
            last_led[index] = (led[0], led[1]);
        }
    }
    Ok(())
}

fn worker_apply_status(led_bus: &mut BusI2c, packed: u16) -> AppResult<()> {
    #[cfg(feature = "f4")]
    {
        let status = unpack_status_led(packed);
        led_bus.write_regs2(STATUS_LED_ADDR, REG_OUTPUT_0, [status.0, status.1])
    }
    #[cfg(not(feature = "f4"))]
    {
        led_bus.write_reg(STATUS_LED_ADDR, REG_OUTPUT_0, unpack_status_led(packed))
    }
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
        let worker = Arc::new(Pca9555WorkerState {
            alive: AtomicBool::new(true),
            di_bits: AtomicBits64::new(0),
            do_desired: AtomicBits64::new(0),
            do_hardware: AtomicBits64::new(0),
            status_led: AtomicU16::new(pack_status_led(status_led)),
        });
        let worker_state = Arc::clone(&worker);
        // Keep the bit-bang writer off the main/network CPU. AtomicBits64 readers
        // can otherwise observe the writer's odd seqlock while it is preempted
        // on the same core, starving the main loop under TCP load.
        crate::health::set_next_thread_core(crate::health::CORE_RT);
        let spawn = thread::Builder::new()
            .name("pca9555-i2c".into())
            .stack_size(crate::safety::stack_budget::PCA9555_I2C)
            .spawn(move || pca9555_worker(io_bus, led_bus, worker_state));
        crate::health::reset_thread_core();
        if let Err(error) = spawn {
            worker.alive.store(false, Ordering::Release);
            return Err(AppError::Io(format!("spawn pca9555 worker: {error}")));
        }
        crate::health::register_with_stack(
            &PCA9555_TASK_HB,
            crate::safety::stack_budget::PCA9555_I2C,
        );
        Ok(Self { worker })
    }

    pub fn set_bt_led(&self, connected: bool) -> AppResult<()> {
        #[cfg(feature = "f4")]
        {
            self.update_status_led(|current| {
                let status = unpack_status_led(current);
                let next = if connected {
                    (status.0 & STATUS_LED_BT_F4, status.1)
                } else {
                    (status.0 | !STATUS_LED_BT_F4, status.1)
                };
                pack_status_led(next)
            });
        }
        #[cfg(not(feature = "f4"))]
        {
            self.update_status_led(|current| {
                with_status_led(unpack_status_led(current), STATUS_LED_BT, connected) as u16
            });
        }
        Ok(())
    }

    pub fn set_port_leds(&self, rs485: [bool; 2], fiber: [bool; 2]) -> AppResult<()> {
        #[cfg(feature = "f4")]
        {
            self.update_status_led(|current| {
                let status = unpack_status_led(current);
                let mut next = status;
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
                pack_status_led(next)
            });
        }
        #[cfg(not(feature = "f4"))]
        {
            self.update_status_led(|current| {
                with_fiber_leds(with_rs485_leds(unpack_status_led(current), rs485), fiber) as u16
            });
        }
        Ok(())
    }

    fn update_status_led(&self, update: impl Fn(u16) -> u16) {
        let mut current = self.worker.status_led.load(Ordering::Acquire);
        loop {
            let next = update(current);
            match self.worker.status_led.compare_exchange(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
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
        self.worker_alive()?;
        Ok(self.worker.di_bits.load_bits())
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
        self.worker_alive()?;
        if self.worker.do_desired.load_bits() == value {
            return Ok(());
        }
        self.worker.do_desired.store_bits(value);
        Ok(())
    }

    fn write_do(&self, idx: usize, on: bool) -> AppResult<()> {
        if idx >= self.do_count() {
            return Err(AppError::Io(format!("do idx {idx} out of range")));
        }
        let current = self.worker.do_desired.load_bits();
        let value = if on {
            current | (1u64 << idx)
        } else {
            current & !(1u64 << idx)
        };
        self.write_do_all(value)
    }

    fn read_do_cached(&self) -> u64 {
        self.worker.do_desired.load_bits()
    }

    fn read_do_actual(&self) -> AppResult<u64> {
        self.worker_alive()?;
        Ok(self.worker.do_hardware.load_bits())
    }
}

impl Pca9555Duo {
    fn worker_alive(&self) -> AppResult<()> {
        if self.worker.alive.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(AppError::Io("pca9555 worker stopped".into()))
        }
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
