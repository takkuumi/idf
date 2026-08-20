//! Per-port Modbus RTU runtime.
//!
//! MCA configures each physical RS485 independently as master or slave. Each
//! worker below exclusively owns one UART and reopens it when the immutable
//! SystemConfig snapshot changes, so master/slave can never contend for the
//! same driver.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::rtu_master;
use crate::modbus::shared::BusBackend;
use crate::rs485::{Rs485Config, Rs485Port};

static STARTED: AtomicBool = AtomicBool::new(false);
static PORT_STARTED: [AtomicBool; 3] = [const { AtomicBool::new(false) }; 3];
static PORT0_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-port0", 30);
static PORT1_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-port1", 30);
static PORT2_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-port2", 30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortMode {
    Master,
    Slave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RuntimeConfig {
    wire: Rs485Config,
    mode: PortMode,
    slave_addr: u8,
    retry_count: u32,
    timeout_ms: u64,
    interval_ms: u64,
}

fn runtime_config(index: usize) -> Option<RuntimeConfig> {
    crate::bus::config_state::config_read_with(|state| {
        let saved = state.cfg.rs485.get(index)?;
        let wire = Rs485Config::from_port_with_saved(index, saved)?;
        Some(apply_runtime_policy(
            index,
            saved,
            wire,
            crate::rs485::dip::boot_address(),
        ))
    })
    .flatten()
}

fn apply_runtime_policy(
    index: usize,
    saved: &crate::device::system_config::Rs485Config,
    mut wire: Rs485Config,
    boot_dip_address: u8,
) -> RuntimeConfig {
    let dip_address = (index == 0 && boot_dip_address != 0).then_some(boot_dip_address);
    if dip_address.is_some() {
        wire.baud = 9600;
        wire.data_bits = 8;
        wire.parity = 'N';
        wire.stop_bits = 1;
    }
    RuntimeConfig {
        wire,
        // The MCA register contract only distinguishes internal mode 0
        // (master) from every other value (slave).
        mode: if dip_address.is_none() && saved.mode == 0 {
            PortMode::Master
        } else {
            PortMode::Slave
        },
        slave_addr: dip_address.unwrap_or(saved.slave_addr.clamp(1, 247)),
        retry_count: u32::from(saved.retry_count.min(5)),
        timeout_ms: u64::from(saved.timeout_ms.clamp(20, 5000)),
        interval_ms: u64::from(saved.interval_ms.clamp(20, 5000)),
    }
}

fn heartbeat(index: usize) -> &'static TaskHb {
    match index {
        0 => &PORT0_HB,
        1 => &PORT1_HB,
        _ => &PORT2_HB,
    }
}

fn stack_size(index: usize) -> usize {
    match index {
        0 => crate::safety::stack_budget::MODBUS_RTU_PORT0,
        1 => crate::safety::stack_budget::MODBUS_RTU_PORT1,
        _ => crate::safety::stack_budget::MODBUS_RTU_PORT2,
    }
}

fn spawn_port(index: usize) -> AppResult<()> {
    let started = PORT_STARTED.get(index).ok_or_else(|| {
        crate::error::AppError::Modbus(format!("invalid RS485 port index {index}"))
    })?;
    if started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    let hb = heartbeat(index);
    let name = match index {
        0 => "mb-rtu-port0",
        1 => "mb-rtu-port1",
        _ => "mb-rtu-port2",
    };
    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name(name.into())
        .stack_size(stack_size(index))
        .spawn(move || port_loop(index, hb));
    health::reset_thread_core();
    if let Err(error) = result {
        started.store(false, Ordering::Release);
        return Err(crate::error::AppError::Modbus(format!(
            "spawn {name}: {error}"
        )));
    }
    health::register_with_stack(hb, stack_size(index));
    Ok(())
}

pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    spawn_port(0)?;
    spawn_port(1)?;
    if crate::config::modbus::rtu_port2::ENABLED {
        spawn_port(2)?;
    } else {
        log::warn!(
            "[modbus-rtu] RS485-3 runtime is implemented but disabled: UART0 is the active console"
        );
    }
    STARTED.store(true, Ordering::Release);
    Ok(())
}

fn port_loop(index: usize, hb: &'static TaskHb) {
    health::subscribe_wdt();
    let backend = BusBackend;
    let mut active = None;
    let mut port: Option<Rs485Port> = None;
    let mut poll_table = heapless::Vec::<rtu_master::PollItem, 64>::new();
    let mut last_warn = std::time::Instant::now() - Duration::from_secs(10);

    loop {
        hb.tick();
        health::feed_wdt();
        let Some(desired) = runtime_config(index) else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        if active != Some(desired) {
            port.take();
            match Rs485Port::open(&desired.wire) {
                Ok(opened) => {
                    log::info!(
                        "[modbus-rtu] RS485-{} switched to {:?}, addr={}, {}bps",
                        index + 1,
                        desired.mode,
                        desired.slave_addr,
                        desired.wire.baud
                    );
                    port = Some(opened);
                    active = Some(desired);
                }
                Err(error) => {
                    active = None;
                    if last_warn.elapsed() >= Duration::from_secs(10) {
                        log::warn!("[modbus-rtu] RS485-{} reopen failed: {error}", index + 1);
                        last_warn = std::time::Instant::now();
                    }
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
            }
        }

        let Some(opened) = port.as_mut() else {
            continue;
        };
        match desired.mode {
            PortMode::Master => {
                match rtu_master::load_poll_table() {
                    Ok(updated) if updated != poll_table => {
                        rtu_master::publish_result_ranges(&updated);
                        poll_table = updated;
                    }
                    Ok(_) => {}
                    Err(error) if last_warn.elapsed() >= Duration::from_secs(10) => {
                        log::warn!("[modbus-rtu] ignored incomplete 2300+ configuration: {error}");
                        last_warn = std::time::Instant::now();
                    }
                    Err(_) => {}
                }
                for item in poll_table
                    .iter()
                    .copied()
                    .filter(|item| usize::from(item.port) == index + 1)
                {
                    hb.tick();
                    health::feed_wdt();
                    if let Err(error) = rtu_master::poll_with_retry(
                        opened,
                        item,
                        desired.retry_count,
                        desired.timeout_ms,
                    ) {
                        rtu_master::clear_result(item);
                        if last_warn.elapsed() >= Duration::from_secs(10) {
                            log::warn!(
                                "[modbus-rtu] RS485-{} slave={} fc={:02x} failed: {error}",
                                index + 1,
                                item.slave,
                                item.func
                            );
                            last_warn = std::time::Instant::now();
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(desired.interval_ms));
            }
            PortMode::Slave => {
                let mut frame = [0u8; 256];
                match opened.read(&mut frame, 250) {
                    Ok(0) => {}
                    Ok(length) => {
                        if let Err(error) = crate::modbus::rtu_slave::handle_request(
                            opened,
                            &backend,
                            &frame[..length],
                            desired.slave_addr,
                            index,
                        ) && last_warn.elapsed() >= Duration::from_secs(10)
                        {
                            log::warn!("[modbus-rtu] RS485-{} slave error: {error}", index + 1);
                            last_warn = std::time::Instant::now();
                        }
                    }
                    Err(error) => {
                        if last_warn.elapsed() >= Duration::from_secs(10) {
                            log::warn!("[modbus-rtu] RS485-{} read error: {error}", index + 1);
                            last_warn = std::time::Instant::now();
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_master_modes_follow_mca_slave_semantics() {
        let mode = |saved: u8| {
            if saved == 0 {
                PortMode::Master
            } else {
                PortMode::Slave
            }
        };
        assert_eq!(mode(0), PortMode::Master);
        assert_eq!(mode(1), PortMode::Slave);
        assert_eq!(mode(2), PortMode::Slave);
    }

    #[test]
    fn each_physical_port_has_a_distinct_uart() {
        let first = Rs485Config::from_port(0).unwrap();
        let second = Rs485Config::from_port(1).unwrap();
        let third = Rs485Config::from_port(2).unwrap();
        assert_ne!(first.uart_port, second.uart_port);
        assert_ne!(first.uart_port, third.uart_port);
        assert_ne!(second.uart_port, third.uart_port);
    }

    #[test]
    fn dip_override_remains_authoritative_after_runtime_config_changes() {
        let mut saved = crate::device::system_config::Rs485Config {
            baudrate: 115_200,
            data_bits: 7,
            stop_bits: 2,
            parity: 2,
            slave_addr: 247,
            mode: 0,
            retry_count: 99,
            timeout_ms: 1,
            interval_ms: u16::MAX,
        };
        let wire = Rs485Config::from_port_with_saved(0, &saved).unwrap();
        let runtime = apply_runtime_policy(0, &saved, wire, 9);
        assert_eq!(runtime.mode, PortMode::Slave);
        assert_eq!(runtime.slave_addr, 9);
        assert_eq!(runtime.wire.baud, 9600);
        assert_eq!(runtime.wire.data_bits, 8);
        assert_eq!(runtime.wire.parity, 'N');
        assert_eq!(runtime.wire.stop_bits, 1);
        assert_eq!(runtime.retry_count, 5);
        assert_eq!(runtime.timeout_ms, 20);
        assert_eq!(runtime.interval_ms, 5000);

        saved.mode = 1;
        let wire = Rs485Config::from_port_with_saved(1, &saved).unwrap();
        let runtime = apply_runtime_policy(1, &saved, wire, 9);
        assert_eq!(runtime.slave_addr, 247, "DIP only governs RS485-1");
        assert_eq!(runtime.wire.baud, 115_200);
    }
}
