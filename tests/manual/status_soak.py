#!/usr/bin/env python3
"""Authenticated, direct-to-device Web health sampling for long soaks."""

from __future__ import annotations

import argparse
import http.cookiejar
import json
import os
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("--duration", type=int, default=259200)
    parser.add_argument("--interval", type=int, default=60)
    parser.add_argument("--session-refresh", type=int, default=12 * 60 * 60)
    parser.add_argument("--username", default="admin")
    parser.add_argument(
        "--password-env",
        default="GW_STABILITY_WEB_PASSWORD",
        help="environment variable containing the Web password (default: %(default)s)",
    )
    args = parser.parse_args()
    password = os.environ.get(args.password_env)
    if not password:
        parser.error(f"set environment variable {args.password_env} before running")

    base = f"http://{args.host}"
    jar = http.cookiejar.CookieJar()
    # Explicit empty proxy map is required for private LAN targets on hosts with HTTP_PROXY.
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(jar)
    )

    def login() -> None:
        form = urllib.parse.urlencode({"username": args.username, "pwd": password}).encode()
        request = urllib.request.Request(
            f"{base}/login",
            data=form,
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
        with opener.open(request, timeout=5) as response:
            envelope = json.load(response)
        if envelope.get("code") != "0" or not list(jar):
            raise ValueError(f"Web login failed: code={envelope.get('code')!r}")

    login()
    login_at = time.monotonic()
    # Device sessions expire after 24 hours. Renew at half that interval so a
    # 72-hour soak never spends a day recording the login page as failed JSON.
    session_refresh_seconds = max(1, args.session_refresh)

    start = time.monotonic()
    end = start + args.duration
    previous_uptime: int | None = None
    initial_reset_count: int | None = None
    samples = 0
    errors = 0
    transient_retries = 0
    minimum_heap: int | None = None
    print(
        f"start={datetime.now(timezone.utc).isoformat()} host={args.host} duration={args.duration}s interval={args.interval}s",
        flush=True,
    )
    while time.monotonic() < end:
        sample_start = time.monotonic()
        try:
            if time.monotonic() - login_at >= session_refresh_seconds:
                login()
                login_at = time.monotonic()
                print(f"web_session_refreshed at={datetime.now(timezone.utc).isoformat()}", flush=True)
            def read_status() -> dict:
                with opener.open(f"{base}/getsystemstatus", timeout=5) as response:
                    return json.load(response)

            try:
                envelope = read_status()
            except (OSError, TimeoutError, json.JSONDecodeError, ValueError) as first_error:
                # A single TCP reset/timeout or an expired-session HTML response
                # is retried once. The retry is explicitly logged; only a second
                # failure counts against the soak result.
                transient_retries += 1
                print(
                    f"web_retry n={transient_retries} type={type(first_error).__name__} detail={first_error}",
                    flush=True,
                )
                login()
                login_at = time.monotonic()
                envelope = read_status()
            if envelope.get("type") != "getsystemstatus":
                login()
                login_at = time.monotonic()
                with opener.open(f"{base}/getsystemstatus", timeout=5) as response:
                    envelope = json.load(response)
            if envelope.get("code") != "0":
                raise ValueError(f"status API code={envelope.get('code')!r}")
            data = envelope["data"]
            uptime = int(data["uptime"])
            reset_count = int(data["reset_count"])
            heap = int(data["free_heap"])
            faults = tuple(int(data[name]) for name in ("recoverable", "degradable", "severe"))
            minimum_heap = heap if minimum_heap is None else min(minimum_heap, heap)
            if previous_uptime is not None and uptime < previous_uptime:
                raise ValueError(f"uptime regressed {previous_uptime}->{uptime}")
            if initial_reset_count is not None and reset_count != initial_reset_count:
                raise ValueError(f"reset count changed {initial_reset_count}->{reset_count}")
            if faults != (0, 0, 0):
                raise ValueError(f"health fault counters are nonzero: {faults}")
            if data["recovery_mode"] != "Normal":
                raise ValueError(f"recovery mode is {data['recovery_mode']!r}")
            if not data["ethernet_link"]:
                raise ValueError("Ethernet link is down")
            previous_uptime = uptime
            initial_reset_count = reset_count if initial_reset_count is None else initial_reset_count
            samples += 1
            print(
                f"web_ok n={samples} uptime={uptime} heap={heap} min_heap={minimum_heap} "
                f"link={data['ethernet_link']} reset_count={reset_count} faults={faults}",
                flush=True,
            )
        except Exception as error:  # all failures are logged and cause a failing exit at end
            errors += 1
            print(f"web_error n={samples+errors} type={type(error).__name__} detail={error}", flush=True)
        elapsed = time.monotonic() - sample_start
        time.sleep(max(0.0, args.interval - elapsed))

    print(
        f"end={datetime.now(timezone.utc).isoformat()} duration={time.monotonic()-start:.1f}s "
        f"samples={samples} errors={errors} retries={transient_retries} min_heap={minimum_heap}",
        flush=True,
    )
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
