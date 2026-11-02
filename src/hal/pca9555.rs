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
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use super::digital_io::DigitalIo;
use super::io_worker_state::{OutputQueue, sample_is_fresh};
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

static WORKER_DIAGNOSTICS: OnceLock<Arc<Pca9555WorkerState>> = OnceLock::new();

fn now_ms() -> u32 {
    (unsafe { esp_idf_sys::esp_timer_get_time() } / 1000) as u32
}

struct Pca9555WorkerState {
    alive: AtomicBool,
    heartbeat_ms: AtomicU32,
    di_bits: AtomicBits64,
    di_generation: AtomicU32,
    di_sampled_ms: AtomicU32,
    di_failures: AtomicU32,
    do_desired: AtomicBits64,
    do_hardware: AtomicBits64,
    do_readback_valid: AtomicBool,
    do_readback_ms: AtomicU32,
    do_failures: AtomicU32,
    do_submit_busy: AtomicU32,
    do_confirmed_generation: AtomicU32,
    outputs: Mutex<OutputQueue>,
    led_errors: AtomicU32,
    loop_max_us: AtomicU32,
    status_led: AtomicU16,
}

/// 诊断为近似时点快照；null 表示尚未初始化或正在争用，不伪造正常值。
pub fn diagnostics_json() -> String {
    let Some(state) = WORKER_DIAGNOSTICS.get() else {
        return "null".into();
    };
    let now = now_ms();
    let generation = state.di_generation.load(Ordering::SeqCst);
    let sampled = state.di_sampled_ms.load(Ordering::SeqCst);
    let failures = state.di_failures.load(Ordering::SeqCst);
    let do_readback_valid = state.do_readback_valid.load(Ordering::Acquire);
    let do_readback_ms = state.do_readback_ms.load(Ordering::Acquire);
    let queue_snapshot = state.outputs.try_lock().ok().map(|q| {
        (
            q.len(),
            q.high_water,
            q.rejected,
            q.canceled_after_fault,
            q.latest().generation,
        )
    });
    // Formatting/heap work is outside the submission lock; Web polling must not
    // lengthen the main-loop try_lock contention window.
    let queue = queue_snapshot
        .map(|(depth, high_water, rejected, canceled, generation)| {
            format!(
                r#"{{"depth":{},"high_water":{},"rejected":{},"canceled_after_fault":{},"requested_generation":{}}}"#,
                depth, high_water, rejected, canceled, generation
            )
        })
        .unwrap_or_else(|| "null".into());
    let bits = state
        .do_hardware
        .try_load_bits(4)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".into());
    format!(
        r#"{{"worker_age_ms":{},"di_valid":{},"di_age_ms":{},"di_generation":{},"di_failures":{},"output_queue":{},"do_readback_valid":{},"do_readback_bits":{},"do_readback_age_ms":{},"do_failures":{},"do_submit_busy":{},"do_confirmed_generation":{},"led_errors":{},"loop_max_us":{}}}"#,
        now.wrapping_sub(state.heartbeat_ms.load(Ordering::Acquire)),
        generation >= 2 && sample_is_fresh(true, now, sampled, failures),
        if generation == 0 {
            "null".into()
        } else {
            now.wrapping_sub(sampled).to_string()
        },
        generation,
        failures,
        queue,
        do_readback_valid && now.wrapping_sub(do_readback_ms) <= 250,
        bits,
        now.wrapping_sub(do_readback_ms),
        state.do_failures.load(Ordering::Acquire),
        state.do_submit_busy.load(Ordering::Relaxed),
        state.do_confirmed_generation.load(Ordering::Acquire),
        state.led_errors.load(Ordering::Relaxed),
        state.loop_max_us.load(Ordering::Relaxed)
    )
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

fn pca9555_worker(io_bus: BusI2c, led_bus: BusI2c, state: Arc<Pca9555WorkerState>) {
    crate::health::subscribe_wdt();
    let initial_now = Instant::now();
    let mut last_warn = initial_now - Duration::from_secs(10);
    let mut last_do_check = initial_now - Duration::from_millis(100);
    let mut retry_at = initial_now;
    let mut io_retry_at = initial_now;
    let mut led_retry_at = initial_now;
    let mut di_led = [[0xFF; 2]; MAX_CHIPS];
    let mut do_led = [[0xFF; 2]; MAX_CHIPS];
    let mut last_status = u16::MAX;
    loop {
        let started = Instant::now();
        state.heartbeat_ms.store(now_ms(), Ordering::Release);
        PCA9555_TASK_HB.tick();
        crate::health::feed_wdt();

        // 输入读取和发布独立于灯总线；LED NACK 不得阻止有效 DI 更新。
        // When every bank is absent, avoid hammering the bit-banged bus every
        // 10ms. A responding bank immediately restores the normal cadence.
        let now = Instant::now();
        let io_due = now >= io_retry_at;
        let mut io_bus_success = false;
        let led_due = now >= led_retry_at;
        if io_due {
            let mut led_failed = false;
            match worker_sample_di(&io_bus) {
                Ok((bits, raw)) => {
                    io_bus_success = true;
                    state.di_generation.fetch_add(1, Ordering::SeqCst);
                    state.di_bits.store_bits(bits);
                    state.di_sampled_ms.store(now_ms(), Ordering::SeqCst);
                    state.di_failures.store(0, Ordering::SeqCst);
                    state.di_generation.fetch_add(1, Ordering::SeqCst);
                    if led_due {
                        for (i, &addr) in DI_LED_ADDRS.iter().enumerate() {
                            if di_led[i] != raw[i] {
                                match led_bus.write_regs2(addr, REG_OUTPUT_0, raw[i]) {
                                    Ok(()) => {
                                        di_led[i] = raw[i];
                                    }
                                    Err(_) => {
                                        state.led_errors.fetch_add(1, Ordering::Relaxed);
                                        led_failed = true;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    state.di_failures.fetch_add(1, Ordering::SeqCst);
                    warn_worker(&mut last_warn, "DI", &error);
                }
            }

            // 一轮最多应用一个输出边沿，保留至少一个 worker 调度间隔；
            // 失败保留队首，按 100ms 重试，禁止 LED 故障阻塞输出确认。
            let command = {
                let mut queue = state.outputs.lock().unwrap_or_else(|e| e.into_inner());
                if state.do_failures.load(Ordering::Acquire) != 0 {
                    queue.retain_latest_after_fault();
                }
                queue.front()
            };
            if let Some(command) = command
                && Instant::now() >= retry_at
            {
                let outcome = worker_write_do(&io_bus, command.bits)
                    .and_then(|()| worker_read_do(&io_bus))
                    .and_then(|actual| {
                        if actual == command.bits {
                            Ok(actual)
                        } else {
                            Err(AppError::Io("DO output register readback mismatch".into()))
                        }
                    });
                match outcome {
                    Ok(actual) => {
                        io_bus_success = true;
                        publish_do_readback(&state, actual);
                        state.do_failures.store(0, Ordering::Release);
                        state
                            .do_confirmed_generation
                            .store(command.generation, Ordering::Release);
                        state
                            .outputs
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .confirm(command);
                        last_do_check = Instant::now();
                    }
                    Err(error) => {
                        state.do_readback_valid.store(false, Ordering::Release);
                        state.do_failures.fetch_add(1, Ordering::AcqRel);
                        retry_at = Instant::now() + Duration::from_millis(100);
                        warn_worker(&mut last_warn, "DO", &error);
                    }
                }
            } else if command.is_none() && last_do_check.elapsed() >= Duration::from_millis(100) {
                last_do_check = Instant::now();
                let target = state
                    .outputs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .latest()
                    .bits;
                match worker_read_do(&io_bus).and_then(|actual| {
                    if actual == target {
                        Ok(actual)
                    } else {
                        // Expander brownout/reset: rewrite and verify instead of trusting a cache.
                        worker_write_do(&io_bus, target)?;
                        let actual = worker_read_do(&io_bus)?;
                        if actual == target {
                            Ok(actual)
                        } else {
                            Err(AppError::Io("DO recovery readback mismatch".into()))
                        }
                    }
                }) {
                    Ok(actual) => {
                        io_bus_success = true;
                        publish_do_readback(&state, actual);
                        state.do_failures.store(0, Ordering::Release);
                    }
                    Err(error) => {
                        state.do_readback_valid.store(false, Ordering::Release);
                        state.do_failures.fetch_add(1, Ordering::AcqRel);
                        warn_worker(&mut last_warn, "DO readback", &error);
                    }
                }
            }
            if led_due && state.do_readback_valid.load(Ordering::Acquire) {
                let actual = state.do_hardware.load_bits();
                for (i, &addr) in DO_LED_ADDRS.iter().enumerate() {
                    let raw = (actual >> (i * 16)) as u16;
                    let led = [!(raw as u8), !bit_reverse((raw >> 8) as u8)];
                    if do_led[i] != led {
                        match led_bus.write_regs2(addr, REG_OUTPUT_0, led) {
                            Ok(()) => {
                                do_led[i] = led;
                            }
                            Err(_) => {
                                state.led_errors.fetch_add(1, Ordering::Relaxed);
                                led_failed = true;
                            }
                        }
                    }
                }
            }
            let status = state.status_led.load(Ordering::Acquire);
            if led_due && status != last_status {
                match worker_apply_status(&led_bus, status) {
                    Ok(()) => {
                        last_status = status;
                    }
                    Err(error) => {
                        state.led_errors.fetch_add(1, Ordering::Relaxed);
                        warn_worker(&mut last_warn, "LED", &error);
                        led_failed = true;
                    }
                }
            }
            if led_due {
                led_retry_at = if led_failed {
                    Instant::now() + Duration::from_millis(500)
                } else {
                    Instant::now()
                };
            }
            if !io_bus_success {
                io_retry_at = Instant::now() + Duration::from_millis(500);
            } else {
                io_retry_at = Instant::now();
            }
        }
        let elapsed = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        // Single writer (this worker); keep atomic loads for external readers.
        let maximum = state.loop_max_us.load(Ordering::Relaxed).max(elapsed);
        state.loop_max_us.store(maximum, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(10));
    }
}

fn warn_worker(last_warn: &mut Instant, phase: &str, error: &AppError) {
    if last_warn.elapsed() >= Duration::from_secs(10) {
        log::warn!("[pca9555] {phase} transaction failed: {error}");
        *last_warn = Instant::now();
    }
}

fn publish_do_readback(state: &Pca9555WorkerState, bits: u64) {
    state.do_readback_valid.store(false, Ordering::SeqCst);
    state.do_hardware.store_bits(bits);
    state.do_readback_ms.store(now_ms(), Ordering::Release);
    state.do_readback_valid.store(true, Ordering::SeqCst);
}

fn worker_sample_di(io_bus: &BusI2c) -> AppResult<(u64, [[u8; 2]; MAX_CHIPS])> {
    let mut result = 0u64;
    let mut samples = [[0; 2]; MAX_CHIPS];
    for (index, &addr) in DI_IO_ADDRS.iter().enumerate() {
        let raw = io_bus.read_regs2(addr, REG_INPUT_0)?;
        samples[index] = raw;
        let bits = u64::from(!raw[0]) | (u64::from(bit_reverse(raw[1]) ^ 0xFF) << 8);
        result |= bits << (index * 16);
    }
    Ok((result, samples))
}

fn worker_write_do(io_bus: &BusI2c, value: u64) -> AppResult<()> {
    for (i, &addr) in DO_IO_ADDRS.iter().enumerate() {
        let raw = (value >> (i * 16)) as u16;
        io_bus.write_regs2(
            addr,
            REG_OUTPUT_0,
            [bit_reverse(raw as u8), bit_reverse((raw >> 8) as u8)],
        )?;
    }
    Ok(())
}

fn worker_read_do(io_bus: &BusI2c) -> AppResult<u64> {
    let mut result = 0u64;
    for (i, &addr) in DO_IO_ADDRS.iter().enumerate() {
        let raw = io_bus.read_regs2(addr, REG_OUTPUT_0)?;
        result |= u64::from(bit_reverse(raw[0])) << (i * 16);
        result |= u64::from(bit_reverse(raw[1])) << (i * 16 + 8);
    }
    Ok(result)
}

fn worker_apply_status(led_bus: &BusI2c, packed: u16) -> AppResult<()> {
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
            if let Err(error) = io_bus
                .write_reg(addr, REG_CONFIG_0, 0xFF)
                .and_then(|()| io_bus.write_reg(addr, REG_CONFIG_1, 0xFF))
            {
                log::warn!(
                    "[pca9555] DI chip 0x{addr:02X} unavailable during init; continuing without that bank: {error}"
                );
            }
        }
        // Set safe output latches before enabling outputs.
        for &addr in DO_IO_ADDRS {
            if let Err(error) = io_bus
                .write_regs2(addr, REG_OUTPUT_0, [0x00, 0x00])
                .and_then(|()| io_bus.write_regs2(addr, REG_CONFIG_0, [0x00, 0x00]))
            {
                log::warn!(
                    "[pca9555] DO chip 0x{addr:02X} unavailable during init; continuing without that bank: {error}"
                );
            }
        }
        // DI/DO mirrors and status LEDs are all active-low outputs.
        for &addr in DI_LED_ADDRS
            .iter()
            .chain(DO_LED_ADDRS)
            .chain([STATUS_LED_ADDR].iter())
        {
            if let Err(error) = led_bus
                .write_regs2(addr, REG_CONFIG_0, [0x00, 0x00])
                .and_then(|()| led_bus.write_regs2(addr, REG_OUTPUT_0, [0xFF, 0xFF]))
            {
                log::warn!(
                    "[pca9555] LED chip 0x{addr:02X} unavailable during init; continuing without that bank: {error}"
                );
            }
        }

        #[cfg(feature = "f4")]
        let status_led = (0xFC, 0xFF); // PWR=ON, RUN=ON; other indicators OFF
        #[cfg(not(feature = "f4"))]
        let status_led = 0xDE; // F3/F16-compatible PWR+RUN state
        #[cfg(feature = "f4")]
        if let Err(error) =
            led_bus.write_regs2(STATUS_LED_ADDR, REG_OUTPUT_0, [status_led.0, status_led.1])
        {
            log::warn!("[pca9555] status LED unavailable during init; continuing: {error}");
        }
        #[cfg(not(feature = "f4"))]
        if let Err(error) = led_bus.write_reg(STATUS_LED_ADDR, REG_OUTPUT_0, status_led) {
            log::warn!("[pca9555] status LED unavailable during init; continuing: {error}");
        }

        log::info!(
            "[pca9555] worker initialized: DI={:?} DO={:?} ({}DI/{}DO; missing boards are degraded)",
            DI_IO_ADDRS,
            DO_IO_ADDRS,
            crate::config::hw_version::DI_COUNT,
            crate::config::hw_version::DO_COUNT
        );
        let worker = Arc::new(Pca9555WorkerState {
            alive: AtomicBool::new(true),
            heartbeat_ms: AtomicU32::new(now_ms()),
            di_generation: AtomicU32::new(0),
            di_sampled_ms: AtomicU32::new(0),
            di_failures: AtomicU32::new(0),
            do_readback_valid: AtomicBool::new(false),
            do_readback_ms: AtomicU32::new(0),
            do_failures: AtomicU32::new(0),
            do_submit_busy: AtomicU32::new(0),
            do_confirmed_generation: AtomicU32::new(0),
            outputs: Mutex::new(OutputQueue::new()),
            led_errors: AtomicU32::new(0),
            loop_max_us: AtomicU32::new(0),
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
        let _ = WORKER_DIAGNOSTICS.set(Arc::clone(&worker));
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
        self.read_di_sample()?
            .map(|(bits, _)| bits)
            .ok_or_else(|| AppError::Io("DI publication busy".into()))
    }

    fn read_di_sample(&self) -> AppResult<Option<(u64, Option<u32>)>> {
        self.worker_alive()?;
        for _ in 0..4 {
            let generation = self.worker.di_generation.load(Ordering::SeqCst);
            if generation == 0 || generation & 1 != 0 {
                continue;
            }
            let Some(bits) = self.worker.di_bits.try_load_bits(4) else {
                continue;
            };
            let sampled = self.worker.di_sampled_ms.load(Ordering::SeqCst);
            let failures = self.worker.di_failures.load(Ordering::SeqCst);
            if generation != self.worker.di_generation.load(Ordering::SeqCst) {
                continue;
            }
            if sample_is_fresh(true, now_ms(), sampled, failures) {
                return Ok(Some((bits, Some(generation))));
            }
            return Err(AppError::Io(
                "DI sample stale or hardware read failed".into(),
            ));
        }
        Ok(None)
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
        self.submit_output(|_| value)
    }

    fn write_do(&self, idx: usize, on: bool) -> AppResult<()> {
        if idx >= self.do_count() {
            return Err(AppError::Io(format!("do idx {idx} out of range")));
        }
        self.worker_alive()?;
        self.submit_output(|current| {
            if on {
                current | (1 << idx)
            } else {
                current & !(1 << idx)
            }
        })
    }

    fn read_do_cached(&self) -> u64 {
        self.worker.do_desired.load_bits()
    }

    fn read_do_actual(&self) -> AppResult<u64> {
        self.worker_alive()?;
        if !sample_is_fresh(
            self.worker.do_readback_valid.load(Ordering::SeqCst),
            now_ms(),
            self.worker.do_readback_ms.load(Ordering::Acquire),
            self.worker.do_failures.load(Ordering::Acquire),
        ) {
            return Err(AppError::Io(
                "DO hardware readback unavailable or stale".into(),
            ));
        }
        self.worker
            .do_hardware
            .try_load_bits(8)
            .ok_or_else(|| AppError::Io("DO hardware readback busy".into()))
    }
}

impl Pca9555Duo {
    fn submit_output(&self, update: impl FnOnce(u64) -> u64) -> AppResult<()> {
        let mut queue = match self.worker.outputs.try_lock() {
            Ok(queue) => queue,
            Err(TryLockError::WouldBlock) => {
                self.worker.do_submit_busy.fetch_add(1, Ordering::Relaxed);
                return Err(AppError::Busy("DO queue contended"));
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(AppError::Io("DO queue poisoned".into()));
            }
        };
        let value = update(queue.latest().bits);
        if !queue.submit(value) {
            return Err(AppError::Busy("DO queue full"));
        }
        self.worker.do_desired.store_bits(value);
        Ok(())
    }

    fn worker_alive(&self) -> AppResult<()> {
        if self.worker.alive.load(Ordering::Acquire)
            && now_ms().wrapping_sub(self.worker.heartbeat_ms.load(Ordering::Acquire)) <= 1000
        {
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
