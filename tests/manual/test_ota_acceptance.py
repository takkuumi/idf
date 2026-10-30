#!/usr/bin/env python3
"""Explicit bench OTA upload, planned restart and configuration-retention check."""
from __future__ import annotations
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import time
import urllib.parse
import urllib.request


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("image")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    password = os.environ.get("GW_STABILITY_WEB_PASSWORD")
    if not args.apply or not password:
        parser.error("--apply and GW_STABILITY_WEB_PASSWORD required")
    image = Path(args.image).read_bytes()
    if not image or image[0] != 0xE9 or len(image) > 0x240000:
        parser.error("not a bounded ESP application image")
    base = f"http://{args.host}/"
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def login():
        with opener.open(urllib.request.Request(base + "login", data=urllib.parse.urlencode(
                {"username": "admin", "pwd": password}).encode()), timeout=5) as response:
            response.read()

    def api(path):
        with opener.open(base + path, timeout=5) as response:
            result = json.load(response)
        if str(result.get("code")) != "0":
            raise ValueError(f"invalid response from {path}")
        return result["data"]

    login()
    before = {path: api(path) for path in ("getnetworkconfig", "getportconfig", "getsysteminfo", "getsensorconfig", "getbleconfig")}
    reset_count = api("getsystemstatus")["reset_count"]
    print(json.dumps({"event": "upload_begin", "size": len(image), "sha256": hashlib.sha256(image).hexdigest(), "reset_count": reset_count}), flush=True)
    request = urllib.request.Request(base + "updateota", data=image,
        headers={"Content-Type": "application/octet-stream"})
    with opener.open(request, timeout=120) as response:
        result = json.load(response)
    if str(result.get("code")) != "0":
        raise ValueError("OTA upload rejected")
    print(json.dumps({"event": "upload_accepted"}), flush=True)
    time.sleep(3)
    deadline = time.monotonic() + 120
    while True:
        try:
            login()
            state = api("getsystemstatus")
            if state["reset_count"] <= reset_count:
                raise ValueError("planned restart has not occurred")
            if state["reset_count"] != reset_count + 1 or state["reset_reason"] != 3:
                raise ValueError("unexpected restart count/reason")
            if state["uptime"] < 45:
                time.sleep(2)
                continue
            if {path: api(path) for path in before} != before:
                raise AssertionError("configuration changed across OTA")
            if state["recovery_mode"] != "Normal" or any(state[k] for k in ("recoverable", "degradable", "severe")):
                raise AssertionError("abnormal health after OTA")
            break
        except (OSError, ValueError, json.JSONDecodeError) as error:
            if time.monotonic() >= deadline:
                raise TimeoutError("OTA did not return healthy") from error
            time.sleep(1)
    print(json.dumps({"event": "ota_restarted_configuration_preserved", "status": state,
                      "note": "confirm running partition and app-valid marker in UART log"}), flush=True)


if __name__ == "__main__":
    main()
