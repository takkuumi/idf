#!/usr/bin/env python3
"""Concurrent read-only TCP + RTU + native macOS BLE hardware stress test.

One target device, supported read requests, at most eight persistent TCP clients.
Never writes outputs/configuration or resets/updates firmware. Preserves every
transaction's timing/error/data-validation result and device health snapshots.
"""
from __future__ import annotations

import argparse
import collections
from datetime import datetime, timedelta, timezone
import hashlib
import http.cookiejar
import json
import math
import os
from pathlib import Path
import signal
import socket
import statistics
import struct
import subprocess
import threading
import time
import urllib.parse
import urllib.request

import serial

TZ = timezone(timedelta(hours=8))
COUNTS = (1, 2, 8, 32, 100, 125)
PORTS = (502, 503, 504, 5002)


def stamp():
    return datetime.now(TZ).isoformat(timespec="milliseconds")


def crc16(data):
    crc = 0xffff
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ (0xa001 if crc & 1 else 0)
    return crc


def recv_exact(sock, count):
    data = bytearray()
    while len(data) < count:
        value = sock.recv(count - len(data))
        if not value:
            raise ConnectionError(f"peer closed at {len(data)}/{count}")
        data.extend(value)
    return bytes(data)


def tcp_exchange(sock, tx, pdu, unit):
    started = time.perf_counter()
    sock.sendall(struct.pack(">HHHB", tx, 0, len(pdu) + 1, unit) + pdu)
    head = recv_exact(sock, 7)
    tid, protocol, length, uid = struct.unpack(">HHHB", head)
    if (tid, protocol, uid) != (tx, 0, unit) or not 2 <= length <= 254:
        raise ValueError(f"MBAP mismatch: {head.hex()}")
    response = recv_exact(sock, length - 1)
    if response[0] != pdu[0]:
        raise ValueError(f"unexpected/exception PDU: {response.hex()}")
    return response, (time.perf_counter() - started) * 1000


class Device:
    def __init__(self, ip, unit):
        self.ip, self.unit = ip, unit
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        form = urllib.parse.urlencode({
            "username": os.getenv("IMC_WEB_USER", "admin"),
            "pwd": os.getenv("IMC_WEB_PASSWORD", "admin123456"),
        }).encode()
        with self.opener.open(urllib.request.Request(f"http://{ip}/login", data=form), timeout=4) as r:
            if json.load(r).get("code") != "0":
                raise RuntimeError("Web authentication rejected")

    def api(self, path):
        start = time.perf_counter()
        with self.opener.open(f"http://{self.ip}/{path}", timeout=3) as r:
            if "application/json" not in r.headers.get("Content-Type", ""):
                raise ValueError(f"Web session replaced or unexpected content at {path}")
            data = json.load(r)
        if data.get("code") != "0":
            raise ValueError(f"Web API rejected {path}: {data.get('code')}")
        return data["data"], (time.perf_counter() - start) * 1000

    def read(self, fc, address, count):
        with socket.create_connection((self.ip, 502), timeout=3) as s:
            s.settimeout(3)
            pdu = struct.pack(">BHH", fc, address, count)
            result, _ = tcp_exchange(s, 0x3333, pdu, self.unit)
            if len(result) != 2 + 2 * count or result[:2] != bytes((fc, 2 * count)):
                raise ValueError(f"Unexpected PDU: {result.hex()}")
            return result

    def span(self, address, count):
        value = bytearray()
        for offset in range(0, count, 125):
            value.extend(self.read(3, address + offset, min(125, count - offset))[2:])
        return bytes(value)


class Control:
    def __init__(self, path):
        self.path = path
        self.value = {"phase": "initial", "tcp_clients": 0, "rs_enabled": False,
                      "ble_enabled": False, "profile": "mixed", "stop": False,
                      "tcp_gap_ms": 0, "rs_gap_ms": 0, "ble_gap_ms": 0,
                      "tcp_reconnect_s": 0, "ble_reconnect_s": 0}
        self.publish()

    def publish(self, **change):
        self.value = {**self.value, **change}
        temp = self.path.with_suffix(".tmp")
        temp.write_text(json.dumps(self.value))
        temp.replace(self.path)


def spec(profile, index):
    counts = (100, 125) if profile == "bulk" else COUNTS
    count = counts[index % len(counts)]
    return count, struct.pack(">BHH", 3, 2300, count)


def log_json(handle, value):
    handle.write(json.dumps({"epoch_s": time.time(), **value}, separators=(",", ":")) + "\n")


def tcp_worker(index, args, control, baseline):
    sock = None
    opened = 0.0
    seq, ordinal = index * 7000 + 1, 0
    last_counter_at = -100.0
    path = args.out / f"tcp-{index}-{PORTS[index % 4]}.jsonl"
    with path.open("w", buffering=1) as out:
        while not control.value["stop"]:
            phase = control.value
            if index >= phase["tcp_clients"]:
                if sock:
                    sock.close(); sock = None
                time.sleep(.02); continue
            if sock and phase["tcp_reconnect_s"] > 0 and time.perf_counter() - opened >= phase["tcp_reconnect_s"]:
                sock.close(); sock = None
            count, pdu = spec(phase["profile"], ordinal)
            ordinal += 1
            started = time.perf_counter()
            try:
                connect_ms = None
                if sock is None:
                    connected_at = time.perf_counter()
                    sock = socket.create_connection((args.ip, PORTS[index % 4]), timeout=3)
                    sock.settimeout(2)
                    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                    connect_ms = (time.perf_counter() - connected_at) * 1000
                    opened = time.perf_counter()
                    log_json(out, {"event": "connect", "phase": phase["phase"], "connect_ms": connect_ms})
                if index == 0 and time.perf_counter() - last_counter_at >= 1:
                    seq = seq % 65535 + 1
                    counters, _ = tcp_exchange(sock, seq, struct.pack(">BHH", 4, 0x0880, 5), args.unit)
                    if len(counters) != 12 or counters[:2] != bytes((4, 10)):
                        raise ValueError("bad health counter PDU")
                    log_json(out, {"event": "device_counters", "phase": phase["phase"],
                                   "values": list(struct.unpack(">5H", counters[2:]))})
                    last_counter_at = time.perf_counter()
                    # Counter telemetry is excluded from read-response latency.
                    started = time.perf_counter()
                seq = seq % 65535 + 1
                response, response_ms = tcp_exchange(sock, seq, pdu, args.unit)
                if response != baseline[f"3:2300:{count}"]:
                    raise ValueError(f"payload mismatch: {response.hex()}")
                log_json(out, {"event": "request_ok", "phase": phase["phase"], "count": count,
                               "latency_ms": (time.perf_counter() - started) * 1000,
                               "response_ms": response_ms, "useful_bytes": count * 2})
            except Exception as e:
                log_json(out, {"event": "request_error", "phase": phase["phase"], "count": count,
                               "latency_ms": (time.perf_counter() - started) * 1000, "error": repr(e)})
                if sock:
                    sock.close(); sock = None
                time.sleep(.1)
            if phase["tcp_gap_ms"]:
                time.sleep(phase["tcp_gap_ms"] / 1000)
    if sock:
        sock.close()


def rs_worker(args, control, baseline):
    port = serial.Serial(args.serial, args.baud, bytesize=8, parity="N", stopbits=1,
                         timeout=.005, write_timeout=2, exclusive=True)
    port.reset_input_buffer()
    ordinal = 0
    packets = {}
    for count in COUNTS:
        p = struct.pack(">BBHH", args.unit, 3, 2300, count)
        packets[count] = p + struct.pack("<H", crc16(p))
    try:
        with (args.out / "rs485.jsonl").open("w", buffering=1) as out:
            while not control.value["stop"]:
                phase = control.value
                if not phase["rs_enabled"]:
                    time.sleep(.02); continue
                count, _ = spec(phase["profile"], ordinal)
                ordinal += 1
                started = time.perf_counter()
                frame = bytearray()
                first_ms = None
                try:
                    packet = packets[count]
                    if port.write(packet) != len(packet):
                        raise IOError("short serial write")
                    port.flush()
                    deadline = started + 1
                    while time.perf_counter() < deadline:
                        byte = port.read(1)
                        if not byte:
                            continue
                        if first_ms is None:
                            first_ms = (time.perf_counter() - started) * 1000
                        frame.extend(byte)
                        if len(frame) >= 2 and frame[1] & 0x80 and len(frame) >= 5:
                            break
                        if len(frame) >= 3 and frame[1] == 3 and len(frame) >= 5 + frame[2]:
                            break
                        if len(frame) >= 256:
                            break
                    if len(frame) != 5 + count * 2:
                        raise ValueError(f"length/timeout: {len(frame)}/{5+count*2}")
                    if crc16(frame[:-2]) != int.from_bytes(frame[-2:], "little"):
                        raise ValueError("CRC mismatch")
                    if frame[0] != args.unit or frame[1:-2] != baseline[f"3:2300:{count}"]:
                        raise ValueError("RTU payload mismatch")
                    log_json(out, {"event": "request_ok", "phase": phase["phase"], "count": count,
                                   "latency_ms": (time.perf_counter() - started) * 1000,
                                   "first_ms": first_ms, "useful_bytes": 2 * count})
                except Exception as e:
                    log_json(out, {"event": "request_error", "phase": phase["phase"], "count": count,
                                   "latency_ms": (time.perf_counter() - started) * 1000,
                                   "error": repr(e), "response_hex": frame.hex()})
                    # Recovery is explicit in the log; no automatic retransmit
                    # is counted as a successful replacement for a failed read.
                    time.sleep(.1); port.reset_input_buffer()
                if phase["rs_gap_ms"]:
                    time.sleep(phase["rs_gap_ms"] / 1000)
    finally:
        port.close()


def health_worker(args, control, device):
    with (args.out / "health.jsonl").open("w", buffering=1) as out:
        last_wall, last_mono = time.time(), time.perf_counter()
        while not control.value["stop"]:
            phase = control.value["phase"]
            started = time.perf_counter()
            try:
                status, status_ms = device.api("getsystemstatus")
                io, io_ms = device.api("getiodata")
                log_json(out, {"event": "health", "phase": phase, "status": status,
                               "diagnostics": io["diagnostics"], "status_ms": status_ms, "io_ms": io_ms,
                               "wall_minus_mono_gap_s": (time.time() - last_wall) - (time.perf_counter() - last_mono)})
                last_wall, last_mono = time.time(), time.perf_counter()
            except Exception as e:
                log_json(out, {"event": "health_error", "phase": phase, "error": repr(e)})
            time.sleep(max(.05, 1 - (time.perf_counter() - started)))


def distribution(values):
    if not values:
        return None
    values = sorted(values)
    def p(q):
        return values[max(0, math.ceil(q * len(values)) - 1)]
    return {"min": values[0], "p50": p(.5), "mean": statistics.mean(values),
            "p95": p(.95), "p99": p(.99), "max": values[-1]}


def summarize(out):
    phases = json.loads((out / "phases.json").read_text())
    buckets = collections.defaultdict(list)
    misc = collections.Counter()
    health_rows = []
    counter_rows = []
    for path in sorted(out.glob("*.jsonl")):
        if path.name == "health.jsonl":
            for line in path.open():
                item = json.loads(line)
                if item["event"] == "health":
                    health_rows.append(item)
                else:
                    misc["health_error"] += 1
            continue
        link = "tcp" if path.name.startswith("tcp-") else path.stem
        if link not in ("tcp", "rs485", "ble"):
            continue
        for line in path.open():
            item = json.loads(line)
            if item["event"] in ("request_ok", "request_error") and item.get("kind", "read") == "read":
                buckets[(item["phase"], link)].append(item)
            elif item["event"] == "device_counters":
                counter_rows.append(item)
            elif item["event"] not in ("unsolicited", "phase_seen"):
                misc[f"{link}.{item['event']}"] += 1
    results = []
    for phase in phases:
        for link in ("tcp", "rs485", "ble"):
            rows = buckets[(phase["phase"], link)]
            if not rows:
                continue
            ok = [r for r in rows if r["event"] == "request_ok"]
            bad = [r for r in rows if r["event"] != "request_ok"]
            per_count = {}
            for count in COUNTS:
                selected = [r for r in ok if r.get("count", int(r.get("key", "0:0:0").split(":")[-1])) == count]
                if selected:
                    per_count[count] = {"success": len(selected), "latency_ms": distribution([r["latency_ms"] for r in selected])}
            exposure = max(phase["duration_s"], max((r["epoch_s"] for r in rows), default=phase["end_epoch_s"]) - phase["start_epoch_s"])
            results.append({"phase": phase["phase"], "link": link, "phase_duration_s": phase["duration_s"],
                            "observation_window_s": exposure, "attempts": len(rows), "success": len(ok), "errors": len(bad),
                            "success_pct": len(ok) * 100 / len(rows),
                            "transactions_s": len(ok) / exposure,
                            "useful_bytes_s": sum(r["useful_bytes"] for r in ok) / exposure,
                            "latency_ms": distribution([r["latency_ms"] for r in ok]),
                            "errors_detail": bad[:30], "per_count": per_count})
    before = health_rows[0]["status"] if health_rows else {}
    after = health_rows[-1]["status"] if health_rows else {}
    result = {"phases": phases, "results": results, "misc_events": dict(misc),
              "health_samples": len(health_rows), "first_status": before, "last_status": after,
              "reset_counts": sorted(set(r["status"]["reset_count"] for r in health_rows)),
              "free_heap_min": min((r["status"]["free_heap"] for r in health_rows), default=None),
              "free_heap_max": max((r["status"]["free_heap"] for r in health_rows), default=None),
              "ble_drops_values": sorted(set(r["values"][4] for r in counter_rows)),
              "device_counter_samples": len(counter_rows),
              "errors": sum(r["errors"] for r in results)}
    (out / "summary.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--ip", default="192.168.51.226")
    p.add_argument("--serial", default="/dev/cu.usbserial-14140")
    p.add_argument("--baud", type=int, default=9600)
    p.add_argument("--unit", type=int, default=1)
    p.add_argument("--ble-peripheral", default="0DD838D4-67B0-5556-00CE-9A6020851A9B")
    p.add_argument("--ble-bin", type=Path, default=Path("/tmp/imc_ble_load"))
    p.add_argument("--expected-build-id", default=None,
                   help="require this firmware build id instead of the historical fixture id")
    p.add_argument("--smoke", action="store_true")
    p.add_argument("--capacity-compare", action="store_true",
                    help="run 8-client TCP-only, 8/7/6-client three-link comparisons")
    p.add_argument("--boundary", action="store_true",
                   help="run TCP-only 8-client and 8-client three-link boundary stages")
    p.add_argument("--out", type=Path, required=True)
    args = p.parse_args()
    args.out = args.out.resolve(); args.out.mkdir(parents=True, exist_ok=False)
    device = Device(args.ip, args.unit)
    status, _ = device.api("getsystemstatus")
    network, _ = device.api("getnetworkconfig")
    expected_build = args.expected_build_id
    if expected_build is not None and status["firmware"]["build_id"] != expected_build:
        raise RuntimeError(
            f"Unexpected target firmware build: {status['firmware']['build_id']} != {expected_build}"
        )
    if network["version"] != "F3":
        raise RuntimeError("Unexpected target firmware/model; re-audit test fixture")
    mac = network["mac"].replace(":", "")
    base = {f"3:2300:{c}": device.read(3, 2300, c) for c in COUNTS}
    persisted = {"logic": device.span(2300, 1700), "text": device.span(5000, 2000),
                 "ports": device.read(3, 2214, 15)[2:]}
    fixture = {"created": stamp(), "target_ip": args.ip, "ble_peripheral": args.ble_peripheral,
               "ethernet_mac_hex": mac, "output_directory": str(args.out),
               "baseline_pdu_hex": {key: value.hex() for key, value in base.items()},
               "status_before": status, "network_before": network,
               "health_input_regs_before": device.read(4, 0x0880, 5).hex(),
               "persisted_sha256_before": {key: hashlib.sha256(value).hexdigest() for key, value in persisted.items()}}
    (args.out / "fixture.json").write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n")
    control = Control(args.out / "control.json")
    plan = [
        ("tcp_baseline", 60, 4, False, False, "mixed"),
        ("rs485_baseline", 60, 0, True, False, "mixed"),
        ("ble_baseline", 60, 0, False, True, "mixed"),
        ("joint_4_clients", 120, 4, True, True, "mixed"),
        ("joint_8_clients", 180, 8, True, True, "mixed"),
        ("joint_8_bulk", 180, 8, True, True, "bulk"),
        ("joint_soak", 600, 4, True, True, "mixed"),
        ("joint_reconnect", 120, 4, True, True, "mixed"),
        ("recovery", 60, 4, True, True, "mixed"),
    ]
    if args.boundary:
        plan = [("tcp_only_8_clients", 60, 8, False, False, "mixed"),
                ("joint_8_clients", 180, 8, True, True, "mixed"),
                ("recovery", 60, 8, True, True, "mixed")]
    elif args.capacity_compare:
        plan = [("tcp_only_8_clients", 60, 8, False, False, "mixed"),
                ("joint_8_clients", 120, 8, True, True, "mixed"),
                ("joint_7_clients", 120, 7, True, True, "mixed"),
                ("joint_6_clients", 120, 6, True, True, "mixed")]
    elif args.smoke:
        plan = [("smoke", 30, 4, True, True, "mixed")]
    phases = []
    threads = [threading.Thread(target=tcp_worker, args=(i, args, control, base), name=f"tcp-{i}") for i in range(8)]
    threads += [threading.Thread(target=rs_worker, args=(args, control, base), name="rs485"),
                threading.Thread(target=health_worker, args=(args, control, device), name="health")]
    ble_log = (args.out / "ble-process.txt").open("w")
    ble = subprocess.Popen([str(args.ble_bin), str(control.path), str(args.out / "fixture.json")], stdout=ble_log, stderr=subprocess.STDOUT)
    caffeine = subprocess.Popen(["caffeinate", "-i", "-m", "-s"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for t in threads:
        t.start()
    interrupted = None
    try:
        for name, duration, tcp_count, rtu, bluetooth, profile in plan:
            control.publish(phase="between", tcp_clients=0, rs_enabled=False, ble_enabled=False,
                            tcp_reconnect_s=0, ble_reconnect_s=0, tcp_gap_ms=0, rs_gap_ms=0, ble_gap_ms=0)
            time.sleep(3)
            print(json.dumps({"event": "phase_start", "at": stamp(), "phase": name,
                              "seconds": duration, "tcp_clients": tcp_count, "rs485": rtu, "ble": bluetooth,
                              "profile": profile}), flush=True)
            phase_start = time.perf_counter()
            entry = {"phase": name, "started": stamp(), "start_epoch_s": time.time(),
                     "planned_s": duration, "tcp_clients": tcp_count, "rs485": rtu,
                     "ble": bluetooth, "profile": profile}
            control.publish(phase=name, tcp_clients=tcp_count, rs_enabled=rtu, ble_enabled=bluetooth,
                            profile=profile, tcp_reconnect_s=5 if name == "joint_reconnect" else 0,
                            ble_reconnect_s=25 if name == "joint_reconnect" else 0,
                            tcp_gap_ms=50 if name == "recovery" else 0,
                            rs_gap_ms=50 if name == "recovery" else 0,
                            ble_gap_ms=50 if name == "recovery" else 0)
            while time.perf_counter() - phase_start < duration:
                time.sleep(max(0, min(10, duration - (time.perf_counter() - phase_start))))
                if ble.poll() is not None:
                    raise RuntimeError(f"BLE process exited unexpectedly ({ble.returncode})")
                dead = [t.name for t in threads if not t.is_alive()]
                if dead:
                    raise RuntimeError(f"Worker threads exited: {dead}")
                print(json.dumps({"event": "progress", "phase": name,
                                  "elapsed_s": round(time.perf_counter() - phase_start, 1), "at": stamp()}), flush=True)
            entry["duration_s"] = time.perf_counter() - phase_start
            entry["ended"] = stamp(); entry["end_epoch_s"] = time.time()
            phases.append(entry)
            (args.out / "phases.json").write_text(json.dumps(phases, indent=2) + "\n")
    except BaseException as e:
        interrupted = repr(e)
        print(json.dumps({"event": "interrupted", "error": interrupted}), flush=True)
    finally:
        control.publish(stop=True, tcp_clients=0, rs_enabled=False, ble_enabled=False)
        for t in threads:
            t.join(timeout=8)
        try:
            ble.wait(timeout=8)
        except subprocess.TimeoutExpired:
            ble.terminate(); ble.wait(timeout=4)
        caffeine.terminate(); caffeine.wait(timeout=4); ble_log.close()
        after = {}
        try:
            new_values = {"logic": device.span(2300, 1700), "text": device.span(5000, 2000),
                          "ports": device.read(3, 2214, 15)[2:]}
            after = {"status_after": device.api("getsystemstatus")[0],
                     "health_input_regs_after": device.read(4, 0x0880, 5).hex(),
                     "persisted_sha256_after": {k: hashlib.sha256(v).hexdigest() for k,v in new_values.items()},
                     "persisted_unchanged": {k: persisted[k] == v for k,v in new_values.items()}}
        except Exception as e:
            after["postcheck_error"] = repr(e)
        (args.out / "postcheck.json").write_text(json.dumps({"interrupted": interrupted, **after}, ensure_ascii=False, indent=2) + "\n")
        (args.out / "phases.json").write_text(json.dumps(phases, indent=2) + "\n")
        summary = summarize(args.out)
        print(json.dumps({"event": "end", "at": stamp(), "phases_completed": len(phases),
                          "errors": summary["errors"], "misc_events": summary["misc_events"],
                          "reset_counts": summary["reset_counts"], "interrupted": interrupted,
                          "out": str(args.out)}), flush=True)
    return 0 if not interrupted and not summary["errors"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
