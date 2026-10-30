#!/usr/bin/env python3
"""Bench-only password save/login/session test; restores the original secret."""
from __future__ import annotations

import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import tempfile
import time
import urllib.parse
import urllib.request


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    original = os.environ.get("GW_STABILITY_WEB_PASSWORD")
    if not args.apply or not original:
        parser.error("--apply and GW_STABILITY_WEB_PASSWORD required")
    temporary = secrets.token_urlsafe(24)
    base = f"http://{args.host}/"

    def session(password):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        with opener.open(urllib.request.Request(base + "login", data=urllib.parse.urlencode(
            {"username": "admin", "pwd": password}).encode()), timeout=5) as response:
            response.read()
        with opener.open(base + "getsystemstatus", timeout=5) as response:
            result = json.load(response)
        if str(result.get("code")) != "0":
            raise ValueError("login/status rejected")
        return opener

    def change(opener, old, new):
        request = urllib.request.Request(base + "updatepwd", data=urllib.parse.urlencode({"opwd": old, "npwd": new}).encode())
        with opener.open(request, timeout=10) as response:
            result = json.load(response)
        if str(result.get("code")) != "0":
            raise ValueError("password save rejected")

    # A private recovery file prevents lockout if power or the test process is
    # interrupted after the password changes. Never print its secret contents.
    with tempfile.NamedTemporaryFile("w", prefix="gateway-password-recovery-", suffix=".json", delete=False) as handle:
        json.dump({"host": args.host, "original": original, "temporary": temporary}, handle)
        recovery = Path(handle.name)
    print(json.dumps({"event": "private_recovery_created", "path": str(recovery)}), flush=True)
    try:
        opener = session(original)
        change(opener, original, temporary)
        with opener.open(base + "getsystemstatus", timeout=5) as response:
            if not response.url.endswith("/login"):
                raise AssertionError("previous session remained valid")
        try:
            session(original)
        except (ValueError, json.JSONDecodeError):
            pass
        else:
            raise AssertionError("old password remained valid")
        session(temporary)
        print(json.dumps({"event": "new_password_login_and_old_session_invalidation_passed"}), flush=True)
    finally:
        deadline = time.monotonic() + 120
        while True:
            try:
                opener = session(temporary)
            except Exception:
                try:
                    session(original)
                    break
                except Exception:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("login unavailable; retain private recovery file")
                    time.sleep(1)
                    continue
            change(opener, temporary, original)
            session(original)
            break
        recovery.unlink()
        print(json.dumps({"event": "original_password_restored_and_login_verified"}), flush=True)


if __name__ == "__main__":
    main()
