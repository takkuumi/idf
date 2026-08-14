# Rust 安全与工业可靠性审计记录

日期: 2026-08-14

## 范围

- 并发快照、NVS/Flash 持久化、DeviceActor 邮箱
- Modbus TCP/RTU、BLE 输入、OTA、W5500、ADC、DI/DO/AO
- 默认、F3/F4 与最小合法 feature 组合

只读参考 ESP-IDF 5.5 W5500 实现；未修改 ESP-IDF、原 C++、手持机和 PC 配置工具源码。

## 已消除的高风险问题

1. 裸指针 RCU 读写竞争可能释放仍在读取的快照，已改用短临界区
   `Spin<Option<Arc<T>>>`；业务闭包和旧快照析构均在锁外执行。
2. 零初始化 `Instant` 属于未定义行为，已移除。
3. DeviceActor 创建失败执行 panic/reboot，已改为 Result + supervisor 重试。
4. NVS 慢写期间自旋会占满 CPU，已改为 poisoned-safe Mutex。
5. FC=0F/10 非法尾地址可能留下前半段写入，已改为整段预验证和 DO 原子发布。
6. RTU 畸形 byte count 可能越界，已在解析前完成 CRC、长度和数量一致性检查。
7. OTA 写失败和校验失败可能遗留 session，已统一 abort/清理并检查分区上限。
8. W5500 部分初始化失败泄漏 SPI/MAC/PHY/netif 资源，已增加 RAII 逆序回收。

## 编译验证

以下命令要求 0 error、0 warning：

```text
cargo clippy --bin gateway -- -D warnings
cargo clippy --no-default-features --features modbus-tcp --bin gateway -- -D warnings
cargo clippy --no-default-features --features modbus-rtu --bin gateway -- -D warnings
cargo clippy --no-default-features --features modbus-tcp,ai-ao --bin gateway -- -D warnings
cargo check --features f3
cargo check --features f4
cargo test --bin gateway --no-run
```

## 真机验收项

- 启动和 60 秒运行窗口无 panic、Stack canary、ENOMEM、pthread create failure。
- Modbus TCP 502/503/504/5002，FC03 125 words、260-byte 最大 ADU、FC0F/10 边界。
- RTU 主从站 CRC、异常帧、超时重试和连续通讯。
- 手持机 BLE 基础属性、端口、点位、逻辑配置读写与重连刷新。
- Web 保存、保存并重启、DI/DO 顺序、NFC 备份/恢复。
- OTA 上传、切换、30 秒健康确认和失败回滚。

72 小时浸泡完成前，不把短时回归描述为 7x24 已被时间验证。

## 2026-08-14 真机复核

- 定位并修复首次 AI 采样必崩：反向映射把 `4095, 0` 直接传给
  `i64::clamp`，违反 `min <= max` 契约并触发 `BREAK`。现按输出端点的
  最小值和最大值夹紧，保持参考固件 `cal_max -> 4095`、`cal_min -> 0`。
- 完整默认固件（BLE + ADC Continuous + 全部协议）连续运行至 180 秒，
  无 panic、Stack canary、pthread 创建失败或非计划重启。
- 60/120/180 秒内部 SRAM 最低值分别为 42/41/41 KiB；7 个登记任务
  `low=0`，最低栈余量 4308 B，HTTP 压力后最大栈使用率 56%。
- Modbus TCP 502/503/504/5002 均返回正确 FC03；125 寄存器响应 259 B；
  260 B 最大 ADU 请求完整接收并返回标准异常响应。
- Web 登录成功，系统、网络、端口、IO、BLE、传感器和 NFC 状态接口均
  返回 `code=0`；在线请求后 heap 和内部 SRAM 未出现持续下降。
- W5500 固定使用 ESP-IDF v5.5 官方 SPI 实现，移除不可达的自定义 DMA
  回调和 3200 B 内部 SRAM staging 缓冲。
