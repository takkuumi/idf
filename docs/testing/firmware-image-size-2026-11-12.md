# F3 LoRa 固件镜像体积评估（2026-11-12）

## OTA 分区基线

- OTA/factory app 分区容量：`0x240000` = 2,359,296 bytes。
- 当前 `Cargo.toml [profile.release]`：`opt-level=3`、fat LTO、单 codegen unit、strip、panic abort。
- 当前源码完整 F3 LoRa release 重新构建的 ESP app image：1,826,624 bytes，77.42%，剩余 532,672 bytes。
- 注意：旧 `target/release-f3-lora` ELF 是较早的源码构建，不可作为当前体积基线；此前 `release/v2.7.0/f3` 包也为 2026-11-07 旧构建，manifest 漏记 `lora` feature。

## 同一源码、相同 F3 LoRa 特性下的优化级比较

通过独立 `CARGO_TARGET_DIR` 分别构建，保持 `f3,lora`、fat LTO、单 codegen unit、strip、panic abort 和相同 sdkconfig defaults；使用 `espflash save-image` 生成 app image，并由 `esptool image_info` 验证 ESP32-S3 image hash：

| Rust 优化级 | App image | OTA 槽占用 | OTA 槽剩余 | 相对 `opt-level=3` |
|---|---:|---:|---:|---:|
| `3`（当前 release） | 1,826,624 B | 77.42% | 532,672 B | 基线 |
| `s` | 1,544,192 B | 65.45% | 815,104 B | -282,432 B（-15.46%） |
| `z` | 1,482,224 B | 62.82% | 877,072 B | -344,400 B（-18.85%） |

`s` 比 `z` 大 61,968 bytes，但更偏向尺寸与速度平衡。当前构建 profile 先保持 `opt-level=3`：本次只完成构建/镜像校验，没有在真实板上对 `s/z` 执行 AI/DO/I²C、NFC、BLE、RS485、Web、TCP、OTA 与长稳性能对照。不可仅凭镜像更小就把候选级别改成生产默认，也不能将候选镜像刷到生产设备后宣称已验收。

## 发布链修正

`scripts/release.sh` 对 F3 的实际构建参数是 `f3,lora`，manifest 现在从构建目标特性列表生成并会记为 `default,f3,lora`。过去的 manifest 漏掉 `lora`，已构建镜像仍然有效，但其元数据不准确；旧 manifest 不追溯改写，下一次重新打包时会产生正确字段。

## 结论

当前 OTA 产物有约 0.51 MiB 的槽内余量。通过 Rust `s` 或 `z` 优化可额外增加约 0.28 MiB / 0.33 MiB 余量；是否成为生产默认，需要在真实设备上对照运行性能、时序、内存/栈和完整功能回归后确定。当前不改变 release 优化级。
