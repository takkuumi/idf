# PC TCP/RTU compatibility verification (2026-07-30)

## Scope

The PC configuration tool is not modified. This firmware change preserves its
fixed Modbus register windows over both TCP and RTU. PC OTA is explicitly out
of scope.

## Regression assertions compiled for the firmware target

| Case | Expected result |
|---|---|
| DeviceMMP FC=03, 2196, 83 words | all 83 holding registers are readable |
| DeviceMMP FC=03 TCP response | MBAP length is 169; PDU byte count is 166; no trailing byte |
| DeviceMMP FC=03 RTU response | 171-byte ADU, byte count 166, CRC over exact response |
| FC=16 logic block at 2300 | test asserts a contiguous PRegBuf round-trip and NVS dirty |
| FC=16 protocol limit | requests over the Modbus 123-register write maximum are rejected |

## Build verification

Executed in `/Users/takumi/Workspace/idf` on 2026-07-30:

```text
cargo build                              PASS
cargo test --bin gateway --no-run        PASS
cargo check (warning count = 0)          PASS
cargo build --features f3                PASS
cargo build --features f4                PASS
```

The test binary is compiled but not executed on the host: ESP-IDF unit tests
require a flashed target or an ESP-IDF test runner. `cargo build --all-features`
is intentionally rejected by `build.rs`, because
`f3` and `f4` are mutually exclusive hardware variants.

## Remaining hardware validation

No firmware was flashed in this session. The following must be exercised on
the target board before declaring end-to-end compatibility: tauri-app TCP and
RTU read/write of base properties, port settings, point control, and logic
configuration; handset BLE flows; NFC and OTA smoke tests; and a 7x24 soak
with stack high-water, heap, W5500 reconnect, and NVS-write counters recorded.
