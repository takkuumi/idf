#!/usr/bin/env python3
"""Bench-only invalid-port rejection, atomic swaps and live rebind; restores ports."""
from __future__ import annotations
import argparse
import json
import socket
import struct
import time
from production_acceptance import receive_frame, request_frame
from stability_soak import read_registers


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if not args.apply:
        parser.error("--apply required: this temporarily changes Modbus listeners")
    sock = socket.create_connection((args.host, 502), timeout=3)
    sock.settimeout(3)
    original = read_registers(sock, 1, 3, 2243, 4)
    print(json.dumps({"event": "ports_backed_up", "ports": original}), flush=True)

    def write(values, single=False, reject=False):
        if single:
            pdu = struct.pack(">BHH", 6, 2243, values[0])
        else:
            pdu = struct.pack(">BHHB", 16, 2243, len(values), 2 * len(values)) + struct.pack(f">{len(values)}H", *values)
        sock.sendall(request_frame(950, pdu))
        result = receive_frame(sock, 950)
        if reject:
            if len(result) != 2 or result[0] != (pdu[0] | 0x80):
                raise AssertionError("invalid port set was acknowledged as successful")
        elif result != (pdu if single else pdu[:5]):
            raise AssertionError("valid port update failed")

    try:
        for values, single in [([original[1]], True), ([0], True), ([80, 1503, 1504, 1505], False), ([1502, 1502, 1504, 1505], False)]:
            write(values, single, reject=True)
            if read_registers(sock, 1, 3, 2243, 4) != original:
                raise AssertionError("rejected request partially modified ports")
        swapped = [original[1], original[0], original[2], original[3]]
        write(swapped)
        if read_registers(sock, 1, 3, 2243, 4) != swapped:
            raise AssertionError("atomic swap failed")
        temporary = [1502, 1503, 1504, 1505]
        write(temporary)
        time.sleep(2)
        with socket.create_connection((args.host, 1502), timeout=3) as probe:
            probe.settimeout(3)
            if read_registers(probe, 1, 3, 2243, 4) != temporary:
                raise AssertionError("live rebind failed")
        print(json.dumps({"event": "invalid_rejection_atomic_swap_and_live_rebind_passed"}), flush=True)
    finally:
        write(original)
        time.sleep(2)
        sock.close()
        with socket.create_connection((args.host, original[0]), timeout=3) as probe:
            probe.settimeout(3)
            if read_registers(probe, 1, 3, 2243, 4) != original:
                raise AssertionError("failed to restore original ports")
        print(json.dumps({"event": "original_ports_restored"}), flush=True)


if __name__ == "__main__":
    main()
