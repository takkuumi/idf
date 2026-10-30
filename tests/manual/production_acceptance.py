#!/usr/bin/env python3
"""Read-only concurrent Web/Modbus acceptance with explicit error/latency data."""

from __future__ import annotations

import argparse
import concurrent.futures
import http.cookiejar
import json
import os
import socket
import struct
import threading
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone

from stability_soak import read_registers

OUTPUT_LOCK = threading.Lock()


def emit(event: str, **fields) -> None:
    with OUTPUT_LOCK:
        print(json.dumps(dict(at=datetime.now(timezone.utc).isoformat(), event=event, **fields)), flush=True)


def receive_frame(sock: socket.socket, transaction: int) -> bytes:
    def exact(length: int) -> bytes:
        data = bytearray()
        while len(data) < length:
            part = sock.recv(length - len(data))
            if not part:
                raise ConnectionError("truncated Modbus response")
            data.extend(part)
        return bytes(data)
    tid, protocol, length, unit = struct.unpack(">HHHB", exact(7))
    if (tid, protocol, unit) != (transaction, 0, 1) or not 2 <= length <= 254:
        raise ValueError("invalid response header")
    return exact(length - 1)


def request_frame(transaction: int, pdu: bytes) -> bytes:
    return struct.pack(">HHHB", transaction, 0, len(pdu) + 1, 1) + pdu


def protocol_checks(host: str, port: int) -> None:
    with socket.create_connection((host, port), timeout=3) as sock:
        sock.settimeout(3)
        # All malformed write requests fail validation before invoking a write.
        for transaction, (pdu, expected) in enumerate([
            (struct.pack(">BHH", 3, 2280, 0), b"\x83\x03"),
            (struct.pack(">BHH", 3, 2280, 126), b"\x83\x03"),
            (struct.pack(">BHH", 4, 65535, 1), b"\x84\x02"),
            (struct.pack(">BHH", 3, 65535, 2), b"\x83\x02"),
            (b"\x7f", b"\xff\x01"),
            (b"\x10\x08\xe8\x00\x01\x00", b"\x90\x03"),
        ], 1):
            sock.sendall(request_frame(transaction, pdu))
            actual = receive_frame(sock, transaction)
            if actual != expected:
                raise ValueError(f"negative case {transaction}: {actual.hex()} != {expected.hex()}")
        pdu = struct.pack(">BHH", 4, 0x087E, 2)
        fragmented = request_frame(20, pdu)
        for byte in fragmented:
            sock.sendall(bytes([byte]))
            time.sleep(0.002)
        if receive_frame(sock, 20)[:2] != b"\x04\x04":
            raise ValueError("fragmented request failed")
        sock.sendall(request_frame(21, pdu) + request_frame(22, pdu))
        for transaction in (21, 22):
            if receive_frame(sock, transaction)[:2] != b"\x04\x04":
                raise ValueError("pipelined request failed")
    emit("protocol_checks_passed", port=port, cases=9)


def percentile(ordered: list[float], fraction: float) -> float:
    return ordered[min(len(ordered) - 1, int((len(ordered) - 1) * fraction))] if ordered else 0.0


def web_validation_checks(opener, base: str, password: str, api) -> None:
    before = {path: api(path) for path in ("getbleconfig", "getsensorconfig")}
    outputs = api("getiodata")["do_data"]
    cases = [
        ("iocontrol", {}, "missing_io_fields"),
        ("iocontrol", {"addr": "invalid", "value": "1"}, "invalid_io_address"),
        ("iocontrol", {"addr": "0", "value": "2"}, "nonboolean_io_value"),
        ("updatesensorconfig", {}, "missing_sensor_fields"),
        ("updatesensorconfig", {"max": "3016"}, "partial_sensor_fields"),
        ("updatesensorconfig", {"ch": "0", "min": "4096", "max": "0"}, "out_of_range_adc"),
        ("updatebleconfig", {}, "missing_ble_name"),
        ("updatebleconfig", {"blename": "bad\0name"}, "nul_ble_name"),
        ("updateportconfig", {}, "missing_uart_fields"),
        ("updatenetworkconfig", {"ip": "999.0.0.1"}, "invalid_network_fields"),
        ("updatepwd", {"opwd": password, "npwd": ""}, "empty_password"),
        ("updatepwd", {"opwd": password, "npwd": "a" * 64}, "unreloadable_password"),
    ]
    for endpoint, fields, label in cases:
        request = urllib.request.Request(base + "/" + endpoint,
            data=urllib.parse.urlencode(fields).encode())
        with opener.open(request, timeout=5) as response:
            result = json.load(response)
        if str(result.get("code")) == "0":
            raise ValueError(f"malformed Web request accepted: {label}")
    if any(api(path) != previous for path, previous in before.items()) or api("getiodata")["do_data"] != outputs:
        raise ValueError("rejected Web requests changed settings or outputs")
    anonymous = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with anonymous.open(base + "/getsystemstatus", timeout=5) as response:
        if not response.url.endswith("/login") or "text/html" not in response.headers.get("Content-Type", ""):
            raise ValueError("unauthenticated status request was not redirected to login")
    emit("web_validation_passed", rejected_cases=len(cases), outputs_unchanged=True, anonymous_status_blocked=True)


def poll_port(args, port: int, deadline: float, baseline_logic: list[int]) -> dict:
    specs = [(4, 0x80, 8), (4, 0x88, 8), (4, 0x087E, 2), (3, 2280, 16),
             (3, 2184, 125), (3, 2300, 32), (4, args.destination, args.count),
             (4, 0x90, 16), (4, 30001, 125), (3, 40001, 125)]
    latencies = []
    errors = 0
    sock = None
    last_result = None
    changes = 0
    requests = 0
    try:
        while time.monotonic() < deadline:
            for fc, address, count in specs:
                if time.monotonic() >= deadline:
                    break
                requests += 1
                started = time.monotonic()
                try:
                    if sock is None:
                        sock = socket.create_connection((args.host, port), timeout=3)
                        sock.settimeout(3)
                    values = read_registers(sock, 1, fc, address, count)
                    if (fc, address) == (3, 2300) and values != baseline_logic:
                        raise ValueError("configuration table changed during read-only run")
                    if (fc, address) == (4, 0x087E) and values[0] != args.expected_version:
                        raise ValueError(f"firmware changed: {values[0]:04x}")
                    if (fc, address) == (4, args.destination):
                        changes += last_result is not None and values != last_result
                        last_result = values
                    latencies.append((time.monotonic() - started) * 1000)
                except Exception as error:
                    errors += 1
                    emit("modbus_error", port=port, fc=fc, address=address, count=count, error=str(error))
                    if sock is not None:
                        sock.close()
                        sock = None
                    time.sleep(0.1)
            if args.interval:
                time.sleep(args.interval)
    finally:
        if sock is not None:
            sock.close()
    latencies.sort()
    result = dict(port=port, requests=requests, successes=len(latencies), errors=errors,
                  mean_ms=sum(latencies) / max(1, len(latencies)),
                  p95_ms=percentile(latencies, 0.95), p99_ms=percentile(latencies, 0.99),
                  maximum_ms=latencies[-1] if latencies else 0, result_changes=changes,
                  final_result=last_result)
    emit("modbus_summary", **result)
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--duration", type=float, default=600)
    parser.add_argument("--interval", type=float, default=0)
    parser.add_argument("--destination", type=int, default=1)
    parser.add_argument("--count", type=int, default=2)
    parser.add_argument("--expected-version", type=lambda text: int(text, 0), default=0x00E5)
    args = parser.parse_args()
    if args.duration <= 0 or args.interval < 0 or not 1 <= args.count <= 125 or not 0 <= args.destination <= 65536 - args.count:
        parser.error("invalid duration, interval or result window")
    password = os.environ.get("GW_STABILITY_WEB_PASSWORD")
    if not password:
        parser.error("set GW_STABILITY_WEB_PASSWORD; credentials are never written to logs")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    login = urllib.request.Request(f"http://{args.host}/login", data=urllib.parse.urlencode(
        {"username": "admin", "pwd": password}).encode())
    with opener.open(login, timeout=5) as response:
        response.read()

    def api(path: str) -> dict:
        with opener.open(f"http://{args.host}/{path}", timeout=5) as response:
            envelope = json.load(response)
        if envelope.get("type") != path or str(envelope.get("code")) != "0":
            raise ValueError(f"invalid {path} response")
        return envelope["data"]

    before = {path: api(path) for path in ("getsystemstatus", "getnetworkconfig", "getportconfig", "getsysteminfo", "getiodata")}
    web_validation_checks(opener, f"http://{args.host}", password, api)
    with socket.create_connection((args.host, 502), timeout=3) as sock:
        sock.settimeout(3)
        baseline_logic = read_registers(sock, 1, 3, 2300, 32)
    for port in (502, 503, 504, 5002):
        protocol_checks(args.host, port)
    emit("start", host=args.host, duration=args.duration, expected_version=args.expected_version,
         baseline=before, logic=baseline_logic)
    started = time.monotonic()
    deadline = started + args.duration
    web_samples = web_errors = 0
    previous_uptime = before["getsystemstatus"]["uptime"]
    initial_resets = before["getsystemstatus"]["reset_count"]
    minimum_heap = before["getsystemstatus"]["free_heap"]
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        futures = [pool.submit(poll_port, args, port, deadline, baseline_logic) for port in (502, 503, 504, 5002)]
        while time.monotonic() < deadline:
            try:
                state = api("getsystemstatus")
                web_samples += 1
                if state["uptime"] < previous_uptime or state["reset_count"] != initial_resets:
                    raise ValueError("device restarted or uptime regressed")
                if not state["ethernet_link"] or state["recovery_mode"] != "Normal":
                    raise ValueError("link/recovery status abnormal")
                if any(state[name] != 0 for name in ("recoverable", "degradable", "severe")):
                    raise ValueError("nonzero firmware fault counters")
                previous_uptime = state["uptime"]
                minimum_heap = min(minimum_heap, state["free_heap"])
                if api("getnetworkconfig") != before["getnetworkconfig"] or api("getportconfig") != before["getportconfig"]:
                    raise ValueError("network/port configuration changed")
                io = api("getiodata")
                emit("web_health", state=state, fiber=[io["state_data"][f"state_addr_{i}"] for i in (6, 7)])
            except Exception as error:
                web_errors += 1
                emit("web_error", error=str(error))
            time.sleep(min(5, max(0, deadline - time.monotonic())))
        results = [future.result() for future in futures]
    duration = time.monotonic() - started
    errors = web_errors + sum(result["errors"] for result in results)
    emit("end", duration=duration, requests=sum(result["requests"] for result in results),
         requests_per_second=sum(result["successes"] for result in results) / duration,
         web_samples=web_samples, web_errors=web_errors, minimum_heap=minimum_heap, errors=errors)
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
