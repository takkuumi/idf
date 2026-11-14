# v2.9.0 → v3.3.0 OTA 兼容性与封板记录

日期：2026-11-15（Asia/Shanghai）

## 结论

**可以 OTA 升级。** 适用范围是同一硬件型号：DEFAULT→DEFAULT、F3→F3、
F4→F4。不能把 F3 App 上传到 F4 或 DEFAULT，也不能把完整 factory 镜像、
分区表或 `ota-data-initial.bin` 当作 Web OTA 文件。

本结论来自分区、存储格式、OTA 实现和产物的逐项核对。当前没有将一台真实
v2.9.0 设备在线升级到 v3.3.0 的硬件闭环记录，因此交付验收仍应在一台保留
现场数据的 v2.9.0 设备上执行一次真实 OTA 烟测。

## 兼容性证据

| 项目 | v2.9.0 | v3.3.0 | 结论 |
|---|---|---|---|
| 分区表 SHA-256 | `03555e3c...1d66275` | `03555e3c...1d66275` | 完全一致 |
| factory/OTA 槽大小 | `0x240000` | `0x240000` | 槽容量一致 |
| F3 App 大小 | 1,829,872 B | 1,854,432 B | 均小于 OTA 槽 |
| NVS schema | `0x0207` / `IMCF` | `0x0207` / `IMCF` | 配置格式兼容 |
| holding raw | `IMC2`, version 1 | `IMC2`, version 1 | 组态/holding 格式兼容 |
| OTA rollback | 开启 | 开启 | 新固件启动后需确认 |
| Web OTA | `esp_ota_begin/write/end` | `esp_ota_begin/write/end` | 流程兼容 |

两版的 `partitions.csv` 内容和生成的 partition table 二进制相同；v3.3.0 的
App 仍在 2.25 MiB OTA 槽内。升级只写入下一个 OTA App 分区，不擦除 NVS、
holding、storage 或当前运行分区。

## 现场升级步骤

1. 先导出或记录设备当前 IP、网关、RS485 参数和组态版本。
2. 确认设备型号与 App 文件一致。
3. 登录 Web 的 OTA 页面，上传对应的 `imc-v3.3.0-<variant>-app.bin`。
4. 等待上传完成、设备自动重启和稳定确认窗口结束；期间不得断电。
5. 重新登录 Web，确认固件版本为 3.3.0，IP/网关、RS485 配置和组态仍在。
6. 通过 Modbus TCP 读取版本和关键配置，再执行一次 RS485、DO/DI、BLE 和 Web
   冒烟检查。

若设备无法访问 Web、OTA 槽不存在或启动后持续回滚，停止重复上传，改用对应
型号的完整串口迁移流程；完整刷机前应备份现场配置，且不得误用
`factory-new-device.bin` 覆盖需要保留数据的设备。

## v3.3.0 封板验收摘要

- 生产核心主机测试：28/28。
- RTU 粘帧/回显解析测试：3/3。
- 无线分帧测试：7/7。
- F3 RS485-1 FC03 `2300/100`：1000/1000 成功。
- 完整响应平均 232.310 ms，最大 252.877 ms。
- CRC 错误、数据不一致、超时：均为 0。
- UART1 `tx_errors=0`，驱动未发生异常重开。
- GPIO19 保持 `485_AD0` 地址输入，UART1 继续使用 GPIO45/46。

详细逐次日志：
`log/hardware/rs485-f3-2026-11-15/rs485-1-poll-2300x1000-v330-final/`。

## 封板剩余门槛

- 在干净、可追溯 Git 提交上重新生成 DEFAULT/F3/F4 三种发布包。
- 至少一台 v2.9.0 真实设备执行一次保留数据的 Web OTA 闭环。
- OTA 后完成一次断电重启、BLE 重连、TCP 502、RS485 和组态读取复核。
