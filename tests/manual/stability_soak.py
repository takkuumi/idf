#!/usr/bin/env python3
"""Read-only Modbus TCP soak for the gateway's four compatibility ports."""

from __future__ import annotations

import argparse
import socket
import struct
import time
from datetime import datetime, timezone


def read_registers(sock: socket.socket, unit: int, function: int, address: int, count: int) -> list[int]:
    transaction = int(time.monotonic_ns()) & 0xFFFF
    pdu = struct.pack(">BHH", function, address, count)
    request = struct.pack(">HHHB", transaction, 0, len(pdu) + 1, unit) + pdu
    sock.sendall(request)
    header = bytearray()
    while len(header) < 7:
        part = sock.recv(7 - len(header))
        if not part:
            raise ConnectionError("peer closed during MBAP header")
        header.extend(part)
    tid, protocol, length, response_unit = struct.unpack(">HHHB", header)
    if tid != transaction or protocol != 0 or response_unit != unit or not 2 <= length <= 254:
        raise ValueError(f"invalid MBAP header {header.hex()}")
    body = bytearray()
    while len(body) < length - 1:
        part = sock.recv(length - 1 - len(body))
        if not part:
            raise ConnectionError("peer closed during PDU")
        body.extend(part)
    if body[0] == function | 0x80:
        raise ValueError(f"Modbus exception 0x{body[1]:02X}")
    if len(body) != count * 2 + 2 or body[0] != function or body[1] != count * 2:
        raise ValueError(f"invalid response PDU {body.hex()}")
    return list(struct.unpack(f">{count}H", body[2:]))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--ports", type=int, nargs="+", default=[502, 503, 504, 5002])
    parser.add_argument("--unit", type=int, default=1)
    parser.add_argument(
        "--expected-version",
        type=lambda value: int(value, 0),
        default=None,
        help="fail if FC04 firmware-version register differs (for example 0x00E4)",
    )
    parser.add_argument("--duration", type=int, default=600)
    parser.add_argument("--interval", type=float, default=1.0)
    args = parser.parse_args()

    started = time.monotonic()
    total = successes = errors = 0
    connections: dict[int, socket.socket | None] = {port: None for port in args.ports}
    print(f"start={datetime.now(timezone.utc).isoformat()} host={args.host} ports={args.ports} duration={args.duration}s")
    while time.monotonic() - started < args.duration:
        cycle_started = time.monotonic()
        for port in args.ports:
            total += 1
            try:
                sock = connections[port]
                if sock is None:
                    sock = socket.create_connection((args.host, port), timeout=3)
                    sock.settimeout(3)
                    connections[port] = sock
                ai = read_registers(sock, args.unit, 4, 0x0080, 8)
                status = read_registers(sock, args.unit, 4, 0x0088, 8)
                holding = read_registers(sock, args.unit, 3, 2280, 16)
                version = read_registers(sock, args.unit, 4, 0x087E, 2)
                if args.expected_version is not None and version[0] != args.expected_version:
                    raise ValueError(
                        f"firmware version mismatch: expected=0x{args.expected_version:04x} "
                        f"actual=0x{version[0]:04x}"
                    )
                successes += 1
                print(f"ok port={port} ai={ai} status={status} holding_crc={sum(holding) & 0xffff:04x} fw={version[0]:04x}/{version[1]:04x}")
            except Exception as error:  # log each transport/protocol failure for soak statistics
                errors += 1
                print(f"error port={port} type={type(error).__name__} detail={error}")
                if connections[port] is not None:
                    connections[port].close()
                    connections[port] = None
        elapsed = time.monotonic() - cycle_started
        time.sleep(max(0.0, args.interval - elapsed))
    print(f"end={datetime.now(timezone.utc).isoformat()} duration={time.monotonic()-started:.1f}s total={total} ok={successes} errors={errors}")
    for sock in connections.values():
        if sock is not None:
            sock.close()
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
