# F3 LoRa 性能与分帧优化（2026-11-14）

状态：F3 候选已刷入实机并完成 LoRa 压测；长度分帧修复的 LoRa 验收通过。满速 TCP 新建连接洪泛出现一次 reset，限速并发与标准连接回收测试通过；UART1/UART2 有线 RTU 和完整业务回归仍待执行。

## 硬件与验收条件

- F3、ESP32-S3、板载 EBYTE E22-400T22D V2 处于透明透传模式，UART0 由板载 E22 使用。
- 设备 USB 下载口已断开；电脑侧外置 E22 对端仍连在 `/dev/cu.usbserial-14140`，设备 IP `192.168.51.226`。
- 对端发送的 FC01/02/03/04 均为只读请求；FC16 压测脚本只允许写入并恢复测试镜像区 `4096..4218`。

## 代码核查与故障来源

UART0 之前复用了有线 RTU 的 3.5 字符静默判帧。Modbus 9600bps 的静默窗口约 4.01ms；E22 会将超过子包长度的数据拆成多段透明输出，无线段间隔不代表 RTU 帧结束。于是较大的 FC16 请求或 FC03/04 响应可能在合法分片边界被截断。另有 UART 首字节预算固定限制在 250ms，无法容纳已测到的透明链路首字节时延。

制造商 E22-400T22D 页面列出空中速率范围 2.4–62.5kbps，发送子包可配置 32/64/128/240 字节。模块实际空中速率和子包设置尚未查询，本轮不向模块发送 AT 配置，也不调整对端设置。

## 更改设计

- 增加有界 Modbus RTU 长度识别：固定长度的 FC01–06、FC15/16 请求和读/写响应，FC01–04 响应 byte-count 解析，异常响应固定 5 字节。
- 仅 F3+LoRa profile 的 UART0 使用长度分帧；UART1/UART2 继续按 3.5 字符静默收帧。
- 已知长度的 RTU 帧收齐后立即返回，不再等待无线包间静默。未知/非法功能码保留有线静默规则回退；缓冲区固定 256 字节，帧间等待与整帧期限有上限。
- LoRa 主站等待首响应字节最多 2500ms；普通 RS485 仍为最多 250ms。

## 固件修改前真实设备基线

使用只读外置 E22 → 空中链路 → F3 → 空中链路 → 外置 E22 测试，检查 CRC 并与 TCP 回读对照：

- 14/14 成功，0 CRC/长度错误。
- FC04/FC03 短读约 495–565ms。
- FC03 60 寄存器约 1.18–1.29s。
- FC03 120 寄存器约 1.84–2.10s。
- 数据见 `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-baseline-before-framing/`。

这些数字是完整无线往返时间，不是 ESP32 UART 解析时间。制造商规格和较低空中速率解释了长帧时间；本轮固件侧优化目标是消除分片截断和无谓帧尾等待，不能提高模块本身的空中速率。

## 待完成验收

- [x] F3+LoRa 候选主机分帧测试 7/7 通过，目标编译和 app 读回校验通过。
- [x] 候选写入 factory，app 分区读回 SHA-256 与候选逐字节一致；分区表按项目 CSV 写入，未全擦，配置保留。
- [x] 候选混合只读 300/300，CRC、长度、值与同设备 TCP 基线均通过：平均 839.5 ms、P95 2054.3 ms、最大 2057.1 ms。
- [x] 候选 FC16 1/16/60/100/115/123 各档通过；123 寄存器连续 10/10，平均 2099.0 ms、最大 2104.4 ms；每轮写入均 TCP 验证并恢复原测试区。
- [x] 候选 FC03 120 寄存器长读 10/10，平均 2053.6 ms、最大 2058.1 ms，载荷均与 TCP 一致。
- [x] 候选 LoRa/TCP 限速并发：LoRa 45/45，TCP 313/313；标准四端口连接回收/建连 100/100、0 错误。
- [ ] 满速 TCP 新建连接洪泛并发出现 1/1523 次 TCP reset（其余 1522 成功）；限速并发未复现，保留为高连接 churn 边界。
- [ ] UART1/2 静默收帧回归、TCP/Web/BLE、配置保存复位及完整整机业务回归。

## 下载口断开后的对照固件测试（2026-11-14）

当前设备仍运行对照构建 `5ed9fc449ea8-dirty`，候选分帧固件尚未装入设备。拔除板载下载 USB、保留外置 E22 对端 `/dev/cu.usbserial-14140` 后完成：

- 首轮只读混合测试 99/100；唯一失败为 FC03 读取 120 个寄存器时 8 秒无应答。随后单独重复该长读 10 次，10/10 成功，完整帧耗时 2.054–2.065 秒。
- 第二轮混合只读测试 100/100 成功，平均完整帧 843.1 ms，P95 2060.4 ms，最大 2071.9 ms。短读约 0.50–0.58 秒；60/120 寄存器长读约 1.28/2.05 秒。
- FC16 分档写入：1、16、60、100、115 个寄存器均成功，最长分别约 2.057 秒；123 个寄存器请求 8 秒无应答。测试镜像区 4096–4218 已按脚本从 TCP 备份并恢复，恢复读回一致。
- 最大写入失败后短读恢复 3/3，平均约 533 ms。说明无线/串口仍能继续通信；123 个寄存器上限尚未验收通过。
- 候选分帧实现的主机测试 7/7 通过；没有 F3 设备下载串口和可用 Web 凭据，本轮无法把候选刷入设备进行 A/B。不能把主机测试当成 LoRa 实机通过。

原始日志：

- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-control-usb-disconnected-2026-11-14T1925/`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-control-100-usb-disconnected-2026-11-14T1926/`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-long-read-10-2026-11-14T1928.jsonl`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-control-100-usb-disconnected-rerun-2026-11-14T1932/`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-control-fc16-usb-disconnected-2026-11-14T1935/`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-post-fc16-failure-recovery-2026-11-14T1936/`

候选完整压测日志：

- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-candidate-mixed-300-post-import-2026-11-14T2005/`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-candidate-fc16-usb-disconnected-2026-11-14T1955/`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-candidate-fc16-123-repeat10-2026-11-14T2012/`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-candidate-long-read-10-corrected-2026-11-14T2002.jsonl`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/lora-candidate-concurrent-paced-2026-11-14T2022/results.json`
- `log/hardware/v3.2.0-full-acceptance-2026-11-14/tcp-connection-after-lora-stress-2026-11-14T2020/`

### 下一步

- [x] 候选已刷入并通过 LoRa 300 轮、最大 FC16、长读和限速并发压测。
- [ ] 完成 UART1/2、有线 RTU、Web/BLE、持久化和其他系统功能回归后，才可把本轮候选提升为正式生产发布物。
