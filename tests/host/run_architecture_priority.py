#!/usr/bin/env python3
"""Run actual IO/RCU/calibration/telemetry sections on the host, without flashing."""
import subprocess
import tempfile
from pathlib import Path
from run_production_core import item

ROOT = Path(__file__).resolve().parents[2]
sync = (ROOT / 'src/sync.rs').read_text()
bits = sync[sync.index('pub struct AtomicBits64'):sync.index('// MpscRing<T, N>')]
# The trailing separator comment is harmless; no hand-maintained copy of algorithms.
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
    subprocess.run(['rustc', '--edition=2024', '--test', str(path/'tests.rs'), '-o', str(path/'tests')], check=True)
    subprocess.run([str(path/'tests'), '--test-threads=4'], check=True, timeout=120)
