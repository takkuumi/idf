#!/usr/bin/env python3
"""Run the actual complete-BLE-request scheduler on the host."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix="imc-ble-request-slots-") as directory:
    p = Path(directory)
    (p / "tests.rs").write_text(f'#[path = "{ROOT / "src/ble_at/request_slots.rs"}"] mod request_slots;\n')
    subprocess.run(["rustc", "+stable", "--edition=2024", "--test", str(p / "tests.rs"), "-o", str(p / "tests")], check=True)
    subprocess.run([str(p / "tests"), "--test-threads=1"], check=True, timeout=60)
