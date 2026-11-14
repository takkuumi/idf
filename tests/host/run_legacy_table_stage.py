#!/usr/bin/env python3
"""Execute actual BLE table-stage tests, including bounded metadata and limits."""
from pathlib import Path
import re
import subprocess
import tempfile
from run_production_core import item

ROOT = Path(__file__).resolve().parents[2]
logic = (ROOT / "src/ble_at/logic_handlers.rs").read_text()
constants = "\n".join(re.findall(r"const LEGACY_TABLE_[A-Z_]+:.*?;", logic))
parts = [
    constants,
    item(logic, "pub enum LegacyTableCommitError"),
    item(logic, "struct LegacyTableStage"),
    item(logic, "impl LegacyTableStage"),
]
parts[1] = "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n" + parts[1]
tests = [
    "staged_legacy_table_waits_for_every_word_before_publish",
    "staged_legacy_table_rejects_out_of_order_pointers_and_foreign_connection",
    "staged_legacy_table_accepts_identical_retry_and_rejects_conflicting_retry",
    "empty_legacy_table_header_is_a_complete_transaction",
    "test_staged_table_metadata_does_not_embed_the_upload_payload",
    "test_staged_table_maximum_payload_is_bounded",
    "test_staged_table_timeout_preserves_the_pending_words",
]
parts += ["#[test]\n" + item(logic, "fn " + name + "(") for name in tests]
source = "#![allow(dead_code)]\nuse std::time::{Duration, Instant};\n" + "\n".join(parts)

sdk = (ROOT / "sdkconfig.defaults").read_text()
budget = (ROOT / "src/safety/stack_budget.rs").read_text()
actual = int(re.search(r"^CONFIG_ESP_MAIN_TASK_STACK_SIZE=(\d+)$", sdk, re.M).group(1))
declared = int(re.search(r"pub const MAIN: usize = (\d+) \* KIB;", budget).group(1)) * 1024
assert actual == declared, f"main stack budget mismatch: sdk={actual}, declared={declared}"

with tempfile.TemporaryDirectory(prefix="imc-legacy-table-stage-") as directory:
    path = Path(directory)
    (path / "tests.rs").write_text(source)
    subprocess.run(["rustc", "+stable", "--edition=2024", "--test", str(path / "tests.rs"), "-o", str(path / "tests")], check=True)
    subprocess.run([str(path / "tests"), "--test-threads=1"], check=True, timeout=30)
print(f"Main stack budget matches sdkconfig: {actual} bytes")
