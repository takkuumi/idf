# Modbus standard PDU and stack-path verification (2026-07-30)

## Compatibility boundary

The protocol limits are the Modbus Application Protocol limits, not the legacy
PC tool's 60-word chunk size:

| Item | Limit |
|---|---:|
| PDU | 253 bytes |
| Modbus TCP MBAP length | 254 bytes |
| Modbus TCP ADU | 260 bytes |
| FC=01/02/0F bits | 2,000 |
| FC=03/04 registers | 125 |
| FC=10 registers | 123 |

## Memory-path change

- FC=01/02 reads now pack bits into the response buffer while iterating the
  backend; they no longer construct `heapless::Vec<bool, 2000>`.
- FC=0F writes pass the request packed bitmap to the backend; they no longer
  expand it into a 2,000-element bool vector.
- A TCP client keeps exactly one 260-byte receive ADU and one 260-byte transmit
  ADU. Eight client states therefore remove 2,080 bytes of PSRAM state.

## Static verification

```text
cargo build                         PASS
cargo test --bin gateway --no-run   PASS (target test binary compiled)
cargo check                         PASS (0 warnings)
cargo build --release               PASS
espflash save-image                 PASS (raw app 1,600,240 bytes)
```

The FC=01 2,000-bit response regression assertion compiles against the exact
252-byte response PDU. No target-board execution or 72-hour soak was performed
in this session; those tests remain required before making a 7x24 claim.
