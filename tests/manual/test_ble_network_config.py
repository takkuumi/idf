#!/usr/bin/env python3
"""Read the handheld BLE network response; optionally change only the IP."""

from __future__ import annotations

import argparse
import asyncio
import secrets
import struct

from bleak import BleakClient, BleakScanner

SERVICE_UUID = "4fafc201-1fb5-459e-8fcc-c5c9c331914b"
CHARACTERISTIC_UUID = "beb5483e-36e1-4688-b7f5-ea07361b26a8"
IP_BASE = 0x08C7
REGISTER_COUNT = 12


def crc16_modbus(data: bytes) -> int:
    crc = 0xFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0xA001 if crc & 1 else 0)
    return crc


def make_frame(transaction_id: int, pdu: bytes, unit: int = 1) -> bytes:
    payload = bytes((unit,)) + pdu
    prefix = struct.pack(">HHH", transaction_id, 0, len(payload)) + payload
    return prefix + struct.pack("<H", crc16_modbus(prefix))


def parse_frames(buffer: bytearray) -> list[bytes]:
    frames = []
    while len(buffer) >= 6:
        payload_len = int.from_bytes(buffer[4:6], "big")
        if not 2 <= payload_len <= 504:
            buffer.clear()
            raise ValueError(f"invalid BLE payload length: {payload_len}")
        total_len = 6 + payload_len + 2
        if len(buffer) < total_len:
            break
        frame = bytes(buffer[:total_len])
        del buffer[:total_len]
        actual_crc = int.from_bytes(frame[-2:], "little")
        if crc16_modbus(frame[:-2]) != actual_crc:
            raise ValueError("BLE response CRC mismatch")
        frames.append(frame)
    return frames


def decode_network_response(frame: bytes) -> tuple[str, str, str]:
    payload_len = int.from_bytes(frame[4:6], "big")
    pdu = frame[6 : 6 + payload_len]
    if len(pdu) != 27 or pdu[1] != 0x03 or pdu[2] != 24:
        raise ValueError(f"unexpected READ_IP response PDU: {pdu.hex()}")
    octets = []
    for offset in range(3, 27, 2):
        high, low = pdu[offset : offset + 2]
        if high != 0:
            raise ValueError(f"invalid encoded IPv4 octet: {high:#04x}{low:#04x}")
        octets.append(low)
    fields = [octets[0:4], octets[4:8], octets[8:12]]
    return tuple(".".join(map(str, field)) for field in fields)  # IP, mask, gateway


def parse_ip(value: str) -> list[int]:
    try:
        octets = [int(part, 10) for part in value.split(".")]
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected dotted IPv4, for example 192.168.1.20") from error
    if len(octets) != 4 or any(octet < 0 or octet > 255 for octet in octets):
        raise argparse.ArgumentTypeError("IPv4 must contain four octets in the range 0..255")
    return octets


async def find_gateway(device: str | None):
    if device:
        return device
    found = await BleakScanner.find_device_by_filter(
        lambda candidate, advertisement: SERVICE_UUID in (advertisement.service_uuids or []),
        timeout=10,
    )
    if found is None:
        raise TimeoutError(f"no BLE device advertising service {SERVICE_UUID}")
    return found


async def exchange(client: BleakClient, request: bytes, expected_tx: int) -> bytes:
    loop = asyncio.get_running_loop()
    response: asyncio.Future[bytes] = loop.create_future()
    buffer = bytearray()

    def on_notify(_sender: int, data: bytearray) -> None:
        buffer.extend(data)
        try:
            frames = parse_frames(buffer)
        except Exception as error:
            if not response.done():
                response.set_exception(error)
            return
        for frame in frames:
            if int.from_bytes(frame[0:2], "big") == expected_tx and not response.done():
                response.set_result(frame)

    await client.start_notify(CHARACTERISTIC_UUID, on_notify)
    try:
        await client.write_gatt_char(CHARACTERISTIC_UUID, request, response=True)
        return await asyncio.wait_for(response, timeout=8)
    finally:
        await client.stop_notify(CHARACTERISTIC_UUID)


async def read_config(device: str | None) -> tuple[str, str, str]:
    target = await find_gateway(device)
    async with BleakClient(target) as client:
        tx = secrets.randbelow(0xFFFF) + 1
        pdu = struct.pack(">BHH", 0x03, IP_BASE, REGISTER_COUNT)
        frame = await exchange(client, make_frame(tx, pdu), tx)
        return decode_network_response(frame)


async def write_ip_and_verify(device: str | None, new_ip: list[int]) -> None:
    target = await find_gateway(device)
    async with BleakClient(target) as client:
        read_tx = secrets.randbelow(0xFFFF) + 1
        read_pdu = struct.pack(">BHH", 0x03, IP_BASE, REGISTER_COUNT)
        before = decode_network_response(
            await exchange(client, make_frame(read_tx, read_pdu), read_tx)
        )
        values = new_ip + parse_ip(before[1]) + parse_ip(before[2])
        write_tx = (read_tx + 1) & 0xFFFF
        data = b"".join(struct.pack(">H", octet) for octet in values)
        write_pdu = struct.pack(">BHHB", 0x10, IP_BASE, REGISTER_COUNT, len(data)) + data
        ack = await exchange(client, make_frame(write_tx, write_pdu), write_tx)
        pdu = ack[6 : 6 + int.from_bytes(ack[4:6], "big")]
        if len(pdu) != 6 or pdu[1] != 0x10:
            raise ValueError(f"unexpected WRITE_IP acknowledgement: {pdu.hex()}")
        print("IP write acknowledged; waiting for the gateway's planned reset")

    await asyncio.sleep(3)
    after = await read_config(device)
    expected = (".".join(map(str, new_ip)), before[1], before[2])
    if after != expected:
        raise AssertionError(f"network config mismatch: expected {expected}, got {after}")
    print(f"Verified after reconnect: IP={after[0]} Mask={after[1]} Gateway={after[2]}")


async def async_main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", help="BLE address/identifier; otherwise discover by service UUID")
    parser.add_argument(
        "--write-ip",
        type=parse_ip,
        help="change only the IP (the gateway resets, then the script reconnects and verifies)",
    )
    args = parser.parse_args()
    if args.write_ip is not None:
        print("Current mask and gateway will be preserved; BLE connectivity is used across the reset.")
        confirmation = input(f"Type the new IP {'.'.join(map(str, args.write_ip))} to continue: ")
        if confirmation != ".".join(map(str, args.write_ip)):
            raise SystemExit("IP confirmation did not match; no write sent")
        await write_ip_and_verify(args.device, args.write_ip)
    else:
        ip, mask, gateway = await read_config(args.device)
        print(f"IP={ip} Mask={mask} Gateway={gateway}")


if __name__ == "__main__":
    asyncio.run(async_main())
