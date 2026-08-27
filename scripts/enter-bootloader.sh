#!/usr/bin/env bash
set -Eeuo pipefail

PORT="${1:-${ESPFLASH_PORT:-/dev/cu.usbserial-1430}}"

# This CH340 board needs both control-line polarities before the ESP32-S3 ROM
# downloader reliably sees GPIO0 low during reset. The following flasher must
# reconnect with no-reset so it does not undo the bootloader state.
python3 - "$PORT" <<'PY'
import serial
import sys
import time

port = sys.argv[1]
sequences = (
    ((False, False), (True, False), (False, True), (False, False)),
    ((True, True), (False, True), (True, False), (False, False)),
)
for sequence in sequences:
    handle = serial.Serial(port, 115200, timeout=0.05)
    for dtr, rts in sequence:
        handle.dtr = dtr
        handle.rts = rts
        time.sleep(0.1)
    handle.close()
    time.sleep(0.2)
PY
