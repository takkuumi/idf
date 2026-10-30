#!/usr/bin/env python3
"""Read-only verification of an RTU result window through Modbus TCP."""

import argparse
import json
import socket
import time
from datetime import datetime, timezone

from stability_soak import read_registers


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--port", type=int, default=502)
    parser.add_argument("--unit", type=int, default=1)
    parser.add_argument("--rs485-port", type=int, choices=(1, 2, 3), default=1)
    parser.add_argument("--destination", type=int, required=True)
    parser.add_argument("--count", type=int, required=True)
    parser.add_argument("--duration", type=float, default=60)
    parser.add_argument("--interval", type=float, default=0.5)
    parser.add_argument("--require-change", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.count <= 125 or args.duration <= 0 or args.interval <= 0:
        parser.error("count must be 1..125; duration and interval must be positive")

    def report(**fields):
        print(json.dumps({"time": datetime.now(timezone.utc).isoformat(), **fields}), flush=True)

    started = time.monotonic()
    samples = errors = changes = 0
    previous = None
    with socket.create_connection((args.host, args.port), timeout=5) as sock:
        sock.settimeout(5)
        report(event="start", destination=args.destination, count=args.count,
               rs485_port=args.rs485_port, duration=args.duration)
        while time.monotonic() - started < args.duration:
            cycle = time.monotonic()
            try:
                values = read_registers(sock, args.unit, 4, args.destination, args.count)
                # 2176..2181 contain the three ports' current error states.
                errors_by_port = read_registers(sock, args.unit, 3, 2176, 6)
                offset = (args.rs485_port - 1) * 2
                comerr, apperr = errors_by_port[offset:offset + 2]
                if comerr or apperr:
                    raise ValueError(f"RS485 error states: comerr={comerr}, apperr={apperr}")
                changes += int(previous is not None and values != previous)
                previous = values
                samples += 1
                report(event="sample", sample=samples, values=values,
                       comerr=comerr, apperr=apperr, latency_ms=round((time.monotonic()-cycle)*1000, 2))
            except (OSError, ValueError) as error:
                errors += 1
                report(event="error", detail=str(error))
                # A protocol/transport error may leave the socket out of sync.
                break
            time.sleep(max(0, args.interval - (time.monotonic() - cycle)))
    passed = samples > 0 and errors == 0 and (not args.require_change or changes > 0)
    report(event="end", samples=samples, errors=errors, changes=changes,
           duration=round(time.monotonic()-started, 2), passed=passed)
    if not passed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
