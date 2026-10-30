#!/usr/bin/env python3
"""Back up, explicitly mutate, immediately reboot, verify and restore test settings.

Requires --apply. Does not change the IP, UART role/baud, password, logic table
or physical outputs. It uses the currently unused raw words 40259 and 6999;
do not use on a plant where these addresses are application-owned.
"""

from __future__ import annotations

import argparse
import copy
import http.cookiejar
import json
import os
import socket
import struct
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

from production_acceptance import receive_frame, request_frame
from stability_soak import read_registers


def emit(event: str, **data) -> None:
    print(json.dumps(dict(at=datetime.now(timezone.utc).isoformat(), event=event, **data)), flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--backup", required=True)
    parser.add_argument("--restore-only", action="store_true", help="restore a retained backup after an interrupted test")
    args = parser.parse_args()
    password = os.environ.get("GW_STABILITY_WEB_PASSWORD")
    if not args.apply or not password:
        parser.error("explicit --apply and GW_STABILITY_WEB_PASSWORD are required")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base = f"http://{args.host}/"

    def login() -> None:
        with opener.open(urllib.request.Request(base + "login", data=urllib.parse.urlencode(
                {"username": "admin", "pwd": password}).encode()), timeout=5) as response:
            response.read()

    def api(path: str, fields=None) -> dict:
        request = urllib.request.Request(base + path,
            data=None if fields is None else urllib.parse.urlencode(fields).encode())
        with opener.open(request, timeout=10) as response:
            envelope = json.load(response)
        if str(envelope.get("code")) != "0":
            raise ValueError(f"{path} failed: code={envelope.get('code')}")
        return envelope.get("data", {})

    def read_word(address: int) -> int:
        with socket.create_connection((args.host, 502), timeout=3) as sock:
            sock.settimeout(3)
            return read_registers(sock, 1, 3, address, 1)[0]

    def write_word(address: int, value: int) -> None:
        with socket.create_connection((args.host, 502), timeout=3) as sock:
            sock.settimeout(3)
            pdu = struct.pack(">BHH", 6, address, value)
            sock.sendall(request_frame(700, pdu))
            if receive_frame(sock, 700) != pdu:
                raise ValueError(f"write acknowledgement mismatch at {address}")

    def snapshot() -> dict:
        state = {path: api(path) for path in ("getnetworkconfig", "getportconfig", "getsysteminfo", "getbleconfig", "getsensorconfig")}
        state["holding"] = {str(address): read_word(address) for address in (40259, 6999)}
        with socket.create_connection((args.host, 502), timeout=3) as sock:
            sock.settimeout(3)
            state["logic"] = read_registers(sock, 1, 3, 2300, 125)
        return state

    def apply(state: dict) -> None:
        api("updatenetworkconfig", {"addressinfo": state["getnetworkconfig"]["addressinfo"]})
        ports = state["getportconfig"]
        for index in range(3):
            api("updateportconfig", dict(index=index, **{field: ports[f"{field}_{index}"] for field in (
                "baud", "datalen", "checkmode", "stopbit", "masterslaveport", "slaveaddress", "retrycount", "responeinteval", "tti")}))
        api("updatesysteminfoconfig", {"devicename": state["getsysteminfo"]["devicename"]})
        api("updatebleconfig", {"blename": state["getbleconfig"]["blename"]})
        channel = max(int(key.removeprefix("ai_")) for key in state["getsensorconfig"])
        api("updatesensorconfig", dict(ch=channel, **state["getsensorconfig"][f"ai_{channel}"]))
        # These are last, with no grace sleep: the next reboot must flush both
        # holding and device_text even inside their ordinary coalescing window.
        for address, value in state["holding"].items():
            write_word(int(address), value)
        api("reboot", {})

    def wait_ready() -> None:
        deadline = time.monotonic() + 120
        last = None
        # Let the acknowledged reset actually execute before trying to log in.
        time.sleep(3)
        while time.monotonic() < deadline:
            try:
                login()
                api("getsystemstatus")
                return
            except Exception as error:
                last = error
                time.sleep(1)
        raise TimeoutError(f"device did not return after reset: {last}")

    login()
    if args.restore_only:
        original = json.loads(Path(args.backup).read_text())
        apply(original)
        wait_ready()
        if snapshot() != original:
            raise AssertionError("retained-backup restore mismatch")
        emit("retained_backup_restored_and_verified")
        return
    original = snapshot()
    backup = Path(args.backup)
    if backup.exists():
        parser.error("backup exists; refuse to overwrite the original recovery data")
    backup.write_text(json.dumps(original, ensure_ascii=False, indent=2))
    test = copy.deepcopy(original)
    test["getnetworkconfig"]["addressinfo"] = "QA-ACCEPT-1030"
    test["getsysteminfo"]["addressinfo"] = "QA-ACCEPT-1030"
    test["getportconfig"]["responeinteval_1"] = 1100 if original["getportconfig"]["responeinteval_1"] != 1100 else 1200
    test["getsysteminfo"]["devicename"] = "Gateway acceptance 1030"
    test["getbleconfig"]["blename"] = "QA1030"
    test["getnetworkconfig"]["bloothaddress"] = "QA1030"
    channel = max(int(key.removeprefix("ai_")) for key in test["getsensorconfig"])
    test["getsensorconfig"][f"ai_{channel}"] = {"min": 605, "max": 3016}
    test["holding"] = {"40259": original["holding"]["40259"] ^ 0xA55A,
                       "6999": original["holding"]["6999"] ^ 0x5AA5}
    emit("backup", path=str(backup), fields=list(original), note="test writes are authorized by --apply")
    try:
        apply(test)
        wait_ready()
        actual = snapshot()
        if actual != test:
            raise AssertionError("test settings failed reboot read-back: " + json.dumps({
                key: dict(expected=test[key], actual=actual[key]) for key in test if test[key] != actual[key]}))
        emit("changed_settings_survived_immediate_reset")
    finally:
        # Always restore, including on a failed changed-value assertion.
        wait_ready()
        apply(original)
        wait_ready()
        restored = snapshot()
        if restored != original:
            raise AssertionError("restore mismatch; use the retained recovery backup")
        emit("original_settings_restored_and_verified_after_reset")


if __name__ == "__main__":
    main()
