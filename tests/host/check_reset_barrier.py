#!/usr/bin/env python3
"""Keep persistent RAM publications and dirty markers inside the reset barrier."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
backends = (ROOT / "src/bus/backends.rs").read_text()
main = (ROOT / "src/main.rs").read_text()
web = (ROOT / "src/web/mod.rs").read_text()
ble_handlers = (ROOT / "src/ble_at/cfg_handlers.rs").read_text()
ble = (ROOT / "src/ble_at/mod.rs").read_text()


def block_after(text: str, marker: str) -> str:
    start = text.index(marker)
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


write_holds = block_after(backends, "pub fn write_hold_regs")
batch_config = block_after(write_holds, "if addr >= regs::HOLD_CFG_BASE")
write_lock = batch_config.index("let _guard = rcu_write_guard();")
assert batch_config.index("CONFIG.write(cs)", write_lock) < batch_config.index(
    "request_persist_config()", write_lock
)
assert batch_config.index("request_persist_config()", write_lock) < batch_config.index(
    "\n        }", write_lock
), "config dirty marker must be set before releasing the write barrier"

batch_text = block_after(write_holds, "if addr >= regs::DEVICE_TEXT_BASE")
assert batch_text.index("STORAGE.write(snap)") < batch_text.index(
    "mark_device_text_dirty()"
)
assert batch_text.index("mark_device_text_dirty()") < batch_text.index(
    "\n        }", batch_text.index("let _guard = rcu_write_guard();")
), "text dirty marker must be set before releasing the write barrier"

single_writer = block_after(backends, "fn write_hold_reg_locked")
single_text = block_after(
    single_writer,
    "if (regs::DEVICE_TEXT_BASE..=regs::DEVICE_TEXT_END).contains(&addr)",
)
assert single_text.index("STORAGE.write(snap)") < single_text.index("mark_device_text_dirty()")

reset = main[main.index("let reset_request ="):main.index("invalid reset request marker")]
assert reset.index("configuration_write_guard()") < reset.index("flush_pending_before_reset()")
assert reset.index("flush_pending_before_reset()") < reset.index("esp_restart()")

for source, label in ((web, "web"), (ble_handlers, "BLE AT handlers"), (ble, "BLE protocol")):
    assert "config_modify(" not in source, f"{label} must use the persistent/configured write API"

persistent_api = block_after(backends, "pub fn config_modify_persistent")
assert "request_persist_config()" in persistent_api

print("reset barrier source contracts: OK")
