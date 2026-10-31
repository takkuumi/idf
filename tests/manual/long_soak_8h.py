#!/usr/bin/env python3
"""Eight-hour read-only gateway soak with serial and network evidence."""

from __future__ import annotations

import argparse
import json
import re
import socket
import struct
import threading
import time
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

try:
    import serial
except ImportError as exc:  # pragma: no cover - hardware runner dependency
    raise SystemExit("pyserial is required: python3 -m pip install pyserial") from exc


RESET_MARKERS = re.compile(
    r"TASK_WDT|WATCHDOG|panic|assert|stack canary|Guru Meditation|reset reason|rst:0x",
    re.IGNORECASE,
)


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def recv_exact(sock: socket.socket, count: int) -> bytes:
    data = bytearray()
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise ConnectionError("peer closed before complete Modbus response")
        data.extend(chunk)
    return bytes(data)


def read_registers(
    sock: socket.socket,
    unit: int,
    function: int,
    address: int,
    count: int,
    transaction: int,
) -> list[int]:
    pdu = struct.pack(">BHH", function, address, count)
    request = struct.pack(">HHHB", transaction, 0, len(pdu) + 1, unit) + pdu
    sock.sendall(request)
    header = recv_exact(sock, 7)
    tid, protocol, length, response_unit = struct.unpack(">HHHB", header)
    if tid != transaction or protocol != 0 or response_unit != unit or not 2 <= length <= 254:
        raise ValueError(f"invalid MBAP header {header.hex()}")
    body = recv_exact(sock, length - 1)
    if body[0] == function | 0x80:
        raise ValueError(f"Modbus exception 0x{body[1]:02X}")
    if len(body) != count * 2 + 2 or body[0] != function or body[1] != count * 2:
        raise ValueError(f"invalid response PDU {body.hex()}")
    return list(struct.unpack(f">{count}H", body[2:]))


def serial_reader(
    port: str,
    raw_path: Path,
    events_path: Path,
    stop: threading.Event,
    counters: dict[str, int],
    lock: threading.Lock,
) -> None:
    with raw_path.open("a", encoding="utf-8", errors="replace") as raw, events_path.open(
        "a", encoding="utf-8"
    ) as events:
        while not stop.is_set():
            try:
                with serial.Serial(port, 115200, timeout=0.5) as handle:
                    with lock:
                        counters["serial_sessions"] += 1
                    events.write(json.dumps({"ts": now_iso(), "event": "serial_connected"}) + "\n")
                    events.flush()
                    while not stop.is_set():
                        chunk = handle.read(4096)
                        if not chunk:
                            continue
                        text = chunk.decode("utf-8", errors="replace")
                        raw.write(f"\n[{now_iso()}] ")
                        raw.write(text)
                        raw.flush()
                        for line in text.splitlines():
                            if RESET_MARKERS.search(line):
                                with lock:
                                    counters["serial_markers"] += 1
                                events.write(
                                    json.dumps(
                                        {
                                            "ts": now_iso(),
                                            "event": "serial_marker",
                                            "line": line[:500],
                                        },
                                        ensure_ascii=False,
                                    )
                                    + "\n"
                                )
                                events.flush()
            except Exception as exc:  # reconnect until the soak ends
                with lock:
                    counters["serial_errors"] += 1
                events.write(
                    json.dumps(
                        {
                            "ts": now_iso(),
                            "event": "serial_error",
                            "error": f"{type(exc).__name__}: {exc}",
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
                events.flush()
                stop.wait(2.0)


def run(args: argparse.Namespace) -> int:
    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    raw_path = out / f"serial-{stamp}.log"
    events_path = out / f"events-{stamp}.jsonl"
    summary_path = out / f"summary-{stamp}.json"

    counters: dict[str, int] = defaultdict(int)
    per_port: dict[int, dict[str, float | int]] = {
        port: {"cycles": 0, "success": 0, "errors": 0, "rtt_sum_ms": 0.0, "rtt_max_ms": 0.0}
        for port in args.ports
    }
    lock = threading.Lock()
    stop = threading.Event()
    serial_thread = threading.Thread(
        target=serial_reader,
        args=(args.serial_port, raw_path, events_path, stop, counters, lock),
        name="serial-reader",
        daemon=True,
    )
    serial_thread.start()

    sockets: dict[int, socket.socket | None] = {port: None for port in args.ports}
    http_ok = 0
    http_errors = 0
    started = time.monotonic()
    next_http = started
    next_summary = started + 60.0
    transaction = 1

    def write_event(payload: dict[str, object]) -> None:
        with events_path.open("a", encoding="utf-8") as events:
            events.write(json.dumps({"ts": now_iso(), **payload}, ensure_ascii=False) + "\n")

    write_event(
        {
            "event": "soak_started",
            "host": args.host,
            "ports": args.ports,
            "duration_s": args.duration,
            "interval_s": args.interval,
            "serial_port": args.serial_port,
        }
    )

    try:
        while time.monotonic() - started < args.duration:
            cycle_started = time.monotonic()
            for port in args.ports:
                port_stats = per_port[port]
                port_stats["cycles"] += 1
                sock = sockets[port]
                request_started = time.perf_counter()
                try:
                    if sock is None:
                        sock = socket.create_connection((args.host, port), timeout=args.timeout)
                        sock.settimeout(args.timeout)
                        sockets[port] = sock
                    ai = read_registers(sock, 1, 4, 0x0080, 8, transaction & 0xFFFF)
                    transaction += 1
                    status = read_registers(sock, 1, 4, 0x0088, 8, transaction & 0xFFFF)
                    transaction += 1
                    holding = read_registers(sock, 1, 3, 2280, 16, transaction & 0xFFFF)
                    transaction += 1
                    version = read_registers(sock, 1, 4, 0x087E, 2, transaction & 0xFFFF)
                    transaction += 1
                    rtt_ms = (time.perf_counter() - request_started) * 1000.0
                    port_stats["success"] += 1
                    port_stats["rtt_sum_ms"] += rtt_ms
                    port_stats["rtt_max_ms"] = max(float(port_stats["rtt_max_ms"]), rtt_ms)
                    write_event(
                        {
                            "event": "modbus_ok",
                            "port": port,
                            "rtt_ms": round(rtt_ms, 3),
                            "ai": ai,
                            "status": status,
                            "holding_checksum": sum(holding) & 0xFFFF,
                            "firmware": version,
                        }
                    )
                except Exception as exc:
                    port_stats["errors"] += 1
                    write_event(
                        {
                            "event": "modbus_error",
                            "port": port,
                            "error": f"{type(exc).__name__}: {exc}",
                        }
                    )
                    if sock is not None:
                        try:
                            sock.close()
                        except OSError:
                            pass
                        sockets[port] = None

            now = time.monotonic()
            if now >= next_http:
                next_http = now + args.http_interval
                http_started = time.perf_counter()
                try:
                    with socket.create_connection((args.host, 80), timeout=args.timeout) as http:
                        http.settimeout(args.timeout)
                        http.sendall(
                            b"GET / HTTP/1.1\r\nHost: gateway\r\nConnection: close\r\n\r\n"
                        )
                        response = http.recv(256)
                    http_ok += 1
                    write_event(
                        {
                            "event": "http_ok",
                            "rtt_ms": round((time.perf_counter() - http_started) * 1000.0, 3),
                            "status": response.split(b"\r\n", 1)[0].decode("ascii", "replace"),
                        }
                    )
                except Exception as exc:
                    http_errors += 1
                    write_event(
                        {
                            "event": "http_error",
                            "error": f"{type(exc).__name__}: {exc}",
                        }
                    )

            if now >= next_summary:
                next_summary = now + 60.0
                with lock:
                    snapshot = dict(counters)
                write_event(
                    {
                        "event": "summary",
                        "elapsed_s": round(now - started, 1),
                        "ports": per_port,
                        "http_ok": http_ok,
                        "http_errors": http_errors,
                        "serial": snapshot,
                    }
                )

            time.sleep(max(0.0, args.interval - (time.monotonic() - cycle_started)))
    finally:
        stop.set()
        for sock in sockets.values():
            if sock is not None:
                try:
                    sock.close()
                except OSError:
                    pass
        serial_thread.join(timeout=3.0)

    with lock:
        serial_snapshot = dict(counters)
    summary = {
        "started_at": now_iso(),
        "host": args.host,
        "ports": args.ports,
        "duration_s": args.duration,
        "interval_s": args.interval,
        "ports_stats": per_port,
        "http_ok": http_ok,
        "http_errors": http_errors,
        "serial": serial_snapshot,
        "serial_log": str(raw_path),
        "events_log": str(events_path),
    }
    summary_path.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    write_event({"event": "soak_finished", "summary": str(summary_path)})
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--serial-port", default="/dev/cu.usbserial-1430")
    parser.add_argument("--output-dir", default="log/hardware/soak-8h-2026-11-01")
    parser.add_argument("--duration", type=float, default=8 * 60 * 60)
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument("--http-interval", type=float, default=5.0)
    parser.add_argument("--timeout", type=float, default=3.0)
    parser.add_argument("--ports", type=int, nargs="+", default=[502, 503, 504, 5002])
    return run(parser.parse_args())


if __name__ == "__main__":
    raise SystemExit(main())
