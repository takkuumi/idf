#!/usr/bin/env python3
"""Bench-only UART0 route test; --apply authorizes DTR/RTS resets.

Waits for the board download adapter to be reconnected while the external E22
peer remains connected. Releases BOOT and pulses EN, verifies the application
over TCP, then probes RF with the download USB still connected. It does not
flash, change MCU GPIO, send configuration commands, or change persisted data.
Always returns control lines to the released state and restarts the application.
"""
import argparse
import json
from pathlib import Path
import socket
import struct
import time
from datetime import datetime, timezone
import serial
from serial.tools import list_ports
from lora_rtu_acceptance import crc16, tcp_read


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--apply', action='store_true'); p.add_argument('--peer', required=True)
    p.add_argument('--host', required=True); p.add_argument('--wait', type=int, default=180)
    p.add_argument('--out', type=Path, required=True); a = p.parse_args()
    if not a.apply: p.error('--apply is required for control-line reset testing')
    a.out.mkdir(parents=True, exist_ok=True)
    with (a.out / 'events.jsonl').open('w') as logfile:
        def emit(event, **data):
            line = json.dumps({'at': datetime.now(timezone.utc).isoformat(), 'event': event, **data})
            logfile.write(line+'\n'); logfile.flush(); print(line, flush=True)
        emit('waiting_for_board_download_adapter', peer=a.peer)
        start = time.monotonic(); board = None
        while time.monotonic() - start < a.wait:
            other = [p.device for p in list_ports.comports() if p.vid == 0x1a86 and p.pid == 0x7523 and p.device != a.peer]
            if len(other) == 1: board = other[0]; break
            time.sleep(1)
        if board is None:
            emit('board_adapter_not_reconnected', tests_started=False)
            return
        def app_ready(timeout=8):
            deadline = time.monotonic()+timeout
            while time.monotonic()<deadline:
                try:
                    value = list(struct.unpack('>2H', tcp_read(a.host, 4, 0x087e, 2)[3:]))
                    if value == [320, 1557]: return value
                except (OSError, AssertionError, ValueError): pass
                time.sleep(.3)
            return None
        def normal_reset(handle):
            handle.dtr = False; handle.rts = False; time.sleep(.1)
            handle.rts = True; time.sleep(.15)
            handle.rts = False; time.sleep(.5)
        control = serial.Serial(port=None, baudrate=115200, timeout=.01)
        control.dtr = False; control.rts = False; control.port = board
        radio = serial.Serial(port=None, baudrate=9600, timeout=.01, write_timeout=1)
        radio.dtr = False; radio.rts = False; radio.port = a.peer
        results = []
        with control, radio:
            try:
                normal_reset(control)
                version = app_ready(); assert version is not None, 'application did not start after normal reset'
                emit('forced_normal_application_verified', board=board, version_date=version)
                # FF is released, TT deasserts the usual two-transistor reset
                # interlock; TF can affect BOOT while the application is running.
                for dtr, rts in [(False, False), (True, True), (True, False)]:
                    control.dtr = dtr; control.rts = rts; time.sleep(.5)
                    version = app_ready()
                    if version is None:
                        row = {'dtr':dtr,'rts':rts,'application_ready':False,'rf_test_started':False}
                        results.append(row); emit('line_state', **row)
                        normal_reset(control); assert app_ready() is not None
                        continue
                    # Drain old bytes before exactly one known request.
                    t = time.monotonic(); discarded = bytearray()
                    while time.monotonic()-t<1: discarded.extend(radio.read(512))
                    request = bytes.fromhex('0104087e000213b3'); response = bytearray(); t = time.monotonic()
                    radio.write(request); radio.flush()
                    while time.monotonic()-t<10:
                        response.extend(radio.read(256))
                        if len(response)>=9: break
                    good = (len(response)==9 and response[:7]==bytes.fromhex('01040401400615')
                            and crc16(response[:-2])==int.from_bytes(response[-2:],'little'))
                    row = {'dtr':dtr,'rts':rts,'application_ready':True,'version_date':version,'request':request.hex(),
                           'response':response.hex(),'rf_valid':good,'elapsed_ms':(time.monotonic()-t)*1000,'discarded_hex':discarded.hex()}
                    results.append(row); emit('line_state', **row)
            finally:
                normal_reset(control); ready = app_ready()
                emit('released_lines_application_restored', application_ready=ready is not None)
        summary = {'board':board,'peer':a.peer,'results':results,'application_restored':ready is not None,
                   'rf_works_with_download_USB':any(r.get('rf_valid') for r in results)}
        (a.out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        emit('summary',**summary)

if __name__ == '__main__': main()
