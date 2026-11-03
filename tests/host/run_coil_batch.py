#!/usr/bin/env python3
"""Exercise the production FC01/FC02 bulk coil mapper on the host."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "src/bus/backends.rs").read_text()


def item(text: str, marker: str) -> str:
    start = text.index(marker)
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


helpers = "\n".join(
    item(SOURCE, marker)
    for marker in ("fn read_coil_from_snapshot(", "fn encode_coil_bits_from_snapshot(")
)
program = f"""
#![allow(dead_code)]
mod config {{
    pub mod hw_version {{ pub const DI_COUNT: usize = 16; pub const DO_COUNT: usize = 16; }}
    pub mod regs {{
        pub const LEGACY_POINT_WINDOW_COUNT: u16 = 48;
        pub const COIL_DO_BASE: u16 = 0x0200;
        pub const COIL_DO_END: u16 = COIL_DO_BASE + 16;
        pub const COIL_INTERNAL_START: u16 = 0x0400;
        pub const COIL_LOGIC_RESTART: u16 = 0x0403;
        pub const LEGACY_COIL_BASE: u16 = 0x0200;
        pub const LEGACY_COIL_END: u16 = 0x07ff;
    }}
}}
use config::{{hw_version, regs}};
struct StorageSnapshot {{ legacy_coils: Vec<u8> }}
{helpers}

#[test]
fn packed_mapping_matches_each_legacy_address() {{
    let mut storage = StorageSnapshot {{ legacy_coils: vec![0; 1536] }};
    storage.legacy_coils[700 - regs::LEGACY_COIL_BASE as usize] = 1;
    let di = 0xA55Au64;
    let do_bits = 0xD39Cu64;
    for (start, count) in [(0, 48), (40, 80), (0x1F0, 96), (0x2F0, 48), (700, 32)] {{
        let mut packed = vec![0xFF; usize::from(count).div_ceil(8)];
        assert!(encode_coil_bits_from_snapshot(start, count, &mut packed, Some(&storage), di, do_bits));
        for offset in 0..count {{
            assert_eq!(
                packed[usize::from(offset) / 8] & (1 << (offset % 8)) != 0,
                read_coil_from_snapshot(start + offset, Some(&storage), di, do_bits).unwrap(),
                "address={{}}", start + offset,
            );
        }}
    }}
}}

#[test]
fn missing_snapshot_only_fails_when_persistent_coils_are_requested() {{
    let mut reserved = [0xFF];
    assert!(encode_coil_bits_from_snapshot(48, 8, &mut reserved, None, 0, 0));
    assert_eq!(reserved, [0]);
    let mut persistent = [0xFF];
    assert!(!encode_coil_bits_from_snapshot(560, 8, &mut persistent, None, 0, 0));
}}

#[test]
fn invalid_ranges_are_rejected_before_address_addition() {{
    assert!(!encode_coil_bits_from_snapshot(u16::MAX, 2, &mut [0], None, 0, 0));
    assert!(!encode_coil_bits_from_snapshot(0, 0, &mut [], None, 0, 0));
}}
"""

with tempfile.TemporaryDirectory(prefix="imc-coil-batch-") as directory:
    target = Path(directory)
    source = target / "tests.rs"
    binary = target / "tests"
    source.write_text(program)
    subprocess.run(["rustc", "+stable", "--edition=2024", "--test", str(source), "-o", str(binary)], check=True)
    subprocess.run([str(binary), "--test-threads=1"], check=True, timeout=20)
