#!/usr/bin/env python3
"""Execute actual pure production code on the host; no ESP-IDF runner/flash."""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def item(text: str, marker: str) -> str:
    """Extract one brace-balanced Rust item with a unique signature."""
    start = text.index(marker)
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


def main() -> None:
    control = (ROOT / "src/control_logic.rs").read_text()
    master = (ROOT / "src/modbus/rtu_master.rs").read_text()
    output = (ROOT / "src/io/do_.rs").read_text()
    health = (ROOT / "src/health.rs").read_text()
    udp = (ROOT / "src/udp_multicast/mod.rs").read_text()
    web = (ROOT / "src/web/mod.rs").read_text()
    calibration = (ROOT / "src/channel/calib.rs").read_text()
    device = (ROOT / "src/device/mod.rs").read_text()
    system_config = (ROOT / "src/device/system_config.rs").read_text()
    wait_statics = "\n".join(re.findall(r"static PERSIST_WAIT_[A-Z_]+:.*?;", device, re.S))
    # The bounded result bank is kept as one contiguous production section.
    # Changes to its boundary deliberately fail extraction instead of silently
    # testing a hand-maintained copy of the implementation.
    bank = master[master.index("const MAX_POLL_ITEMS:"):master.index("fn holding_word(")]
    pure_functions = "\n".join(item(control, "fn " + name + "(") for name in (
        "analog_update_due", "seconds_to_millis", "schedule_fits"))
    pure_tests = "\n".join("#[test]\n" + item(control, "fn " + name + "(") for name in (
        "test_analog_period_survives_millisecond_wrap",
        "test_configured_seconds_are_not_saturated_to_65_seconds",
        "test_pulse_requires_capacity_for_both_edges"))
    do_test = "#[test]\n" + item(output, "fn test_do_failure_waits_full_retry_interval_despite_dirty_flag(")
    source = f'''
// This harness includes unchanged production modules/sections. Some constants
// belong to the device-only polling loop and are intentionally unused here.
#![allow(dead_code)]
#[path = "{ROOT / 'src/config.rs'}"] mod config;
mod bus {{
    #[path = "{ROOT / 'src/bus/register_snapshot.rs'}"] pub(crate) mod register_snapshot;
}}
#[path = "{ROOT / 'src/device/pending_save.rs'}"] mod pending_save;
mod control {{
    const MAX_SCHEDULED: usize = 64;
    {pure_functions}
    {pure_tests}
}}
mod output {{
    const RETRY_TICKS: u32 = 100 / crate::config::MAIN_LOOP_PERIOD_MS as u32;
    {item(output, 'fn retry_pending(')}
    {do_test}
}}
mod result_bank {{
    use std::sync::Mutex;
    use std::sync::atomic::{{AtomicU16, AtomicU32, Ordering}};
    {bank}
    #[test]
    fn test_actual_result_bank_coherence_snapshot_and_writer_contention() {{
        let item = PollItem {{ port: 1, slave: 1, func: 3, start: 4100, count: 2, dest_reg: 4000 }};
        publish_result_ranges(&[item]);
        write_result(item, &[0x1234, 0xEDCB]);
        let original = snapshot_results(4000, 2);
        write_result(item, &[0x5678, 0xA987]);
        assert_eq!(original.get(4000), Some(0x1234));
        assert_eq!(original.get(4001), Some(0xEDCB));
        let mut bytes = [0; 4];
        assert_eq!(encode_result_regs_be(4000, 2, &mut bytes), Some(true));
        assert_eq!(bytes, [0x56, 0x78, 0xA9, 0x87]);

        // Reproduce a producer that has been preempted while holding its write
        // section: capture must wait, not return an illegal-address failure.
        let guard = RESULT_LOCK.lock().unwrap();
        let complete = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {{
            let complete_ref = &complete;
            let reader = scope.spawn(move || {{
                let snapshot = snapshot_results(4000, 2);
                complete_ref.store(true, Ordering::Release);
                assert_eq!(snapshot.get(4000), Some(0x5678));
            }});
            std::thread::sleep(std::time::Duration::from_millis(20));
            assert!(!complete.load(Ordering::Acquire));
            // An unrelated config range does not take the producer mutex.
            assert_eq!(snapshot_results(2280, 16).get(2280), None);
            drop(guard);
            reader.join().unwrap();
        }});
        std::thread::scope(|scope| {{
            let writer = scope.spawn(move || {{
                for value in 0..20_000u16 {{ write_result(item, &[value, !value]); }}
            }});
            for _ in 0..20_000 {{
                let snapshot = snapshot_results(4000, 2);
                assert_eq!(snapshot.get(4001), snapshot.get(4000).map(|v| !v));
            }}
            writer.join().unwrap();
        }});
        clear_result(item);
        assert_eq!(read_result_reg(4000), Some(0));
        assert_eq!(read_result_reg(4001), Some(0));
    }}
}}
mod health {{
    use std::sync::atomic::{{AtomicUsize, Ordering}};
    const MAX_TASKS: usize = 24;
    pub fn feed_wdt() {{}}
    {item(health, 'fn reserve_task_slot(')}
    #[test]
    fn test_registry_reservation_never_exceeds_reader_array_bound() {{
        let count = AtomicUsize::new(0);
        std::thread::scope(|scope| {{
            for _ in 0..8 {{
                let counter = &count;
                scope.spawn(move || {{
                    for _ in 0..1000 {{
                        if let Some(slot) = reserve_task_slot(counter) {{ assert!(slot < MAX_TASKS); }}
                        assert!(counter.load(Ordering::Acquire) <= MAX_TASKS);
                    }}
                }});
            }}
        }});
        assert_eq!(count.load(Ordering::Acquire), MAX_TASKS);
    }}
}}
mod udp {{
    use std::time::{{Duration, Instant}};
    const STATUS_FRESHNESS: Duration = Duration::from_secs(5);
    {item(udp, 'fn packet_is_fresh(')}
    #[test]
    {item(udp, 'fn test_receiving_status_expires_and_does_not_accept_future_time(')}
}}
mod web {{
    {item(web, 'fn parse_io_control_fields(')}
    {item(web, 'fn parse_sensor_fields(')}
    {item(web, 'fn valid_web_password(')}
    #[test]
    {item(web, 'fn test_malformed_io_fields_cannot_default_to_output_zero(')}
    #[test]
    {item(web, 'fn test_sensor_fields_require_explicit_valid_calibration(')}
    #[test]
    {item(web, 'fn test_web_password_must_fit_reload_buffer(')}
}}
mod calibration {{
    const SENSOR_CHANNELS: usize = 8;
    {item(calibration, 'fn calibration_channel_count(')}
    #[test]
    {item(calibration, 'fn test_calibration_ignores_legacy_slots_without_physical_channels(')}
}}
mod tcp_ports {{
    {item(system_config, 'pub(crate) fn valid_tcp_ports(')}
    #[test]
    {item(system_config, 'fn test_tcp_ports_reject_duplicate_zero_and_web_conflicts(')}
}}
mod storage_confirmation {{
    use std::time::{{Duration, Instant}};
    #[derive(Debug)]
    enum AppError {{ Config(String) }}
    type AppResult<T> = Result<T, AppError>;
    enum DeviceCmd {{ FlushPending {{ ticket: u32 }} }}
    {wait_statics}
    struct PersistWaitGuard;
    {item(device, 'impl Drop for PersistWaitGuard')}
    {item(device, 'pub fn await_pending_persistence(')}
    static SEND_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    fn try_send_device_cmd(DeviceCmd::FlushPending {{ ticket }}: DeviceCmd) -> bool {{
        use std::sync::atomic::Ordering;
        match SEND_MODE.load(Ordering::Acquire) {{
            1 => false,
            3 => true, // queued, but deliberately never acknowledged
            mode => {{
                PERSIST_WAIT_OK.store(mode != 2, Ordering::Release);
                PERSIST_WAIT_COMPLETED.store(ticket, Ordering::Release);
                true
            }}
        }}
    }}
    #[test]
    fn test_real_storage_confirmation_success_busy_failure_and_timeout() {{
        use std::sync::atomic::Ordering;
        assert!(await_pending_persistence().is_ok());
        PERSIST_WAIT_BUSY.store(true, Ordering::Release);
        assert!(await_pending_persistence().is_err());
        assert!(PERSIST_WAIT_BUSY.load(Ordering::Acquire));
        PERSIST_WAIT_BUSY.store(false, Ordering::Release);
        for mode in [1, 2, 3] {{
            SEND_MODE.store(mode, Ordering::Release);
            let started = Instant::now();
            assert!(await_pending_persistence().is_err(), "send mode {{mode}}");
            assert!(!PERSIST_WAIT_BUSY.load(Ordering::Acquire), "busy gate leaked after {{mode}}");
            assert!(started.elapsed() < Duration::from_secs(9));
        }}
        SEND_MODE.store(0, Ordering::Release);
        assert!(await_pending_persistence().is_ok());
    }}
}}
'''
    version = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M).group(1)
    with tempfile.TemporaryDirectory(prefix="gateway-host-") as temporary:
        directory = Path(temporary)
        path = directory / "production_core.rs"
        path.write_text(source)
        env = dict(os.environ, CARGO_PKG_VERSION=version)
        subprocess.run(["rustc", "+stable", "--edition=2024", "--test", str(path), "-o", str(directory / "tests")], env=env, check=True)
        subprocess.run([str(directory / "tests"), "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
