# ST25DV RF Mailbox 编解码主机验证

日期：2026-11-08

> 历史记录：此页无 ACK 结论仅适用于 11 月 8 日未焊接 NFC 的旧板。11 月 11 日当前板已读到 `IC_REF=0x26`；ST 官方确认其为合法 K 代身份，本轮驱动已增加严格容量校验及 K/KC 寄存器分支，后续结论见 2026-11-11 硬件记录。

## 范围

- `tests/host/run_nfc_mailbox.py` 从 `src/nfc/mod.rs` 提取实际 `MailboxAssembler`、`mailbox_fragment` 与 mailbox golden tests，在本机 Rust host test runner 执行。
- Golden vector 与 Flutter `test/nfc_mailbox_codec_test.dart` 中 243 字节分片向量一致，固定检查头字段、分片长度和 CRC 字节；不是只用同一个编码器生成测试解码器输入。
- 当前脚本执行结果：8 passed，0 failed；覆盖直接帧、分片 golden vector、CRC 损坏、乱序恢复、512 字节最大帧、非法长度、邮箱状态转换，以及 ST25DV16KC 官方身份签名（IC_REF/MEM_SIZE/BLK_SIZE）。
- 固件 `cargo check --features f3`、`cargo check --features f4`、`cargo fmt --all -- --check`、`git diff --check` 通过；本轮另以 `cargo build --bin imc --features f3` 完整构建并刷入真实 F3 设备。
- 当前诊断版本在每组候选线无地址 ACK 时额外记录释放后的 SDA/SCL 电平，下一次真实启动可据此区分线路低电平故障与目标器件未响应。
- 真实板级串口日志保存在 `log/hardware/nfc/st25dv16kc-f3-probe-2026-11-08.log` 和 `log/hardware/nfc/st25dv16kc-f3-e0-probe-2026-11-08.log`。修正驱动启动正常；探测 E0=1/0 的四个目标地址后，ST25DV 在 GPIO38/37、GPIO35/36 两组候选总线上仍无应答；RF Mailbox 仍未验收。
- App `flutter analyze` 无问题；全套 Flutter 测试通过；macOS Debug 和当前 Xcode 26.5 下 iOS Simulator Debug 均已构建。

## 验收限制

Host vector 验证协议编码与解析逻辑，不验证 ESP32-S3 软件 I²C 电气时序、ST25DV 射频、天线耦合、手机 NFC 控制器差异或设备掉场恢复。固件当前已按 ST25DV16KC-IE 数据手册身份值识别，并在主机写邮箱后增加 ACK polling；这些仍需实体芯片验证。修正身份签名、加入 E0 两种地址并刷入 F3 后，串口显示两组候选引脚在系统地址 `0x57/0x56` 均为 `NoSystemAddressAck`；E0 位已排除，仍需确认供电、PCB 实际引脚、可配置的设备码或器件状态。当前仍无真实 Mailbox RF 往返证据。新增串口日志为 `log/hardware/nfc/st25dv16kc-f3-e0-probe-2026-11-08.log`。需要实体板确认供电、引脚和外部上拉，再用实体手机完成 RF Mailbox 往返、掉场恢复及网络参数写入后的应答/重启测试。

## 2026-11-08 真实 F3 诊断刷写

- 使用 `/dev/cu.usbserial-1430` 按完整分区参数、`--no-skip` 刷入诊断版本；未擦除 NVS、holding 或 storage，仅按烧录命令清理 otadata。
- 芯片识别：ESP32-S3 rev 0.2，MAC `fc:01:2c:05:64:b8`；factory 应用 `1,808,400/2,359,296` 字节（76.65%）。
- 启动版本：2.7.0，build id `ffa1b692d67b-dirty`；启动后未见 panic、WDT、stack canary 或服务启动失败。
- 新诊断结果：
  - GPIO38/37：`NoSystemAddressAck`，释放后 `SDA=1 SCL=1`；
  - GPIO35/36：`NoSystemAddressAck`，释放后 `SDA=1 SCL=1`。
- 设备以约 5 秒退避持续重复上述探测，运行至约 16.8 秒仍无异常。两组总线均为空闲高电平，当前证据不支持“总线被拉低/明显无上拉”；仍需核对 ST25DV 供电、实际 PCB 连线、器件 I²C_DEVICE_CODE 和器件状态。
- 用户随后确认当前 F3 板上的 ST25DV 芯片未焊接；因此上述无 ACK 结果已定性为预期的可选硬件缺失。该板不执行真实 NFC 射频验收，固件的无 NFC 降级启动行为通过。
