#!/usr/bin/env python3
"""Real-device BLE acceptance; writes require --persistence and are restored.

No network settings, UART parameters, logic data, or output points are changed.
The optional persistence test changes only the location field, requests a reset,
checks the marker, then restores the exact original words and reboots again.
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import re
import statistics
import struct
import time
from datetime import datetime, timezone, timedelta
from pathlib import Path

from bleak import BleakClient, BleakScanner

SERVICE = "4fafc201-1fb5-459e-8fcc-c5c9c331914b"
CHAR = "beb5483e-36e1-4688-b7f5-ea07361b26a8"


def crc16(data):
    value = 0xFFFF
    for byte in data:
        value ^= byte
        for _ in range(8):
            value = (value >> 1) ^ (0xA001 if value & 1 else 0)
    return value


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int((len(ordered) - 1) * fraction))] if ordered else None


class Probe:
    def __init__(self, output, mac, expected_version):
        self.expected_version = bytes(map(int, expected_version.split(".")))
        self.out = output
        self.mac = bytes.fromhex(mac.replace(":", ""))
        self.tx = 0x7100
        self.client = None
        self.device = None
        self.pending = {}
        self.text = asyncio.Queue()
        self.buffer = bytearray()
        self.metrics = []
        self.errors = []
        self.unsolicited = 0
        self.unsolicited_types = {}
        self.last_reset_count = None
        self.protocol_errors = []
        self.logfile = (output / "events.jsonl").open("a")

    def emit(self, event, **fields):
        row = dict(at=datetime.now(timezone(timedelta(hours=8))).isoformat(), event=event, **fields)
        self.logfile.write(json.dumps(row, ensure_ascii=False) + "\n")
        self.logfile.flush()
        if event != "exchange":
            print(json.dumps(row, ensure_ascii=False), flush=True)

    def notify(self, _, data):
        self.buffer.extend(data)
        while self.buffer:
            if self.buffer.startswith((b"OK", b"ERROR")):
                if b"\n" not in self.buffer:
                    return
                end = self.buffer.index(b"\n") + 1
                line = bytes(self.buffer[:end]).decode("utf-8", errors="replace").strip()
                del self.buffer[:end]
                self.text.put_nowait(line)
                continue
            if len(self.buffer) < 6:
                return
            length = int.from_bytes(self.buffer[4:6], "big")
            if self.buffer[2:4] != b"\x00\x00" or not 2 <= length <= 504:
                self.protocol_errors.append(bytes(self.buffer[:6]).hex())
                del self.buffer[0]
                continue
            size = 6 + length + 2
            if len(self.buffer) < size:
                return
            frame = bytes(self.buffer[:size])
            del self.buffer[:size]
            if crc16(frame[:-2]) != int.from_bytes(frame[-2:], "little"):
                self.protocol_errors.append("crc:" + frame.hex())
                continue
            transaction = int.from_bytes(frame[:2], "big")
            payload = frame[6:-2]
            # AT replies also use the length/CRC wrapper, with transaction 0.
            if transaction == 0 and payload.startswith((b"OK", b"ERROR")):
                self.text.put_nowait(payload.decode("utf-8", errors="replace").strip())
                continue
            future = self.pending.get(transaction)
            if future and not future.done():
                future.set_result(frame[6:-2])
            else:
                self.unsolicited += 1
                kind = ('heartbeat' if transaction == 0 and len(payload) == 10 and payload[:2] == b'\x01\x11'
                        else 'di_report' if transaction == 1 and payload[:2] == b'\x01\x94' else 'other')
                self.unsolicited_types[kind] = self.unsolicited_types.get(kind, 0) + 1
                self.emit("unsolicited_frame", kind=kind, tx=transaction, payload=payload.hex())

    async def connect(self):
        if self.device is None:
            devices = await BleakScanner.discover(timeout=10, return_adv=True)
            candidates = [d for d, a in devices.values() if SERVICE in (a.service_uuids or [])]
        else:
            candidates = [self.device]
        for device in candidates:
            start = time.monotonic()
            self.client = BleakClient(device, timeout=20)
            await self.client.connect()
            self.buffer.clear()
            self.text = asyncio.Queue()
            await self.client.start_notify(CHAR, self.notify)
            try:
                mac = await self.custom(0xC6)
                if mac != self.mac:
                    await self.disconnect()
                    continue
                self.device = device
                self.emit("connected", mac=mac.hex(":"), duration_ms=round((time.monotonic()-start)*1000, 3))
                return
            except BaseException:
                await self.disconnect()
                raise
        raise RuntimeError("target IMC service/MAC not found")

    async def disconnect(self):
        if self.client is not None:
            await self.client.disconnect()
            self.client = None
        self.buffer.clear()

    async def exchange(self, function, data=b"", timeout=8):
        self.tx = (self.tx + 1) & 0xFFFF
        transaction = self.tx
        payload = bytes((1, function)) + data
        frame = struct.pack(">HHH", transaction, 0, len(payload)) + payload
        frame += struct.pack("<H", crc16(frame))
        future = asyncio.get_running_loop().create_future()
        self.pending[transaction] = future
        start = time.monotonic()
        try:
            # Force transport reassembly without relying on platform long writes.
            for offset in range(0, len(frame), 20):
                await self.client.write_gatt_char(CHAR, frame[offset:offset+20], response=True)
            response = await asyncio.wait_for(future, timeout)
            latency = (time.monotonic()-start)*1000
            self.metrics.append(latency)
            self.emit("exchange", function=hex(function), tx=transaction,
                      request=data.hex(), response=response.hex(), latency_ms=round(latency,3))
            return response
        except Exception as error:
            self.emit("exchange_error", function=hex(function), tx=transaction,
                      request=data.hex(), error=repr(error), latency_ms=round((time.monotonic()-start)*1000,3))
            raise
        finally:
            self.pending.pop(transaction, None)

    async def custom(self, function, data=b""):
        response = await self.exchange(function, data)
        if response[:2] != bytes((function, 0)):
            raise RuntimeError(f"custom {function:02x}: {response.hex()}")
        return response[2:]

    async def registers(self, function, address, count):
        # The stock handheld's exact location read is a variable-length ASCII
        # alias. Read one extra register to request the raw Modbus word view.
        if function == 3 and address == 2205 and count == 8:
            return (await self.registers(3, address, count + 1))[:count]
        response = await self.exchange(function, struct.pack(">HH", address, count))
        if response[:3] != bytes((1, function, count*2)) or len(response) != 3+count*2:
            raise RuntimeError(f"register {function}/{address}/{count}: {response.hex()}")
        return list(struct.unpack(">"+"H"*count, response[3:]))

    async def read_span(self, address, count):
        words = []
        for offset in range(0, count, 120):
            words += await self.registers(3, address+offset, min(120, count-offset))
        return words

    async def at(self, command):
        while not self.text.empty():
            self.text.get_nowait()
        await self.client.write_gatt_char(CHAR, command.encode()+b"\r\n", response=True)
        line = await asyncio.wait_for(self.text.get(), 8)
        self.emit("at", command=command, response=line)
        if not line.startswith("OK"):
            raise RuntimeError(line)
        if command == "AT+STATUS":
            self.last_reset_count = int(re.search(r"rst=(\d+)", line).group(1))
        return line

    async def write_words(self, address, words):
        payload = struct.pack(">HHB", address, len(words), len(words)*2)
        payload += struct.pack(">"+"H"*len(words), *words)
        response = await self.exchange(0x10, payload)
        expected = bytes((1, 0x10))+struct.pack(">HH", address, len(words))
        if response != expected:
            raise RuntimeError(f"write {address}: {response.hex()}")

    async def reboot(self):
        previous_resets = self.last_reset_count
        # Planned reset is requested via the same public coil as the PC tool.
        try:
            response = await self.exchange(5, struct.pack(">HH", 0x0402, 0xFF00), timeout=4)
            self.emit("reset_ack", response=response.hex())
        except (asyncio.TimeoutError, Exception) as error:
            self.emit("reset_ack_lost", error=str(error))
        await self.disconnect()
        await asyncio.sleep(4)
        last = None
        for _ in range(6):
            try:
                await self.connect()
                await self.at("AT+STATUS")
                if previous_resets is None or self.last_reset_count != (previous_resets + 1) % 65536:
                    raise RuntimeError(f"planned reset counter mismatch: {previous_resets} -> {self.last_reset_count}")
                self.emit("planned_reset_count_verified", before=previous_resets, after=self.last_reset_count)
                return
            except Exception as error:
                last = error
                await self.disconnect()
                await asyncio.sleep(2)
        raise RuntimeError(f"device did not return after reset: {last}")

    async def snapshot(self):
        snapshot = {}
        for function, label in [(0xCF,"firmware"),(0xCE,"network"),(0xC2,"sn"),(0xC4,"location"),(0xC8,"tcp_port"),(0xCA,"ble_id"),(0xCC,"model")]:
            snapshot[label] = (await self.custom(function)).hex()
        snapshot["location_words"] = await self.registers(3, 2205, 8)
        snapshot["calibration"] = await self.registers(3, 2280, 16)
        snapshot["port_words"] = await self.registers(3, 2214, 15)
        snapshot["logic"] = await self.read_span(2300, 1700)
        snapshot["text"] = await self.read_span(5000, 2000)
        for key in ("logic", "text"):
            digest=hashlib.sha256(struct.pack(">"+"H"*len(snapshot[key]),*snapshot[key])).hexdigest()
            self.emit("snapshot", region=key, words=len(snapshot[key]), sha256=digest)
        return snapshot

    async def exercise(self, seconds):
        before = await self.at("AT+STATUS")
        for command in ["AT+VERSION", "AT+CFGINFO", "AT+CFG485=0", "AT+CFG485=1", "AT+CFG485=2"]:
            await self.at(command)
        self.emit("metadata", values=await self.registers(4,0x0800,6))
        health_before = await self.registers(4,0x0880,5)
        self.emit("health_before", values=health_before)
        baseline = await self.snapshot()
        assert bytes.fromhex(baseline['firmware'])[:3] == self.expected_version
        (self.out/"baseline.json").write_text(json.dumps(baseline,indent=2)+"\n")
        for function,data,expected in [
            (3, struct.pack(">HH",2280,0), bytes((1,0x83,3))),
            (4, struct.pack(">HH",65535,1), bytes((1,0x84,2))),
            (0x7F,b"",bytes((1,0xFF,1))),
            (0x10,bytes.fromhex("08e8000100"),bytes((1,0x90,3))),
        ]:
            response=await self.exchange(function,data)
            assert response==expected,(function,response.hex(),expected.hex())
        self.emit("invalid_requests_rejected",count=4)
        start=time.monotonic(); cycles=0
        while time.monotonic()-start < seconds:
            try:
                version=await self.custom(0xCF)
                assert version[:3]==self.expected_version
                await self.registers(4,0x0080,4)
                await self.registers(4,0x0880,5)
                await self.registers(3,2280,16)
                await self.registers(3,2300,60)
                for function,address in [(1,512),(2,0)]:
                    response=await self.exchange(function,struct.pack(">HH",address,16))
                    assert response[:3]==bytes((1,function,2)),response.hex()
                cycles+=1
                if cycles%10==0:self.emit("pressure_progress",cycles=cycles,elapsed_s=round(time.monotonic()-start,1))
            except Exception as error:
                self.errors.append(repr(error));self.emit("pressure_error",error=repr(error))
                break
        after=await self.at("AT+STATUS")
        rst=lambda s:int(re.search(r"rst=(\d+)",s).group(1))
        assert rst(before)==rst(after),"unexpected reset during read pressure"
        health_after = await self.registers(4,0x0880,5)
        self.emit("pressure_done",cycles=cycles,elapsed_s=round(time.monotonic()-start,2),reset_count=rst(after),health=health_after)
        assert health_after == health_before, "fault/drop counters changed during read-only pressure"
        final=await self.snapshot()
        assert final==baseline,"read-only pressure changed persistent configuration"
        return baseline

    async def logic_roundtrip(self, baseline):
        words = baseline["logic"]
        count = words[0]
        header_size = count + 2
        terminal = words[count + 1]
        if not 0 < count <= 32 or not header_size < terminal <= len(words):
            raise RuntimeError("logic roundtrip requires a nonempty valid existing table")
        header = words[:header_size]
        for immediate_disconnect in (False, True):
            await self.write_words(2300, header)
            chunks = list(range(header_size, terminal, 60))
            for index, offset in enumerate(chunks):
                chunk = words[offset:min(offset + 60, terminal)]
                if immediate_disconnect and index == len(chunks) - 1:
                    self.tx = (self.tx + 1) & 0xFFFF
                    payload = bytes((1, 0x10)) + struct.pack(">HHB", 2300 + offset, len(chunk), len(chunk) * 2)
                    payload += struct.pack(">" + "H" * len(chunk), *chunk)
                    frame = struct.pack(">HHH", self.tx, 0, len(payload)) + payload
                    frame += struct.pack("<H", crc16(frame))
                    for position in range(0, len(frame), 20):
                        await self.client.write_gatt_char(CHAR, frame[position:position+20], response=True)
                    self.emit("logic_final_att_submitted", protocol_ack_awaited=False)
                    await self.disconnect()
                    await asyncio.sleep(.3)
                    await self.connect()
                else:
                    await self.write_words(2300 + offset, chunk)
            current = await self.read_span(2300, len(words))
            if current != words:
                raise RuntimeError("logic contents changed while resending the original table")
            self.emit("logic_roundtrip_verified", immediate_disconnect=immediate_disconnect,
                      count=count, active_words=terminal, expected_flash_commits=1)

    async def persistence(self, baseline):
        original=baseline["location_words"]
        marker=b"VERIFY-SAVE".ljust(16,b"\x00")
        marker_words=list(struct.unpack(">8H",marker))
        (self.out/"restore-location.json").write_text(json.dumps({"address":2205,"original":original,"marker":marker_words},indent=2)+"\n")
        changed=False
        try:
            changed=True
            await self.write_words(2205,marker_words)
            await self.reboot()
            current=await self.registers(3,2205,8)
            assert current==marker_words,("marker lost after immediate reset",current)
            self.emit("persist_marker_verified",location_words=current)
            assert await self.read_span(2300,1700)==baseline['logic'],"logic changed across reset"
            assert await self.read_span(5000,2000)==baseline['text'],"text changed across reset"
        finally:
            if changed:
                if not self.client or not self.client.is_connected:await self.connect()
                await self.write_words(2205,original)
                await self.reboot()
                current=await self.registers(3,2205,8)
                assert current==original,("original location restore failed",current)
                restored=await self.snapshot()
                assert restored==baseline,"configuration differs after restoration"
                self.emit("persistent_configuration_restored",verified=True)


async def main(args):
    output=Path(args.out);output.mkdir(parents=True,exist_ok=True)
    probe=Probe(output,args.mac,args.expected_version);passed=False;error=None
    try:
        await probe.connect()
        baseline=await probe.exercise(args.seconds)
        if probe.errors or probe.protocol_errors:
            raise RuntimeError("read-only pressure failed; reset/write phases were not started")
        for cycle in range(args.reconnects):
            await probe.disconnect();await asyncio.sleep(.4);start=time.monotonic()
            await probe.connect();assert (await probe.custom(0xCF))[:3]==probe.expected_version
            probe.emit("reconnect_verified",cycle=cycle+1,duration_ms=round((time.monotonic()-start)*1000,2))
        if args.persistence:await probe.persistence(baseline)
        if args.logic_roundtrip:
            for cycle in range(args.logic_repeats):
                await probe.logic_roundtrip(baseline)
                probe.emit("logic_upload_cycle_done", cycle=cycle+1)
        passed=not probe.errors and not probe.protocol_errors
    except BaseException as exc:
        error=repr(exc);probe.emit("acceptance_error",error=error)
    finally:
        await probe.disconnect()
        summary={"passed":passed,"error":error,"requests_ok":len(probe.metrics),"pressure_errors":probe.errors,"framing_errors":probe.protocol_errors,"unsolicited_frames":probe.unsolicited,"unsolicited_types":probe.unsolicited_types,
                 "latency_ms":{"min":min(probe.metrics) if probe.metrics else None,"mean":statistics.mean(probe.metrics) if probe.metrics else None,"p50":percentile(probe.metrics,.5),"p95":percentile(probe.metrics,.95),"p99":percentile(probe.metrics,.99),"max":max(probe.metrics) if probe.metrics else None}}
        (output/"summary.json").write_text(json.dumps(summary,ensure_ascii=False,indent=2)+"\n")
        probe.emit("summary",**summary);probe.logfile.close()
    if not passed:raise SystemExit(1)


if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-version",default="3.2.0")
    parser.add_argument("--mac",required=True,help="Target Ethernet MAC; verified before any mutation")
    parser.add_argument("--out",required=True)
    parser.add_argument("--seconds",type=int,default=180)
    parser.add_argument("--reconnects",type=int,default=5)
    parser.add_argument("--persistence",action="store_true")
    parser.add_argument("--logic-roundtrip",action="store_true",help="Resend the current table twice, including final ATT write then immediate disconnect")
    parser.add_argument("--logic-repeats",type=int,default=1)
    asyncio.run(main(parser.parse_args()))
