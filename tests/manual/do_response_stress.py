#!/usr/bin/env python3
"""Switch unloaded DOs, measure acknowledgements/readback, and restore outputs.

Requires an explicitly authorized unloaded bench, --apply, and a new backup path.
Register readback is not relay-contact feedback. Timings include host/network and
HTTP polling. This script does not claim precise electrical output latency.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import http.cookiejar
import json
import math
import os
from pathlib import Path
import signal
import socket
import struct
import threading
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone

from production_acceptance import receive_frame, request_frame
from stability_soak import read_registers


def emit(event, **fields):
    print(json.dumps(dict(at=datetime.now(timezone.utc).isoformat(), event=event,
                          **fields)), flush=True)


def distribution(values):
    ordered = sorted(values)
    if not ordered:
        return dict(count=0)
    return dict(count=len(ordered), mean_ms=sum(ordered) / len(ordered),
                p95_ms=ordered[math.ceil(.95 * len(ordered)) - 1],
                p99_ms=ordered[math.ceil(.99 * len(ordered)) - 1],
                maximum_ms=ordered[-1])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('host')
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--backup', required=True)
    parser.add_argument('--duration', type=float, default=600)
    parser.add_argument('--idle-duration', type=float, default=60)
    parser.add_argument('--minimum-hold-ms', type=float, default=100)
    parser.add_argument('--confirmation-timeout', type=float, default=2)
    parser.add_argument('--restore-only', action='store_true')
    args = parser.parse_args()
    password = os.environ.get('GW_STABILITY_WEB_PASSWORD')
    if not args.apply or not password:
        parser.error('--apply and GW_STABILITY_WEB_PASSWORD are required')
    if (not math.isfinite(args.duration) or args.duration <= 0
            or not 0 <= args.idle_duration <= args.duration
            or not 100 <= args.minimum_hold_ms <= 10000
            or not .1 <= args.confirmation_timeout <= 10):
        parser.error('invalid duration, phase, hold time or confirmation timeout')
    backup = Path(args.backup)
    if backup.exists() and not args.restore_only:
        parser.error('backup exists; refuse to overwrite recovery state')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base = 'http://' + args.host + '/'

    def api(endpoint, fields=None):
        request = urllib.request.Request(base + endpoint,
            data=None if fields is None else urllib.parse.urlencode(fields).encode())
        with opener.open(request, timeout=5) as response:
            if 'application/json' not in response.headers.get('Content-Type', ''):
                raise PermissionError(
                    f'{endpoint}: expected JSON, status={response.status}, '
                    f'final_url={response.url}, '
                    f'content_type={response.headers.get("Content-Type", "")}; '
                    'stop output test')
            result = json.load(response)
        if str(result.get('code')) != '0':
            raise ValueError(f'{endpoint}: code={result.get("code")}')
        return result.get('data', {})

    api('login', dict(username='admin', pwd=password))
    initial_io = api('getiodata')
    initial_status = api('getsystemstatus')
    count = len(initial_io['do_data'])
    if not 1 <= count <= 64:
        raise ValueError('unsupported DO count')
    mask = (1 << count) - 1

    def target_bits(io):
        return sum(int(io['do_data'][f'do_addr_{n}']) << n for n in range(count))

    if args.restore_only:
        original = json.loads(backup.read_text())
        if original['host'] != args.host or original['count'] != count:
            raise ValueError('backup host or DO count mismatch')
    else:
        original = dict(host=args.host, count=count, bits=target_bits(initial_io),
            state=initial_status, io=initial_io,
            config={p: api(p) for p in ('getnetworkconfig', 'getportconfig',
                                        'getbleconfig', 'getsensorconfig')})
        with backup.open('x') as dest:
            json.dump(original, dest, ensure_ascii=False, indent=2)

    stop = threading.Event()
    load_stop = threading.Event()
    load_results = []
    records = []
    control = None
    transaction = 0

    def modbus(pdu):
        nonlocal control, transaction
        if control is None:
            control = socket.create_connection((args.host, 502), timeout=3)
            control.settimeout(3)
        transaction = (transaction + 1) & 65535
        control.sendall(request_frame(transaction, pdu))
        return receive_frame(control, transaction)

    def read_coils():
        result = modbus(struct.pack('>BHH', 1, 512, count))
        size = (count + 7) // 8
        if len(result) != size + 2 or result[:2] != bytes((1, size)):
            raise ValueError('invalid coil response: ' + result.hex())
        return int.from_bytes(result[2:], 'little') & mask

    def write_bulk(bits):
        data = bits.to_bytes((count + 7) // 8, 'little')
        prefix = struct.pack('>BHH', 15, 512, count)
        if modbus(prefix + bytes((len(data),)) + data) != prefix:
            raise ValueError('FC15 acknowledgement mismatch')

    def observe(expected, previous_generation=None, timeout=None):
        deadline = time.monotonic() + (timeout or args.confirmation_timeout)
        polls = 0
        while time.monotonic() < deadline:
            io = api('getiodata')
            at = time.perf_counter()
            q = io['diagnostics']['io']
            polls += 1
            if q['do_failures'] or q['di_failures'] or q['led_errors']:
                raise ValueError('hardware transaction error: ' + json.dumps(q))
            queue = q['output_queue']
            changed = previous_generation is None or q['do_confirmed_generation'] != previous_generation
            if (q['do_readback_valid'] and q['do_readback_bits'] == expected
                    and target_bits(io) == expected and queue and queue['depth'] == 0
                    and q['do_confirmed_generation'] == queue['requested_generation']
                    and changed):
                if queue['rejected'] or queue['canceled_after_fault']:
                    raise ValueError('output queue error: ' + json.dumps(queue))
                return at, q, polls
            time.sleep(.002)
        raise TimeoutError(f'DO register confirmation timed out: expected={expected:#x}')

    def load_port(port):
        successes = errors = 0
        delays = []
        try:
            with socket.create_connection((args.host, port), timeout=3) as sock:
                sock.settimeout(3)
                while not load_stop.is_set():
                    started = time.perf_counter()
                    read_registers(sock, 1, 4, 0x80, 8)
                    delays.append((time.perf_counter() - started) * 1000)
                    successes += 1
        except Exception as error:
            errors += 1
            emit('background_error', port=port, error=str(error))
            stop.set()
        return dict(port=port, successes=successes, errors=errors,
                    latency=distribution(delays))

    def interrupt(_signum, _frame):
        stop.set()

    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
    error = None
    restored = False
    started = time.monotonic()
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=4)
    futures = []
    try:
        _, q, _ = observe(target_bits(initial_io))
        current = target_bits(initial_io)
        generation = q['do_confirmed_generation']
        emit('start', host=args.host, duration_s=args.duration, count=count,
             original_bits=original['bits'], idle_duration_s=args.idle_duration,
             minimum_hold_ms=args.minimum_hold_ms, reset_count=initial_status['reset_count'])
        progress_at = 0
        step = 0
        while not args.restore_only and not stop.is_set() and time.monotonic() - started < args.duration:
            elapsed = time.monotonic() - started
            phase = 'idle' if elapsed < args.idle_duration else 'loaded'
            if phase == 'loaded' and not futures:
                futures = [pool.submit(load_port, port) for port in (502, 503, 504, 5002)]
                emit('background_started', ports=[502, 503, 504, 5002])
            method = ('fc05', 'web', 'fc15')[step % 3]
            channel = (step // 3) % count
            expected = current ^ (1 << channel)
            if method == 'fc15':
                patterns = [0, mask, int('55' * ((count + 7) // 8), 16) & mask,
                            int('aa' * ((count + 7) // 8), 16) & mask,
                            1 << channel, mask ^ (1 << channel)]
                expected = patterns[(step // 3) % len(patterns)]
                if expected == current:
                    expected ^= 1 << channel
            t0 = time.perf_counter()
            if method == 'web':
                api('iocontrol', dict(addr=channel, value=(expected >> channel) & 1))
            elif method == 'fc05':
                pdu = struct.pack('>BHH', 5, 512 + channel,
                                  0xFF00 if expected & (1 << channel) else 0)
                if modbus(pdu) != pdu:
                    raise ValueError('FC05 acknowledgement mismatch')
            else:
                write_bulk(expected)
            ack_at = time.perf_counter()
            coil_started = time.perf_counter()
            actual = read_coils()
            coil_at = time.perf_counter()
            if actual != expected:
                raise ValueError(f'coil target mismatch: {actual:#x} != {expected:#x}')
            confirmed_at, q, polls = observe(expected, generation)
            record = dict(phase=phase, method=method, channel=channel + 1,
                before=current, target=expected, write_ack_ms=(ack_at-t0)*1000,
                coil_read_ms=(coil_at-coil_started)*1000,
                coil_visible_ms=(coil_at-t0)*1000,
                register_observed_ms=(confirmed_at-t0)*1000, polls=polls,
                readback_age_ms=q['do_readback_age_ms'],
                confirmed_generation=q['do_confirmed_generation'],
                queue_high_water=q['output_queue']['high_water'])
            record['submit_busy'] = q.get('do_submit_busy')
            records.append(record)
            emit('do_confirmed', **record)
            generation = q['do_confirmed_generation']
            current = expected
            step += 1
            if elapsed >= progress_at:
                state = api('getsystemstatus')
                if state['reset_count'] != initial_status['reset_count']:
                    raise ValueError('unexpected device reset')
                emit('progress', elapsed_s=time.monotonic()-started,
                     writes=len(records), state=state, diagnostics=q)
                progress_at += 30
            stop.wait(max(0, args.minimum_hold_ms / 1000 - (time.perf_counter()-t0)))
    except Exception as caught:
        error = caught
        emit('test_error', error=str(caught))
    finally:
        load_stop.set()
        for future in futures:
            load_results.append(future.result())
        pool.shutdown(wait=True)
        # Restore by Modbus even if the single Web session was replaced.
        try:
            if control is not None:
                control.close()
                control = None
            write_bulk(original['bits'])
            try:
                observe(original['bits'], timeout=5)
            except PermissionError as changed:
                # Preserve the test failure; a new session is only for verifying
                # cleanup, never for silently resuming the measured window.
                emit('restore_session_replaced', error=str(changed))
                api('login', dict(username='admin', pwd=password))
                observe(original['bits'], timeout=5)
            time.sleep(2.2)  # Allow existing DO persistence task to see restored bits.
            observe(original['bits'], timeout=5)
            state = api('getsystemstatus')
            if state['reset_count'] != initial_status['reset_count']:
                raise ValueError('reset count changed')
            if not args.restore_only:
                for endpoint, before in original['config'].items():
                    if api(endpoint) != before:
                        raise ValueError('configuration changed: ' + endpoint)
            restored = True
            emit('restored', bits=original['bits'], state=state)
        except Exception as caught:
            emit('restore_error', error=str(caught), backup=str(backup))
            if error is None:
                error = caught
        if control is not None:
            control.close()
    for phase in ('idle', 'loaded'):
        for method in ('fc05', 'web', 'fc15'):
            subset = [r for r in records if r['phase'] == phase and r['method'] == method]
            emit('latency_summary', phase=phase, method=method,
                 **{key: distribution([r[key] for r in subset]) for key in
                    ('write_ack_ms', 'coil_read_ms', 'coil_visible_ms', 'register_observed_ms')})
    for result in load_results:
        emit('background_summary', **result)
    emit('end', duration_s=time.monotonic()-started, writes=len(records),
         errors=int(error is not None), restored=restored, interrupted=stop.is_set(),
         background_errors=sum(r['errors'] for r in load_results))
    if error is not None or not restored or any(r['errors'] for r in load_results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
