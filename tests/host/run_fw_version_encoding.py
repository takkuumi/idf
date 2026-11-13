#!/usr/bin/env python3
"""Exercise the production firmware version encoder/decoder on host Rust."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "src/device/system_config.rs").read_text()


def item(marker: str) -> str:
    start = SOURCE.index(marker)
    opening = SOURCE.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (SOURCE[end] == "{") - (SOURCE[end] == "}")
        end += 1
    return SOURCE[start:end]


program = """
#![allow(dead_code)]
struct SystemConfig;
impl SystemConfig {
""" + item("pub fn encode_fw_version(") + item("pub fn decode_fw_version(") + """
}
#[test]
fn legacy_version_code_stays_compatible() {
    assert_eq!(SystemConfig::encode_fw_version(2, 9, 0), 290);
    assert_eq!(SystemConfig::decode_fw_version(290), (2, 9, 0));
}
#[test]
fn extended_version_code_roundtrips_two_digit_minor() {
    let code = SystemConfig::encode_fw_version(2, 10, 0);
    assert_eq!(code, 0xF2A0);
    assert_eq!(SystemConfig::decode_fw_version(code), (2, 10, 0));
}
#[test]
fn extended_version_code_roundtrips_two_digit_patch() {
    let code = SystemConfig::encode_fw_version(2, 9, 12);
    assert_eq!(SystemConfig::decode_fw_version(code), (2, 9, 12));
}
#[test]
fn unrepresentable_version_segments_fail_closed() {
    assert_eq!(SystemConfig::encode_fw_version(16, 0, 0), u16::MAX);
}
"""

with tempfile.TemporaryDirectory(prefix="imc-fw-version-") as temporary:
    test_source = Path(temporary) / "tests.rs"
    binary = Path(temporary) / "tests"
    test_source.write_text(program)
    subprocess.run(
        ["rustc", "+stable", "--edition=2024", "--test", str(test_source), "-o", str(binary)],
        check=True,
    )
    subprocess.run([str(binary), "--test-threads=1"], check=True, timeout=30)
