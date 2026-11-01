#!/usr/bin/env python3
"""Exercise benchmark output restoration with an in-memory socket substitute."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tests/manual'))
spec = importlib.util.spec_from_file_location('tcp_benchmark', ROOT / 'tests/manual/tcp_latency_benchmark.py')
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class Board:
    def __init__(self, inject=False):
        self.bits = 0xFE07
        self.writes = 0
        self.inject = inject
        self.lock = threading.Lock()

    def connect(self, *_args, **_kwargs):
        return Connection(self)

    def receive(self, connection, _transaction):
        pdu = connection.pdu
        with self.lock:
            if pdu[0] == 5:
                bit = int.from_bytes(pdu[1:3], 'big') - 512
                if pdu[3:] == b'\xff\x00':
                    self.bits |= 1 << bit
                else:
                    self.bits &= ~(1 << bit)
                self.writes += 1
                return pdu
            if self.inject and self.writes:
                self.inject = False
                raise ConnectionError('injected read failure after a real output write')
            return b'\x01\x02' + self.bits.to_bytes(2, 'little')


class Connection:
    def __init__(self, board):
        self.board = board
        self.pdu = b''

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()

    def settimeout(self, _value):
        pass

    def sendall(self, frame):
        self.pdu = frame[7:]

    def close(self):
        pass


class CleanupTests(unittest.TestCase):
    def run_case(self, inject):
        board = Board(inject)
        with tempfile.TemporaryDirectory(prefix='tcp-benchmark-cleanup-') as directory:
            path = Path(directory)
            argv = ['benchmark', '127.0.0.1', '--apply', '--backup', str(path/'backup.json'),
                    '--output', str(path/'run.jsonl'), '--duration', '.05', '--interval', '.1']
            with patch.object(sys, 'argv', argv), \
                    patch.object(benchmark.socket, 'create_connection', board.connect), \
                    patch.object(benchmark, 'receive_frame', board.receive), \
                    patch.object(benchmark.time, 'sleep', lambda _: None), \
                    patch.object(benchmark.signal, 'signal', lambda *_: None):
                if inject:
                    with self.assertRaises(SystemExit) as result:
                        benchmark.main()
                    self.assertEqual(result.exception.code, 1)
                else:
                    benchmark.main()
            rows = [json.loads(line) for line in (path/'run.jsonl').read_text().splitlines()]
            self.assertEqual(board.bits, 0xFE07)
            self.assertEqual(rows[-1]['final_bits'], 0xFE07)
            self.assertTrue(all(item['restored'] for item in rows[-1]['results']))
            self.assertEqual(len([row for row in rows if row['event']=='port_restored']), 4)
            if inject:
                self.assertGreater(rows[-1]['errors'], 0)
            else:
                self.assertEqual(rows[-1]['errors'], 0)

    def test_normal_completion_restores_all_four_outputs(self):
        self.run_case(False)

    def test_read_failure_after_write_still_restores_outputs(self):
        self.run_case(True)


if __name__ == '__main__':
    unittest.main()
