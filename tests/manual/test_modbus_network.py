#!/usr/bin/env python3
"""Read and validate the gateway IP/mask/gateway holding-register layout."""

from __future__ import annotations

import argparse
import socket
import struct

IP_BASE = 2247
REGISTER_COUNT = 12


def recv_exact(sock: socket.socket, count: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < count:
        chunk = sock.recv(count - len(chunks))
        if not chunk:
            raise ConnectionError("Modbus TCP peer closed the connection")
        chunks.extend(chunk)
    return bytes(chunks)


def read_registers(host: str, port: int, unit: int) -> list[int]:
    transaction_id = 1
    pdu = struct.pack(">BHH", 0x03, IP_BASE, REGISTER_COUNT)
    request = struct.pack(">HHHB", transaction_id, 0, len(pdu) + 1, unit) + pdu
    with socket.create_connection((host, port), timeout=4) as sock:
        sock.settimeout(4)
        sock.sendall(request)
        header = recv_exact(sock, 7)
        received_tid, protocol_id, length, received_unit = struct.unpack(">HHHB", header)
        if received_tid != transaction_id or protocol_id != 0 or received_unit != unit:
            raise ValueError("unexpected Modbus TCP response header")
        if not 2 <= length <= 254:
            raise ValueError(f"invalid MBAP length: {length}")
        pdu_response = recv_exact(sock, length - 1)

    if pdu_response[0] == (0x03 | 0x80):
        raise ValueError(f"Modbus exception code 0x{pdu_response[1]:02X}")
    if len(pdu_response) != 2 + REGISTER_COUNT * 2 or pdu_response[0] != 0x03:
        raise ValueError(f"unexpected FC03 response: {pdu_response.hex()}")
    if pdu_response[1] != REGISTER_COUNT * 2:
        raise ValueError(f"unexpected byte count: {pdu_response[1]}")
    return list(struct.unpack(">12H", pdu_response[2:]))


def format_ipv4(values: list[int]) -> str:
    if len(values) != 4 or any(value > 255 for value in values):
        raise ValueError(f"register values are not IPv4 octets: {values}")
    return ".".join(str(value) for value in values)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host", help="gateway IPv4 address")
    parser.add_argument("--port", type=int, default=502)
    parser.add_argument("--unit", type=int, default=1)
    args = parser.parse_args()

    values = read_registers(args.host, args.port, args.unit)
    fields = {
        "IP": format_ipv4(values[0:4]),
        "Mask": format_ipv4(values[4:8]),
        "Gateway": format_ipv4(values[8:12]),
    }
    print(f"Modbus TCP {args.host}:{args.port}, registers {IP_BASE}..{IP_BASE + 11}")
    for name, address in fields.items():
        print(f"{name:7} {address}")


if __name__ == "__main__":
    main()
