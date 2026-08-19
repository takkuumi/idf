# MCA Full Address Contract and Regression

Reference implementation: `/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE`.
Target firmware: IDF gateway on `192.168.51.221:5002`.

## Address contract

| Function | Address window | Compatibility behavior |
| --- | ---: | --- |
| FC01/FC02 | `0..2047` | `0..DI-1` input mirror, `48..511` reserved zero, `512..DO_END` physical Q, remaining legacy D bits persistent |
| FC05/FC0F | `512..2047` | Physical Q and persistent legacy D bits; `0x0402` restart side effect is retained for FC05 |
| FC03 | `0..299` | Legacy `PCtrlBuf`; 10..13 expose the original 8-bit Q/I groups and configured feedback words override when matched |
| FC04 | `0..2175` | Legacy `PMntrBuf` plus the complete read-only window; unimplemented legacy words return zero |
| FC03/FC04 | `4000..4223` | RS485 master result mirror, cleared on failed poll |
| FC03/FC16 | `2176..4223` | Persistent PRegBuf/configuration, including the complete `2300+` variable-length table |
| FC03/FC06/FC16 | `4222` | Legacy protection word remains ordinary readable/writable PReg behavior |
| FC03/FC16 | `5000..6999` | Persistent device text words |

## Runtime behavior restored

The IDF control executor now parses the same 2300+ IO record layout as the
reference firmware. A control write:

1. clears every configured Q point for that logic;
2. waits the configured delay without blocking the Modbus task;
3. applies the control bitfield by physical Q/I point number, matching the
   original MCA bit positions even when the configured point list is sparse;
4. clears pulse outputs after the configured pulse interval;
5. derives the feedback word from I points first, then Q points when no I
   feedback is configured.

The corresponding FC03 and FC04 reads expose the dynamic feedback value. Pulse
outputs are excluded from steady-state NVS persistence.

## Persistence regression

The following values were written, verified, rebooted, and restored to zero:

* `PCtrlBuf[299] = 0x1234`
* legacy coil `2047 = ON`
* legacy coils `560..563 = 1011`
* PReg `2274..2277 = 4142 4344 4546 4748`

Raw holding snapshot generation advanced across A/B slots and the reboot read
back matched exactly. After cleanup, all four values read back as zero.

## RS485 result semantics

The external slave on the test bench did not respond. This is reported as an
empty-response timeout and the configured result range is cleared to zero,
matching the old firmware's `Modbus_Clear_Result` behavior. It must not be
interpreted as a failure of the 2300+ mirror itself.

## 2026-08-19 final hardware acceptance

On `192.168.51.221:5002` with the updated IDF image:

* FC06 and FC16 control writes produced identical physical Q bitmaps;
* Q0..Q15 was restored to `0110011000000000` (`0x66`) and control words 1/2
  were restored to zero;
* raw holding A/B state, group configuration and DO state survived reboot;
* the full address-window scan passed with standard 125-word request chunks;
* 60 seconds of runtime showed no panic, stack canary, reboot or stalled task;
  RTU warnings were limited to expected empty-response timeouts from the
  unconnected downstream slaves.
