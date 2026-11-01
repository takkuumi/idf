# BLE 广播静默停止自愈验收

日期：2026-11-01（Asia/Shanghai）  
提交：`767d12eeade3`  
硬件：F3 / PCA9555，ESP32-S3 8MB

## 修复行为

- 不再把一次成功的 GAP start callback 当作广播永久活跃证明。
- 异步配置、启动、停止操作分别登记状态；单个操作最多等待 5 秒，回调丢失后取消卡住状态并重建广播数据。
- 广播已确认活跃但设备处于空闲连接状态时，每 60 秒续期一次。续期利用 IDF 的 legacy start API，内部有短暂 stop/start 间隔；连接建立期间禁止维护调用。
- BLE 名称修改与 GAP 操作串行；普通断开只恢复广播，不执行名称重建。SDK 错误使用 5 秒退避。
- 将 `u32` uptime 回卷、失败退避、连接取消和不同操作回调匹配纳入状态机。

## 验证

- `tests/host/run_ble_advertising.py`：20/20 通过。该 harness 从生产源码提取广播维护函数和 GAP callback，模拟控制器状态、成功/失败/丢失回调、连接和改名。
- 全量 host-check：核心 27、TCP 10、IO 单芯片 8 与三芯片 8、架构 28、事件总线 5、RCU Loom、NVS 10、storage 配置 13 全部通过。
- `cargo +stable fmt --all -- --check`：通过。
- F3 `cargo clippy --release --locked --bin imc --features f3 -- -D warnings`：通过，零 warning。
- F3 release app：1,772,448 bytes，OTA 槽占用 75.1%；发布包 SHA 校验通过。
- 460800 baud 首次写 app 到约 54% 时串口传输中断；设备没有启动该不完整 app。切换到 115200 baud 完整重刷，bootloader `0x0`、partition table `0x8000`、otadata `0x10000`、app `0x20000` 四段全部 digest matched。
- 未执行全擦；现场 IP `192.168.51.226`、掩码 `255.255.255.0`、网关 `192.168.51.1` 和原持久化区保留。
- 刷后被动 UART 看到版本 `2.5.0`、build id `767d12eeade3`、设备名 `Mesh`、GATT 服务启动及 `BLE advertising active`。20 秒启动采集未见 panic、WDT、stack canary 或广播失败。

## 验证边界

host harness 验证了固件调用顺序与状态机在静默停止模型下能恢复，但无法强制真实 ESP32 BLE controller 无回调地停止。实机扫描端到端与长时间现场空口稳定性仍由后续使用手持机观察；本记录不把启动成功日志等同于已完成空口故障注入。
