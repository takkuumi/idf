# 工业化整改静态验收记录

日期：2026-07-29 至 2026-07-30

## 已执行

```text
cargo check                                  PASS, 0 warning
cargo check --features wifi                  PASS, 0 warning
cargo check --features f4                    PASS, 0 warning
cargo test --bin gateway --no-run             PASS, test executable generated
cargo build                                  PASS, firmware ELF generated
cargo clippy -- -A warnings -D clippy::mut_from_ref
                                             PASS, MainLoopCell alias UB lint clean
git diff --check                             PASS
```

`.cargo/config.toml` 已启用 `panic-abort-tests`，标准测试编译命令与固件使用相同 abort 语义。

## 产物与配置核对

- 固件：`target/xtensa-esp32s3-espidf/debug/gateway`
- 目标：ESP32-S3，双核，240MHz，8MB Flash，2MB Quad PSRAM 80MHz。
- heap poisoning：Light；FreeRTOS stack canary/watchpoint：启用；C/C++ stack protector：Strong；coredump：Flash ELF。
- task WDT：10 秒；OTA rollback：启用；main stack：32KB。
- linker map DRAM：data 0x65C9，bss 0x7108，合计 54,993B；bss 后连续候选 172,280B。
- 真机输入日志确认旧构建出现 `mb-tcp` pthread ENOMEM、UDP IGMP 错误 125、NFC
  EEPROM/NDEF 随机写失败；本轮分别以取消 TCP pthread、修复 `in_addr` 字节序、
  恢复 100kHz I2C 并迁移 NFC 工作区处理。修复后尚未重新烧录复测。

## 未执行

- 未烧录、未连接手机、未做 NFC/OTA/TCP 真机操作。
- 未执行 72 小时压力测试。

原因：当前没有用户授权的硬件维护窗口。静态结果不得表述为 100% 真机兼容或 7x24
实证完成。
