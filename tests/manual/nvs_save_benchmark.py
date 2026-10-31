#!/usr/bin/env python3
"""Bounded Web save benchmark; restores temporary metadata in a finally block.

Measures HTTP save-confirmation latency, including Actor scheduling. It does
not report physical Flash timings or erase counts. Only device display name
is temporarily changed; IP/UART/BLE/calibration/outputs are never changed.
Run exclusively: the device permits one Web session, so logging in displaces
an operator's session. --reboot-check deliberately restarts the device twice.
"""

import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import statistics
import time
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--samples", type=int, default=12)
    parser.add_argument("--changed-samples", type=int, default=6)
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--reboot-check", action="store_true",
                        help="Verify test metadata and restored settings across two planned reboots.")
    args = parser.parse_args()
    password = os.environ.get("GW_STABILITY_WEB_PASSWORD")
    if not args.apply or not password:
        parser.error("--apply and GW_STABILITY_WEB_PASSWORD are required")
    if not 1 <= args.samples <= 30 or not 0 <= args.changed_samples <= 16:
        parser.error("samples must be 1..30; changed-samples must be 0..16")
    if args.backup.exists():
        parser.error("backup already exists; refusing to overwrite recovery data")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base = "http://" + args.host + "/"

    def api(path, fields=None):
        data = None if fields is None else urllib.parse.urlencode(fields).encode()
        with opener.open(urllib.request.Request(base + path, data=data), timeout=10) as response:
            result = json.load(response)
        if str(result.get("code")) != "0":
            raise ValueError(f"{path} rejected: {result.get('code')}")
        return result.get("data", {})

    def login():
        api("login", {"username": "admin", "pwd": password})

    login()
    paths = ("getnetworkconfig", "getportconfig", "getsysteminfo", "getsensorconfig", "getbleconfig")
    before = {path: api(path) for path in paths}
    status = api("getsystemstatus")
    if status["recovery_mode"] != "Normal" or any(status[k] for k in ("recoverable", "degradable", "severe")):
        raise ValueError("device is not healthy before benchmark")
    args.backup.write_text(json.dumps(before, ensure_ascii=False, indent=2))
    args.backup.chmod(0o600)
    original_name = before["getsysteminfo"]["devicename"]
    original_address = before["getsysteminfo"]["addressinfo"]
    planned_resets = 0

    def reboot_and_wait():
        nonlocal planned_resets
        api("reboot", {})
        planned_resets += 1
        expected = status["reset_count"] + planned_resets
        deadline = time.monotonic() + 120
        time.sleep(3)
        while time.monotonic() < deadline:
            try:
                login()
                health = api("getsystemstatus")
            except (OSError, ValueError):
                time.sleep(1)
                continue
            if health["reset_count"] > expected:
                raise AssertionError("unexpected additional reboot")
            if health["reset_count"] == expected and health["uptime"] >= 35:
                if health["recovery_mode"] != "Normal" or any(health[k] for k in ("recoverable", "degradable", "severe")):
                    raise AssertionError("abnormal health after planned reboot")
                return
            time.sleep(1)
        raise TimeoutError("device did not return healthy after planned reboot")

    def save(name):
        started = time.monotonic()
        # The unchanged addressinfo exercises the shared SystemConfig path as
        # well as Web metadata. No network address or UART setting is changed.
        api("updatesysteminfoconfig", {"devicename": name, "addressinfo": original_address})
        elapsed = (time.monotonic() - started) * 1000
        if api("getsysteminfo")["devicename"] != name:
            raise AssertionError("save read-back mismatch")
        return elapsed

    results = {}
    try:
        for mode, count in (("unchanged", args.samples), ("changed", args.changed_samples)):
            times = []
            for index in range(count):
                name = original_name if mode == "unchanged" else f"QA-NVS-{index:02d}"
                elapsed = save(name)
                times.append(elapsed)
                print(json.dumps({"event": "save", "mode": mode, "sample": index + 1,
                                  "http_ms": round(elapsed, 3)}), flush=True)
            if times:
                ordered = sorted(times)
                results[mode] = {"samples": len(times), "median_ms": statistics.median(times),
                    "p95_ms": ordered[int((len(times)-1)*0.95)], "max_ms": max(times)}
        if args.reboot_check:
            expected_name = original_name if args.changed_samples == 0 else f"QA-NVS-{args.changed_samples-1:02d}"
            reboot_and_wait()
            if api("getsysteminfo")["devicename"] != expected_name:
                raise AssertionError("changed metadata did not survive immediate reboot")
            print(json.dumps({"event": "changed_metadata_survived_reboot"}), flush=True)
    finally:
        save(original_name)
        if args.reboot_check:
            reboot_and_wait()
        after = {path: api(path) for path in paths}
        if after != before:
            changed = [key for key in before if before[key] != after[key]]
            raise AssertionError(f"restoration mismatch in {changed}; retain {args.backup}")
        print(json.dumps({"event": "original_configuration_restored"}), flush=True)
    final_status = api("getsystemstatus")
    if final_status["reset_count"] != status["reset_count"] + planned_resets or (
            not planned_resets and final_status["uptime"] < status["uptime"]):
        raise AssertionError("device restarted during benchmark")
    if final_status["recovery_mode"] != "Normal" or any(final_status[k] for k in ("recoverable", "degradable", "severe")):
        raise AssertionError("device health changed during benchmark")
    print(json.dumps({"event": "summary", "results": results,
        "reset_count": final_status["reset_count"], "free_heap": final_status["free_heap"],
        "planned_resets": planned_resets,
        "note": "HTTP save-confirmation latency; not raw Flash program/erase time"}), flush=True)


if __name__ == "__main__":
    main()
