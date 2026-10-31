#!/usr/bin/env python3
"""Run actual IO/RCU/calibration/telemetry sections on the host, without flashing."""
import subprocess
import os
import re
import tempfile
from pathlib import Path
from run_production_core import item

ROOT = Path(__file__).resolve().parents[2]
sync = (ROOT / 'src/sync.rs').read_text()
bits = sync[sync.index('pub struct AtomicBits64'):sync.index('// MpscRing<T, N>')]
# The trailing separator comment is harmless; no hand-maintained copy of algorithms.
calibration = (ROOT / 'src/channel/calib.rs').read_text()
start = calibration.index('storage_modify_holding(|holding| {') + len('storage_modify_holding(|holding| ')
depth = 1
end = start + 1
while depth:
    depth += (calibration[end] == '{') - (calibration[end] == '}')
    end += 1
calibration_callback = calibration[start:end]
bit_tests = '\n'.join('#[test]\n' + item(sync, 'fn ' + name + '(') for name in (
    'test_bits_basic', 'test_bits_bit_ops', 'test_bits_mask_replace',
    'test_bits_bounded_read_detects_preempted_writer',
    'test_bits_32bit_generation_survives_old_u8_wrap', 'test_bits_concurrent'))
source = f'''
#![allow(dead_code)]
extern crate self as log;
#[macro_export] macro_rules! info {{ ($($arg:tt)*) => {{ let _ = format_args!($($arg)*); }}; }}
#[path = "{ROOT / 'src/bus/rcu.rs'}"] mod rcu;
#[path = "{ROOT / 'src/hal/io_worker_state.rs'}"] mod io_worker_state;
#[path = "{ROOT / 'src/channel/calibration_window.rs'}"] mod calibration_window;
#[path = "{ROOT / 'src/telemetry.rs'}"] mod telemetry;
#[path = "{ROOT / 'src/config.rs'}"] mod config;
mod calibration_write {{
    struct Session {{ channel: usize }}
    struct Window {{ raw_min: u16, raw_max: u16 }}
    fn apply(holding: &mut [u16], min_ok: bool, max_ok: bool) -> bool {{
        let session = Session {{ channel: 0 }};
        let window = Window {{ raw_min: 606, raw_max: 3017 }};
        let mut changed = false;
        let mut callback = |holding: &mut [u16]| {calibration_callback};
        callback(holding);
        changed
    }}
    #[test]
    fn test_calibration_does_not_overwrite_concurrent_field_edit() {{
        let min = usize::from(crate::config::regs::HOLD_SENSOR_MIN_BASE - crate::config::regs::HOLD_CFG_BASE);
        let max = usize::from(crate::config::regs::HOLD_SENSOR_MAX_BASE - crate::config::regs::HOLD_CFG_BASE);
        for edited in [min, max] {{
            let mut holding = [0; 256]; holding[edited] = 777;
            assert!(!apply(&mut holding, true, true));
            assert_eq!(holding[edited], 777);
            assert_eq!(holding[if edited == min {{ max }} else {{ min }}], 0);
        }}
        let mut holding = [0; 256];
        assert!(apply(&mut holding, true, false));
        assert_eq!(holding[min], 606);
        assert_eq!(holding[max], 0);
    }}
}}
mod bits {{
 use std::sync::atomic::{{AtomicU16, AtomicU32, Ordering}};
 use std::hint::spin_loop;
 use std::sync::Arc;
 use std::thread;
 {bits}
 {bit_tests}
}}
'''
with tempfile.TemporaryDirectory(prefix='gateway-architecture-tests-') as directory:
    path = Path(directory)
    (path/'tests.rs').write_text(source)
    env = dict(os.environ, CARGO_PKG_VERSION=re.search(r'^version\s*=\s*"([^"]+)"', (ROOT/'Cargo.toml').read_text(), re.M).group(1))
    subprocess.run(['rustc', '--edition=2024', '--test', str(path/'tests.rs'), '-o', str(path/'tests')], check=True, env=env)
    subprocess.run([str(path/'tests'), '--test-threads=4'], check=True, timeout=120)
