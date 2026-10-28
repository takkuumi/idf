#!/usr/bin/env python3
"""Read-only UART log capture with reconnect handling for long hardware soaks."""

from __future__ import annotations

import argparse
import sys
import time
from datetime import datetime, timezone

import serial


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", default="/dev/cu.usbserial-1430")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--duration", type=int, default=259200)
    parser.add_argument("--reconnect-delay", type=float, default=5.0)
    args = parser.parse_args()

    deadline = time.monotonic() + args.duration
    errors = 0
    sessions = 0
    print(
        f"serial_start={datetime.now(timezone.utc).isoformat()} port={args.port} baud={args.baud} duration={args.duration}s",
        flush=True,
    )
    while time.monotonic() < deadline:
        try:
            with serial.Serial(args.port, args.baud, timeout=1) as source:
                sessions += 1
                print(
                    f"serial_connected n={sessions} at={datetime.now(timezone.utc).isoformat()}",
                    flush=True,
                )
                while time.monotonic() < deadline:
                    chunk = source.read(1024)
                    if chunk:
                        sys.stdout.write(chunk.decode("utf-8", "replace"))
                        sys.stdout.flush()
        except (OSError, serial.SerialException) as error:
            errors += 1
            print(
                f"serial_reopen n={errors} type={type(error).__name__} detail={error} at={datetime.now(timezone.utc).isoformat()}",
                flush=True,
            )
            time.sleep(min(args.reconnect_delay, max(0.0, deadline - time.monotonic())))
    print(
        f"serial_end={datetime.now(timezone.utc).isoformat()} sessions={sessions} reopen_errors={errors}",
        flush=True,
    )


if __name__ == "__main__":
    main()
