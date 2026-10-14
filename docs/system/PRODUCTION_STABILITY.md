# 生产可靠性基线

本文档记录当前固件的可靠性措施、验证证据和发布门槛。它描述可以检查的工程事实，不把未经长时间测试的系统称为“绝对可靠”。

## 故障边界

固件将故障分成四类：可重试的外设错误、可降级的链路错误、需要保留现场的严重错误，以及由 ESP-IDF 看门狗或 panic 处理器接管的致命错误。网络、BLE、NFC、Web 和 Modbus 启动失败不会阻塞其它已经可用的服务；主循环和关键后台任务由 Task WDT、心跳和栈水位监控共同保护。

## 已实现的保护

| 区域 | 当前措施 |
|---|---|
| 启动 | 固定 DIO/40 MHz/8 MB 分区；NVS、holding 和协议数据分别校验；损坏数据回退到默认或另一槽位 |
| 配置 | SystemConfig 单 blob；holding 使用带 generation/CRC 的 A/B 槽；网络配置在写入 NVS 前统一校验 |
| 网络参数 | 启动时修复历史 mask/gateway 交换；非法静态 IP、掩码、网关和 DNS 不会写入持久化存储；DHCP 只接受 0/1 |
| Web/BLE/Modbus | 写入入口共享同一配置快照；保存请求合并落盘；“保存并重启”先完成同步持久化屏障 |
| OTA | 写入前校验大小，结束时由 `esp_ota_end` 校验镜像，启动后服务稳定 30 秒才取消 bootloader 回滚 |
| 任务 | 关键线程使用集中栈预算；任务心跳、Task WDT、FreeRTOS 栈 canary 和周期性高水位报告同时启用 |
| 通信 | TCP 请求、部分帧、发送队列和客户端数量均有上限；监听器支持事务性重绑定和自动恢复 |
| 现场分析 | reset reason、计数、panic 日志和 core dump 分区保留；主循环周期、堆和栈水位定期记录 |

## 配置安全规则

所有入口必须遵守以下顺序：解析输入、校验完整参数、更新 RCU 快照、请求持久化。网络配置不得在 socket、BLE 回调或 Modbus 回调中直接执行长时间 Flash 操作；只有重启前路径使用同步持久化屏障。NVS 不可用时保留 dirty 状态并继续重试，不能清除“待保存”标志。

启动发现非法持久化网络配置时，只替换网络字段为出厂值，保留 SN、设备名、BLE 名和 RS485 参数，并在配置快照发布后重新落盘。这样不会带着坏地址启动以太网，也不会丢失其它现场参数。

## 发布门槛

每个发布候选必须通过：

1. `cargo test --bin gateway --no-run`；测试运行器禁止直接烧入设备。
2. `cargo build --release`，并检查警告数为零。
3. `git diff --check`、Python 手工测试脚本语法检查和发布产物 SHA-256 校验。
4. 使用完整 bootloader、分区表、otadata、factory 镜像烧录；烧录过程不得中断，业务 NVS 不得全擦。
5. 启动日志包含 `device init ok`、BLE advertising、Web listening、Modbus TCP bound 和 `entering main loop`。
6. 实机回读网络 IP、掩码、网关；修改 IP 后重启，再次确认掩码和网关未交换。
7. OTA 镜像完成升级后保持运行至少 30 秒，确认新镜像被标记有效；失败时能回滚到旧镜像。

## 长时间验证

生产前应记录至少 72 小时的真实设备运行数据：reset count/reason、最低堆、最低 internal SRAM、所有任务最低栈余量、TCP/BLE 重连次数、RS485 超时和 NVS 持久化错误。任何任务出现 stall、栈余量进入临界阈值、持久化错误持续增长或无故复位，都必须回到故障分析流程，不能仅通过增加重试次数掩盖。

## 仍需持续验证的项目

- 真实负载下的 72 小时压力和断电恢复测试。
- 断网、拔插 W5500、BLE 反复连接、RS485 从站掉线和 Web 并发保存测试。
- OTA 写入中断、掉电和回滚测试。
- 不同硬件变体（默认/F3/F4）的独立烧录与参数回读。

## 2026-10-14 验证记录

- 默认配置：`cargo test --bin gateway --no-run`、`cargo build --release` 通过，编译警告为 0。
- F3：`CARGO_TARGET_DIR=target/release-f3 cargo build --release --locked --bin gateway --features f3` 通过。
- F4：`CARGO_TARGET_DIR=target/release-f4 cargo build --release --locked --bin gateway --features f4` 通过。
- 真实设备基础回归：应用启动、BLE 广播、Web、Modbus TCP 和网络参数重启后回读已通过；本轮代码变更仍需按完整烧录门槛重新刷入后执行现场长测。
