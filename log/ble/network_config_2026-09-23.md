# 网络配置修复验证记录 (2026-09-23)

## 固件构建与烧录

- `cargo build --bin gateway`：通过。
- `cargo test --bin gateway --no-run`：通过，单元测试目标编译成功。
- `espflash flash` 使用 `docs/FLASH.md` 中完整 factory 命令，包含
  `--no-skip`、bootloader、`partitions.csv`、`--erase-parts otadata` 和
  DIO/40MHz/8MB 参数；设备识别为 ESP32-S3 v0.2，factory 应用 1,739,824 B。
- NVS 未擦除。启动日志确认 factory 槽、生产分区表、App v2.2.6、DIO/40MHz，
  网络启动为 `192.168.51.226 / 255.255.255.0 / 192.168.51.1`，BLE 广播启动，
  W5500、Web 和 Modbus TCP 502/503/504/5002 均进入运行态。
- 未观察到 panic、canary 或启动复位循环。
- 最终含协议测试与无警告修正的镜像再次完整刷入成功（第二次因短暂 USB 串口
  断开未开始；按指南 115200 baud 重试后完成）。串口再次确认 App v2.2.6、
  factory 启动、W5500/BLE/Modbus 服务均正常。

## 设备网络寄存器实测

使用真实设备 Modbus TCP 502 只读 FC03，读取 holding 2247–2258：

```text
[192, 168, 51, 226, 255, 255, 255, 0, 192, 168, 51, 1]
IP      = 192.168.51.226
Mask    = 255.255.255.0
Gateway = 192.168.51.1
```

结果符合 IP、掩码、网关的寄存器布局。未通过 BLE 手持机实际写入网络配置：
本次可用设备为串口连接的网关，没有可操作的 Android 手持机客户端。因此
FC16 写后复位及手持机页面显示仍需 Android 客户端闭环确认；固件端测试计划见
`HANDHELD_CONFIG_TEST_PLAN.md`。

## 2026-09-24 现场问题复核

用户复现“只改 IP 后掩码/网关互换”后，串口启动日志曾显示：

```text
network config: ip=192.168.51.220 mask=192.168.51.1 gw=255.255.255.0
```

根因是 Android `DeviceFragment` 调用 `sendWriteIPCMD` 时把
`maskBuffer` 传给名为 `gateway` 的形参、把 `gatewayBuffer` 传给名为 `mask`
的形参；实际发送字节仍是 IP、掩码、网关。固件此前按 Java 形参名再次交换，
造成现场看到的结果。

修复后固件不再交换 BLE FC16 字段，并增加旧错误快照迁移：当掩码不是连续合法
掩码、而网关是合法连续掩码时启动自动交换并同步写回 NVS。最终串口和 Modbus
TCP 读回均为：IP `192.168.51.220`、掩码 `255.255.255.0`、网关
`192.168.51.1`。

## 全栈审计补充 (2026-09-24)

- 修复 `AT+CFG485` 完整波特率解析，避免 `9600` 被错误保存为 `960000`。
- BLE 逻辑配置读取改为固定容量缓冲，避免响应超过容量时静默截断。
- RS485 未实现的透明转发模式不再假装工作，mode=3 安全降级为 Slave。
- 默认/F3/F4 特性检查、默认构建和测试目标编译通过；无新增 warning。

## 生产 Release 刷机复核 (2026-09-24)

- `cargo build --release --bin gateway` 完成，应用镜像 1,668,848 B，占 factory/OTA
  应用槽 70.74%。
- 按完整生产命令刷入 factory，保留 NVS，DIO/40MHz/8MB；启动确认分区表、PSRAM、
  W5500、BLE GATT、Web、NFC、RS485 和四个 Modbus TCP listener 均正常。
- 运行约 60 秒期间无 panic、stack canary、任务创建失败或复位循环；主循环峰值
  约 6.4 ms，内部 heap 最低约 62 KiB。
