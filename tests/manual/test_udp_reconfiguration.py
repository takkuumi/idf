#!/usr/bin/env python3
"""Explicit bench-only UDP source/port/freshness test; restores every register."""

from __future__ import annotations

import argparse
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

from production_acceptance import receive_frame, request_frame
from stability_soak import read_registers


def emit(event: str, **values) -> None:
    print(json.dumps(dict(at=datetime.now(timezone.utc).isoformat(), event=event, **values)), flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    password = os.environ.get("GW_STABILITY_WEB_PASSWORD")
    if not args.apply or not password:
        parser.error("explicit --apply and GW_STABILITY_WEB_PASSWORD required")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    with opener.open(urllib.request.Request(f"http://{args.host}/login", data=urllib.parse.urlencode(
        {"username": "admin", "pwd": password}).encode()), timeout=5) as response:
        response.read()

    def status() -> dict:
        with opener.open(f"http://{args.host}/getsystemstatus", timeout=5) as response:
            result = json.load(response)
        if str(result.get("code")) != "0":
            raise ValueError("status rejected")
        return result["data"]

    def read(fc: int, address: int, count: int) -> list[int]:
        with socket.create_connection((args.host, 502), timeout=3) as sock:
            sock.settimeout(3)
            return read_registers(sock, 1, fc, address, count)

    def write(address: int, value: int) -> None:
        with socket.create_connection((args.host, 502), timeout=3) as sock:
            sock.settimeout(3)
            pdu = struct.pack(">BHH", 6, address, value)
            sock.sendall(request_frame(900, pdu))
            if receive_frame(sock, 900) != pdu:
                raise ValueError(f"holding write failed at {address}")

    original = read(3, 2190, 5)
    port = original[2] or 5003
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.connect((args.host, port))
        local_ip = probe.getsockname()[0]
    octets = bytes(map(int, local_ip.split(".")))
    own_source = [int.from_bytes(octets[:2], "big"), int.from_bytes(octets[2:], "big")]
    emit("begin", original_registers=original, local_ip=local_ip, port=port)
    stop = threading.Event()
    flood = None
    try:
        # Keep a stream of rejected packets arriving while changing the port.
        # A zero source filter denies every packet, including this test host.
        write(2193, 0)
        write(2194, 0)
        def send_rejected() -> None:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                while not stop.wait(0.02):
                    sock.sendto(b"discard", (args.host, port))
        flood = threading.Thread(target=send_rejected)
        flood.start()
        new_port = 5004 if port != 5004 else 5005
        write(2192, new_port)
        time.sleep(2)
        write(2193, own_source[0])
        write(2194, own_source[1])
        time.sleep(2)
        payload = bytes([0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0])
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.bind((local_ip, 0))
            for _ in range(10):
                sock.sendto(payload, (args.host, new_port))
                time.sleep(0.1)
        current = status()
        expected = list(struct.unpack("<4H", payload))
        actual = read(4, 0x90, 4)
        if not current["udp_multicast"] or current["udp_received_bytes"] != len(payload) or actual != expected:
            raise AssertionError(f"live rebind or UDP image failed: active={current['udp_multicast']} len={current['udp_received_bytes']} data={actual}")
        emit("rebound_under_rejected_traffic", data=actual)
        stop.set()
        flood.join()
        time.sleep(6)
        if status()["udp_multicast"]:
            raise AssertionError("UDP still reported receiving after freshness deadline")
        if read(4, 0x90, 4) != expected:
            raise AssertionError("last-known image unexpectedly destroyed by expiry")
        emit("freshness_expired_with_last_known_image_retained")
    finally:
        stop.set()
        if flood:
            flood.join()
        for offset, value in enumerate(original):
            write(2190 + offset, value)
        time.sleep(2)
        if read(3, 2190, 5) != original:
            raise AssertionError("failed to restore original UDP configuration")
        emit("original_udp_configuration_restored")


if __name__ == "__main__":
    main()
