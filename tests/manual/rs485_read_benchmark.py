#!/usr/bin/env python3
"""Read-only FC03 size/throughput benchmark on a connected RS485 port.

Latency starts BEFORE serial.write()/flush(), ending at the complete CRC-checked
response. This includes request TX, turnaround, response RX and host USB/OS delay.
Successful transactions per second use total measured batch wall time, not the
reciprocal of a partial RX timer. TCP supplies a static holding-register baseline.
No configuration, outputs, baud rate or firmware is changed on the device.
"""
from __future__ import annotations

import argparse
import csv
from datetime import datetime, timedelta, timezone
import http.cookiejar
import json
import math
import os
from pathlib import Path
import socket
import statistics
import struct
import time
import urllib.parse
import urllib.request

import serial

LOCAL_TZ = timezone(timedelta(hours=8))


def now():
    return datetime.now(LOCAL_TZ).isoformat(timespec="seconds")


def crc16(data):
    value = 0xFFFF
    for byte in data:
        value ^= byte
        for _ in range(8):
            value = (value >> 1) ^ (0xA001 if value & 1 else 0)
    return value


def exact(sock, size):
    value = bytearray()
    while len(value) < size:
        chunk = sock.recv(size - len(value))
        if not chunk:
            raise ConnectionError("TCP closed before full Modbus response")
        value.extend(chunk)
    return bytes(value)


def tcp_read(ip, unit, address, count, function=3):
    pdu = struct.pack(">BHH", function, address, count)
    packet = struct.pack(">HHHB", 0x3301, 0, len(pdu) + 1, unit) + pdu
    with socket.create_connection((ip, 502), timeout=4) as sock:
        sock.settimeout(4)
        sock.sendall(packet)
        header = exact(sock, 7)
        tid, protocol, length, returned_unit = struct.unpack(">HHHB", header)
        if (tid, protocol, returned_unit) != (0x3301, 0, unit) or not 2 <= length <= 254:
            raise ValueError(f"Invalid TCP header {header.hex()}")
        body = exact(sock, length - 1)
    if len(body) != 2 + count * 2 or body[:2] != bytes((function, count * 2)):
        raise ValueError(f"Unexpected TCP PDU {body.hex()}")
    return body[2:]


def words(data):
    return list(struct.unpack(">" + "H" * (len(data) // 2), data))


class DeviceHealth:
    def __init__(self, ip):
        self.base = f"http://{ip}"
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())
        )
        body = urllib.parse.urlencode({
            "username": os.environ.get("IMC_WEB_USER", "admin"),
            "pwd": os.environ.get("IMC_WEB_PASSWORD", "admin123456"),
        }).encode()
        request = urllib.request.Request(self.base + "/login", data=body)
        with self.opener.open(request, timeout=4) as response:
            value = json.load(response)
        if value.get("code") != "0":
            raise ValueError("Web login rejected; set IMC_WEB_USER/IMC_WEB_PASSWORD")

    def get(self, path):
        with self.opener.open(self.base + path, timeout=4) as response:
            value = json.load(response)
        if value.get("code") != "0":
            raise ValueError(f"Health request failed: {path}")
        return value["data"]

    def snapshot(self):
        io = self.get("/getiodata")
        return {"at": now(), "status": self.get("/getsystemstatus"),
                "ports": self.get("/getportconfig"), "diagnostics": io["diagnostics"]}


def exchange(port, packet, expected, unit, count, timeout):
    # Do not reset input or clear delayed data on every request: unsolicited or
    # duplicate responses must remain visible and fail the bench if misframed.
    started_ns = time.perf_counter_ns()
    sent = port.write(packet)
    if sent != len(packet):
        raise OSError(f"Short serial write {sent}/{len(packet)}")
    port.flush()
    drained_ns = time.perf_counter_ns()
    deadline_ns = started_ns + int(timeout * 1e9)
    frame = bytearray()
    first_ns = None
    target = None
    while time.perf_counter_ns() < deadline_ns:
        chunk = port.read(1)
        if not chunk:
            continue
        if first_ns is None:
            first_ns = time.perf_counter_ns()
        frame.extend(chunk)
        if len(frame) >= 2 and frame[1] & 0x80:
            target = 5
        elif len(frame) >= 3 and frame[1] == 3:
            target = 5 + frame[2]
        if target is not None and len(frame) >= target:
            break
        if len(frame) >= 256:
            break
    completed_ns = time.perf_counter_ns()
    shape_ok = len(frame) == 5 + 2 * count and frame[:3] == bytes((unit, 3, 2 * count))
    crc_ok = len(frame) >= 4 and crc16(frame[:-2]) == int.from_bytes(frame[-2:], "little")
    data_ok = shape_ok and frame[3:-2] == expected
    return {
        "ok": shape_ok and crc_ok and data_ok,
        "request_hex": packet.hex(), "response_hex": frame.hex(),
        "rx_bytes": len(frame), "shape_ok": shape_ok, "crc_ok": crc_ok,
        "tcp_match": data_ok,
        "write_flush_ms": (drained_ns - started_ns) / 1e6,
        "first_ms": None if first_ns is None else (first_ns - started_ns) / 1e6,
        "complete_ms": (completed_ns - started_ns) / 1e6,
        "receive_after_flush_ms": (completed_ns - drained_ns) / 1e6,
    }


def summarize_latency(rows, name):
    values = sorted(row[name] for row in rows if row["ok"] and row[name] is not None)
    if not values:
        return None
    percentile = lambda p: values[max(0, math.ceil(p * len(values)) - 1)]
    return {"min": min(values), "median": statistics.median(values),
            "mean": statistics.mean(values), "p95": percentile(.95),
            "p99": percentile(.99), "max": max(values)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ip", default="192.168.51.226")
    parser.add_argument("--serial", default="/dev/cu.usbserial-14140")
    parser.add_argument("--baud", type=int, default=9600)
    parser.add_argument("--unit", type=int, default=1)
    parser.add_argument("--address", type=int, default=2300)
    parser.add_argument("--counts", default="1,2,4,8,16,32,64,100,125")
    parser.add_argument("--gaps-ms", default="4.2,0")
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--timeout-ms", type=int, default=1000)
    parser.add_argument("--expected-uart", type=int, default=2)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    counts = [int(value) for value in args.counts.split(",")]
    gaps = [float(value) for value in args.gaps_ms.split(",")]
    if (not counts or not all(1 <= count <= 125 and 0 <= args.address <= 65536 - count for count in counts)
            or args.samples < 1 or args.timeout_ms < 1 or not gaps or any(gap < 0 for gap in gaps)):
        parser.error("Invalid count/address/samples/timeout/gaps")
    args.out.mkdir(parents=True, exist_ok=False)
    health = DeviceHealth(args.ip)
    before = health.snapshot()
    firmware = words(tcp_read(args.ip, args.unit, 0x087E, 2, function=4))
    model = words(tcp_read(args.ip, args.unit, 0x08A5, 1))
    baseline = {count: tcp_read(args.ip, args.unit, args.address, count) for count in counts}
    port = serial.Serial(args.serial, args.baud, bytesize=8, parity="N", stopbits=1,
                         timeout=.005, write_timeout=2, exclusive=True)
    port.reset_input_buffer()
    meta = {"at": now(), "args": {**vars(args), "out": str(args.out)},
            "firmware_regs": firmware, "model_regs": model,
            "measurement": "perf_counter_ns immediately BEFORE serial.write through complete response; PC/USB inclusive",
            "uart_format": "8N1", "tcp_baselines_hex": {k:v.hex() for k,v in baseline.items()},
            "health_before": before}
    (args.out / "metadata.json").write_text(json.dumps(meta, ensure_ascii=False, indent=2) + "\n")
    results = []
    failed = False
    try:
        with (args.out / "events.jsonl").open("w", buffering=1) as events:
            for gap_ms in gaps:
                for count in counts:
                    request = struct.pack(">BBHH", args.unit, 3, args.address, count)
                    request += struct.pack("<H", crc16(request))
                    time.sleep(.1)
                    pending = port.read_all()
                    if pending:
                        events.write(json.dumps({"event": "unexpected_between_batches", "bytes": pending.hex()}) + "\n")
                        failed = True
                    # Three warmups are logged, but excluded from throughput.
                    for index in range(3):
                        row = exchange(port, request, baseline[count], args.unit, count, args.timeout_ms / 1000)
                        events.write(json.dumps({"event": "warmup", "count": count, "gap_ms": gap_ms, "index": index + 1, **row}) + "\n")
                        if not row["ok"]:
                            raise RuntimeError(f"Warmup failed (count={count}): {row}")
                        time.sleep(max(gap_ms / 1000, .0042))
                    rows = []
                    start = time.perf_counter_ns()
                    for index in range(args.samples):
                        row = exchange(port, request, baseline[count], args.unit, count, args.timeout_ms / 1000)
                        rows.append(row)
                        # Include pacing and log overhead in wall-time throughput.
                        events.write(json.dumps({"event": "request", "count": count, "gap_ms": gap_ms, "index": index + 1, **row}) + "\n")
                        if not row["ok"]:
                            failed = True
                        if index + 1 < args.samples and gap_ms:
                            time.sleep(gap_ms / 1000)
                    elapsed = (time.perf_counter_ns() - start) / 1e9
                    ok = sum(row["ok"] for row in rows)
                    result = {"count": count, "samples": len(rows), "success": ok,
                              "fail": len(rows) - ok, "gap_ms": gap_ms, "wall_seconds": elapsed,
                              "transactions_per_second": ok / elapsed,
                              "registers_per_second": ok * count / elapsed,
                              "useful_bytes_per_second": ok * count * 2 / elapsed,
                              "wire_bytes_only_ms": (8 + 5 + 2 * count) * 10 * 1000 / args.baud,
                              "complete_ms": summarize_latency(rows, "complete_ms"),
                              "first_ms": summarize_latency(rows, "first_ms"),
                              "write_flush_ms": summarize_latency(rows, "write_flush_ms"),
                              "timeouts": sum(row["rx_bytes"] == 0 for row in rows),
                              "crc_errors": sum(row["rx_bytes"] > 0 and not row["crc_ok"] for row in rows),
                              "data_mismatches": sum(row["shape_ok"] and row["crc_ok"] and not row["tcp_match"] for row in rows)}
                    results.append(result)
                    print(json.dumps(result, ensure_ascii=False), flush=True)
    finally:
        port.close()
        after = health.snapshot()
        (args.out / "health-after.json").write_text(json.dumps(after, ensure_ascii=False, indent=2) + "\n")
        deltas = []
        for old, new in zip(before["diagnostics"]["rtu"], after["diagnostics"]["rtu"]):
            deltas.append({"uart": new["uart"], **{key: new[key] - old[key] for key in
                           ("rx_frames", "tx_frames", "tx_errors", "service_frames", "driver_opens")}})
        expected_requests = len(counts) * len(gaps) * (args.samples + 3)
        matched_uart = next((row for row in deltas if row["uart"] == args.expected_uart), None)
        port_verified = bool(matched_uart and matched_uart["tx_frames"] == expected_requests
                             and matched_uart["service_frames"] == expected_requests
                             and matched_uart["tx_errors"] == 0 and matched_uart["driver_opens"] == 0)
        no_reset = before["status"]["reset_count"] == after["status"]["reset_count"] and after["status"]["uptime"] >= before["status"]["uptime"]
        summary = {"ended": now(), "all_pass": not failed and port_verified and no_reset and len(results) == len(counts) * len(gaps),
                   "expected_uart": args.expected_uart, "port_verified": port_verified,
                   "no_reset": no_reset, "uart_deltas": deltas, "results": results}
        (args.out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
        fields = ["gap_ms", "count", "samples", "success", "fail", "wire_bytes_only_ms", "wall_seconds", "transactions_per_second", "registers_per_second", "useful_bytes_per_second"]
        with (args.out / "summary.csv").open("w", newline="") as csv_file:
            writer = csv.DictWriter(csv_file, fieldnames=fields + ["mean_ms", "p95_ms", "max_ms"])
            writer.writeheader()
            for row in results:
                writer.writerow({**{k: row[k] for k in fields},
                                 **{k: row["complete_ms"][v] if row["complete_ms"] else None for k,v in
                                    (("mean_ms", "mean"), ("p95_ms", "p95"), ("max_ms", "max"))}})
        print(json.dumps({k:v for k,v in summary.items() if k != "results"}, ensure_ascii=False), flush=True)
    return 0 if summary["all_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
