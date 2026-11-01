#!/usr/bin/env python3
"""Authorized unloaded-bench FC05/FC01 benchmark; restore each tested output.

Requires --apply and a new recovery backup. Timings are host/network round trips,
not output-latch or relay-contact feedback. Do not run with other output writers.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import json
import math
import signal
import socket
import struct
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

from production_acceptance import receive_frame, request_frame


def stats(values):
    ordered = sorted(values)
    if not ordered:
        return {"count": 0}
    return dict(count=len(ordered), p50=ordered[math.ceil(.50 * len(ordered))-1],
                p95=ordered[math.ceil(.95 * len(ordered))-1],
                p99=ordered[math.ceil(.99 * len(ordered))-1], max=ordered[-1])


def read_bits(sock, transaction):
    sock.sendall(request_frame(transaction, struct.pack(">BHH", 1, 512, 16)))
    response = receive_frame(sock, transaction)
    if len(response) != 4 or response[:2] != b"\x01\x02":
        raise ValueError("invalid FC01 response: " + response.hex())
    return int.from_bytes(response[2:], "little")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--duration", type=float, default=60)
    parser.add_argument("--interval", type=float, default=.25)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not args.apply:
        parser.error("--apply requires explicit authorization to switch unloaded Q1..Q4")
    if (not math.isfinite(args.duration) or args.duration <= 0
            or not math.isfinite(args.interval) or args.interval < .1):
        parser.error("duration must be finite/positive and interval >= .1s")
    if args.output.exists() or args.backup.exists():
        parser.error("output/backup exists; never overwrite test or recovery evidence")
    with socket.create_connection((args.host, 502), timeout=3) as sock:
        sock.settimeout(3)
        original = read_bits(sock, 901)
    with args.backup.open("x") as recovery:
        json.dump(dict(host=args.host, at=datetime.now(timezone.utc).isoformat(),
                       original_bits=original, tested_mask=15, coil_base=512), recovery, indent=2)
    log_lock = threading.Lock()
    stop = threading.Event()
    interrupted = False

    def on_signal(_signum, _frame):
        nonlocal interrupted
        interrupted = True
        stop.set()

    signal.signal(signal.SIGINT, on_signal)
    signal.signal(signal.SIGTERM, on_signal)
    with args.output.open("x") as sink:
        def emit(event, **fields):
            record = dict(at=datetime.now(timezone.utc).isoformat(), event=event, **fields)
            with log_lock:
                sink.write(json.dumps(record) + "\n")
                sink.flush()

        def worker(port, channel):
            writes, reads, errors = [], [], []
            sock = None
            restored = False
            transaction = 1000 + channel
            original_bit = bool(original & (1 << channel))
            started = time.monotonic()
            try:
                sock = socket.create_connection((args.host, port), timeout=3)
                sock.settimeout(3)
                value = original_bit
                emit("port_start", port=port, channel=channel+1, original_bits=original)
                while not stop.is_set() and time.monotonic()-started < args.duration:
                    value = not value
                    transaction = (transaction+1) & 65535
                    pdu = struct.pack(">BHH", 5, 512+channel, 0xff00 if value else 0)
                    t0 = time.perf_counter()
                    sock.sendall(request_frame(transaction, pdu))
                    if receive_frame(sock, transaction) != pdu:
                        raise ValueError("FC05 acknowledgement mismatch")
                    write_ms = (time.perf_counter()-t0)*1000
                    transaction = (transaction+1) & 65535
                    t0 = time.perf_counter()
                    actual = read_bits(sock, transaction)
                    read_ms = (time.perf_counter()-t0)*1000
                    if bool(actual & (1 << channel)) != value:
                        raise ValueError(f"FC01 mismatch Q{channel+1}: bits={actual:#x}")
                    writes.append(write_ms)
                    reads.append(read_ms)
                    emit("sample", port=port, channel=channel+1, value=value,
                         write_ack_ms=write_ms, coil_read_ms=read_ms)
                    stop.wait(args.interval)
            except Exception as error:
                errors.append(str(error))
                stop.set()
                emit("port_error", port=port, error=str(error))
            finally:
                if sock is not None:
                    sock.close()
                # Use a new socket so a transport error cannot suppress cleanup.
                # Restore only this worker's bit; never overwrite other Q points.
                try:
                    with socket.create_connection((args.host, port), timeout=3) as recovery:
                        recovery.settimeout(3)
                        pdu = struct.pack(">BHH", 5, 512+channel, 0xff00 if original_bit else 0)
                        recovery.sendall(request_frame(65000, pdu))
                        if receive_frame(recovery, 65000) != pdu:
                            raise ValueError("restore acknowledgement mismatch")
                        bits = read_bits(recovery, 65001)
                        if bool(bits & (1 << channel)) != original_bit:
                            raise ValueError("restore FC01 mismatch")
                    restored = True
                    emit("port_restored", port=port, bits=bits)
                except Exception as error:
                    errors.append(str(error))
                    emit("restore_error", port=port, error=str(error), backup=str(args.backup))
            result = dict(port=port, channel=channel+1, writes=len(writes),
                          errors=len(errors), restored=restored,
                          write_ack_ms=stats(writes), coil_read_ms=stats(reads),
                          error_details=errors)
            emit("port_summary", **result)
            return result

        emit("start", host=args.host, ports=[502,503,504,5002], duration=args.duration,
             interval=args.interval, original_bits=original)
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            futures = [pool.submit(worker, port, channel)
                       for channel, port in enumerate((502,503,504,5002))]
            results = [future.result() for future in futures]
        time.sleep(2.2)  # Existing persistence task can observe restored outputs.
        final_errors = []
        final_bits = None
        try:
            with socket.create_connection((args.host,502),timeout=3) as sock:
                sock.settimeout(3)
                final_bits = read_bits(sock,65002)
            if final_bits != original:
                raise ValueError(f"final outputs changed: {final_bits:#x} != {original:#x}")
        except Exception as error:
            final_errors.append(str(error))
        errors = sum(r['errors'] for r in results)+len(final_errors)
        emit("end", results=results, errors=errors, interrupted=interrupted,
             original_bits=original, final_bits=final_bits, final_errors=final_errors)
    if errors or interrupted or any(not r['restored'] or not r['writes'] for r in results):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
