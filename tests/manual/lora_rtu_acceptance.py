#!/usr/bin/env python3
"""Read-only transparent E22/RTU peer acceptance against a known TCP device.

Open ONLY the external USB-E22 peer. The board UART0 USB route may disconnect
the onboard radio electrically while VBUS is present; software cannot claim an
RF pass when that route is blocked. Does not change radio/UART settings, GPIO,
outputs or persistent configuration. Serial bytes and all failed attempts are
retained; positive RF payloads must match the same request over Ethernet.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import socket
import statistics
import struct
import time
import serial


def crc16(data):
    crc = 0xffff
    for b in data:
        crc ^= b
        for _ in range(8): crc = (crc >> 1) ^ (0xa001 if crc & 1 else 0)
    return crc


def tcp_read(host, function, address, count):
    pdu = struct.pack('>BHH', function, address, count)
    with socket.create_connection((host, 502), 3) as s:
        s.settimeout(3); s.sendall(struct.pack('>HHHB', 0x7320, 0, len(pdu) + 1, 1) + pdu)
        data = b''
        while len(data) < 7:
            part = s.recv(7-len(data))
            if not part: raise RuntimeError('TCP header truncated')
            data += part
        length = int.from_bytes(data[4:6], 'big')
        while len(data) < 6+length:
            part = s.recv(6+length-len(data))
            if not part: raise RuntimeError('TCP payload truncated')
            data += part
        assert data[:4] == b'\x73\x20\x00\x00' and data[6] == 1
        return data[6:]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--port', required=True); p.add_argument('--host', required=True)
    p.add_argument('--baud', type=int, default=9600); p.add_argument('--count', type=int, default=100)
    p.add_argument('--timeout', type=float, default=5); p.add_argument('--interval', type=float, default=.1)
    p.add_argument('--out', type=Path, required=True); p.add_argument('--expected-version', type=int, default=320)
    a = p.parse_args(); a.out.mkdir(parents=True, exist_ok=True)
    cases = [(4, 0x087e, 1), (3, 2263, 6), (4, 0x0080, 4), (1, 512, 16), (2, 0, 16), (3, 2300, 60), (3, 5000, 120)]
    stable = {(fc, addr, n): tcp_read(a.host, fc, addr, n) for fc, addr, n in cases if fc == 3 or addr == 0x087e}
    assert stable[cases[0]] == bytes([1,4,2]) + a.expected_version.to_bytes(2, 'big')
    latencies = []; failures = []; first_bytes = []
    with (a.out / 'events.jsonl').open('w') as logfile:
        def emit(event, **data):
            row = {'at': datetime.now(timezone.utc).isoformat(), 'event': event, **data}; line = json.dumps(row)
            logfile.write(line + '\n'); logfile.flush(); print(line, flush=True)
        source = serial.Serial(port=None, baudrate=a.baud, timeout=.01, write_timeout=1)
        source.dtr = False; source.rts = False; source.port = a.port
        with source:
            # Preserve and report startup bytes before the measured phase.
            start = time.monotonic(); startup = bytearray()
            while time.monotonic() - start < 2: startup.extend(source.read(256))
            emit('serial_opened', port=a.port, baud=a.baud, startup_bytes=startup.hex(), measured_requests=a.count)
            for index in range(a.count):
                case = cases[index % len(cases)]
                fc, address, count = case
                payload = struct.pack('>BBHH', 1, fc, address, count)
                frame = payload + struct.pack('<H', crc16(payload))
                start = time.monotonic(); source.write(frame); source.flush()
                response = bytearray(); first = None; expected_length = None
                while time.monotonic() - start < a.timeout:
                    chunk = source.read(256)
                    if chunk:
                        if first is None: first = (time.monotonic() - start) * 1000
                        response.extend(chunk)
                        if len(response) >= 3:
                            expected_length = 5 if response[1] & 0x80 else 5 + response[2]
                            if len(response) >= expected_length: break
                elapsed = (time.monotonic() - start) * 1000
                try:
                    assert expected_length is not None and len(response) == expected_length, 'timeout/truncated/extra bytes'
                    assert crc16(response[:-2]) == int.from_bytes(response[-2:], 'little'), 'CRC mismatch'
                    assert response[0] == 1 and response[1] == fc, 'wrong unit/function/exception'
                    expected_size = count * 2 if fc in (3, 4) else (count + 7) // 8
                    assert response[2] == expected_size, 'wrong byte count'
                    if case in stable: assert response[:-2] == stable[case], 'RF payload differs from TCP baseline'
                    latencies.append(elapsed); first_bytes.append(first)
                    emit('response', index=index, request=frame.hex(), response=response.hex(), first_byte_ms=first, complete_ms=elapsed)
                except AssertionError as error:
                    failure = {'index': index, 'request': frame.hex(), 'response': response.hex(), 'error': str(error), 'ms': elapsed}
                    failures.append(failure); emit('failure', **failure)
                    # Do not hide missing RF routes behind a long pseudo-soak.
                    if len(failures) >= 3 and not latencies: break
                time.sleep(a.interval)
    def metrics(values):
        ordered = sorted(values)
        return {'mean': statistics.mean(values), 'p95': ordered[int((len(ordered)-1)*.95)], 'max': max(values)} if values else None
    summary = {'passed': len(latencies) == a.count and not failures, 'requests_ok': len(latencies), 'failures': failures, 'port': a.port, 'baud': a.baud, 'cases': cases, 'first_byte_ms': metrics(first_bytes), 'complete_ms': metrics(latencies)}
    (a.out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n'); print(json.dumps(summary))
    assert summary['passed'], 'see retained RF request/response log; not an RF acceptance pass'

if __name__ == '__main__': main()
