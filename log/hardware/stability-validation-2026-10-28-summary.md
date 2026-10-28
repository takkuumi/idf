# Firmware stability validation (2026-10-28)

## Test image and device

- Device MAC: `80:b5:4e:5b:24:e4`; configured address: `192.168.51.225`.
- Hardware: ESP32-S3, 8 MiB flash, W5500 Ethernet.
- Image: default release profile, firmware `2.2.7`, build ID `fd89b4ca1ec9-dirty`.
- App image: 1,681,584 bytes; SHA-256 `b5606e1dc98d0c790ecfd66401f73e78fbe996e44474dde5082647fecd4da831`.
- Flashed bootloader, partition table, OTA data and factory app with the repository's full-flash script. All four segments passed readback verification. NVS and `holding` were preserved.

## Results

- **Modbus TCP soak:** 30 minutes, 7,180 complete polling rounds across ports 502, 503, 504 and 5002; 0 transport or protocol errors. Each port held one persistent connection. AI, status, sensor-calibration holding range and firmware-version reads remained valid.
- **Connection capacity:** four additional clients were added to the four soak clients. All 120 reads across 30 rounds succeeded at the configured 8-client limit. A ninth client was reset by the server as expected. The four extra clients were then closed and the soak continued without errors.
- **Protocol fault recovery:** byte-fragmented requests, 20 pipelined requests, malformed MBAP headers, unsupported/malformed PDUs, four slow partial clients and malformed/slow HTTP requests were injected. Invalid Modbus clients were closed or received standard exceptions, slow clients expired, and every listener plus the Web status/login path recovered immediately.
- **Web status:** authenticated direct-to-device polling returned 60/60 successful responses over five minutes. Link remained up, recovery mode remained `Normal`, fault counters remained zero, and persisted reset count remained 66 during the observation.
- **Memory and loop:** sampled free heap stayed near 2.09 MiB; minimum observed free heap was about 2.02 MiB and internal-heap watermark about 48 KiB. One 60-second window around the holding-save test reported 16 iterations over the 5 ms loop budget and a 35 ms maximum; the following two windows reported zero misses and maxima below 4.7 ms. This transient scheduling delay is recorded for follow-up; it did not trigger a reset or recovery fault.
- **Holding A/B:** ten FC06 writes of the existing value at PLC alias register 40051 were each read back. Then the test changed that otherwise-unused user register from `0x0000` to marker `0xA55A`; after the quiet-save interval, raw flash showed slot 0 generation 15 containing the marker and a valid CRC, while slot 1 generation 14 retained the old value and a valid CRC. Reset loaded slot 0 generation 15 and Modbus read back `0xA55A`. The test restored `0x0000`, waited for persistence, and raw flash showed slot 1 generation 16 containing `0x0000` with valid CRC; final reset loaded slot 1 generation 16 and register 40051 remained `0x0000`. All four Modbus ports returned firmware version `0x00E3`, date `0x0615` after startup.
- **Boot:** the device completed startup after the reset, restored the saved configuration, brought up BLE advertising, W5500/IP, Web, NFC detection, and all four Modbus TCP listeners. No panic, watchdog reset, or recovery fault was observed.

## Build and static checks

- `cargo check --locked`: default, F3 and F4 variants passed.
- `cargo check --locked --no-default-features --features modbus-tcp`: passed.
- `cargo build --release --locked`: passed.
- `cargo clippy --locked -- -D warnings`, `cargo fmt --check`, `git diff --check`, and Python bytecode compilation for the soak script passed.
- Default, F3 and F4 Rust unit-test targets and the Modbus integration-test target compiled with `cargo test --no-run`. The configured Xtensa runner intentionally refuses to execute libtest images on the device, so these tests were compile-checked but not executed.

## Test-harness notes and limits

- An initial probe opened more than 10 new TCP connections per second and exercised the firmware's connection-rate limiter. The soak was changed to reuse one connection per port; its 7,180-round result has no errors.
- One Web probe traversed the workstation's configured HTTP proxy and received a 503. Direct-to-device polling bypassed that proxy and succeeded 60/60 times.
- The first offline CRC calculation included 16 bytes of slot padding by mistake. Recalculation using the firmware's 6,504-byte total record boundary matched both stored CRCs; the device independently loaded generation 14.
- No power cut was injected during an active flash erase/write. This validation covers ordinary reset recovery, not every possible brownout timing. No real AI current source, RS485 slave, or handheld BLE client was attached during this run.
- This 30-minute run is a targeted hardware regression check, not a substitute for the documented 72-hour soak or for the missing peripheral end-to-end tests.

Full serial, network and test output: [`stability-validation-2026-10-28.log`](stability-validation-2026-10-28.log). The reusable read-only probe is [`tests/manual/stability_soak.py`](../../tests/manual/stability_soak.py).

An additional 72-hour read-only soak is running against the same device and image from `2026-10-28T11:28:58Z`. A first detached launch ended during shell teardown; its partial lines remain at the beginning of the live log. The active run is attached to the current test session and is not counted as a pass here. Live log: `log/hardware/stability-soak-72h-2026-10-28.log`.

A parallel Web health monitor started at `2026-10-28T12:51:32Z` and checks uptime, reset count, heap, link, recovery mode and fault counters every 60 seconds. It is also still running and is not counted as a completed pass. Live log: `log/hardware/status-soak-72h-2026-10-28.log`.

The first Web monitor log contains eight expected fault-injection observations after malformed/slow HTTP requests, plus the original monitor's pre-fix JSON handling. The monitor was corrected to renew sessions and retry login-page responses; a 3-second renewal test completed 8/8 samples without error. A clean 72-hour Web monitor restarted at `2026-10-28T13:19:27Z` with `--session-refresh 43200`; its log is `log/hardware/status-soak-72h-clean-2026-10-28.log`.
