# NFC 实板接入与固件验证（2026-11-11）

## 设备与构建

- 当前设备：ESP32-S3 rev 0.2，MAC `fc:01:2c:05:64:88`，串口 `/dev/cu.usbserial-1430`，F3。
- 固件版本保持 2.7.0，工作树构建；未进行正式发布或 Git 提交。
- 最终 ELF SHA-256：`2532892035c8f1916f6ffd55bd05158f6364f640231ca81b0f05b749c7d34cf7`；串口启动显示 `253289203...`，与实际刷入构建一致。ESP-IDF 的 Compile time 是缓存 C 组件的时间，不用它判断 Rust 镜像是否更新。
- 应用大小 `1,813,296 / 2,359,296` 字节（76.86%）。按 AGENTS.md 完整 bootloader/partition/factory 参数、`--no-skip` 烧录，仅清理 otadata；命令未擦除 NVS、holding 或 storage。
- 烧录和监控启动会人为复位。此前交接记录中的一次 Ctrl+R 同样属于人工复位，不能计为设备自发故障。

## 根因与修改

1. 实板 `IC_REF=0x26`，不是代码此前严格要求的 KC `0x51`。ST25DV16K 是合法代际；以 K/KC 身份及独立容量签名联合判断，拒绝未知/容量不符芯片。
2. K 的 MB_WDG 在 `0x000E bits2:0`；KC 在 FTM `0x000D bits3:1`，KC 的 `0x000E` 是 I2C_CFG。驱动按代际处理并读回校验，不混写。
3. 处理 Mailbox 使能/读取失败；新 MCU 会话清除芯片残留消息，应答每片等待上限 10 秒，失败清理并重探测。直接请求取消旧分片组装。
4. BLE 主循环不再阻塞等待 NFC 持有的协议分发锁，改为保留 RX_READY 并重排队；NFC 落盘等待不会通过这把锁拖住 I/O。
5. 更新“当前板未焊接”和“0x26 一定错误”等过期结论；K 代寄存器与 ST 官方驱动、ES0616 勘误交叉核对。

## 实机读值与启动结果

```text
ST25DV16K detected on SDA=38 SCL=37
system=0x57 data=0x53 IC_REF=0x26 MEM_SIZE=0x01FF BLOCK=0x03
ENDA1=8 I2CSS=4 I2C_SSO=1
RF Mailbox ready (generation=K, MB_EN=1, watchdog=off)
```

- 完整容量签名：2048 字节用户 EEPROM，4 字节 RF 块。
- 最终启动约 2.232 秒进入 Mailbox ready；NDEF/快照维护及安全会话路径成功完成，没有静默跳过识别失败。
- 旧快照与本机不同，固件记录等待显式 restore；未自动恢复旧快照。该结果不等于本轮验证了显式备份/恢复。
- BLE 广播、PCA9555 worker、Web 和四个 Modbus TCP 监听正常启动；IP 分配为 `192.168.1.200`，本轮未修改网络参数。

## 软件验证

| 检查 | 结果 |
| --- | --- |
| NFC 生产方法/协议/身份主机测试 | 17 passed |
| BLE/NFC 分发锁竞争、重排队、过期请求 | 5 passed |
| BLE 广播恢复与连接回调 | 22 passed |
| 生产核心 | 27 passed |
| PCA9555 单/三芯片 IO | 8 + 8 passed |
| F3、F4、无 BLE 的 RTU cargo check | 通过，无警告 |
| F3 Clippy `-D warnings` | 通过 |
| F3 单元测试目标 `--no-run` | 编译检查通过，不计为板上执行 |
| fmt / diff | 通过 |

NFC Host 提取实际生产方法执行，不只测试同一编码器的自洽性；覆盖 CRC/golden vector、512 字节边界、K/KC 独立看门狗位置、无 VCC、读回不一致、1000 次空闲轮询不新增 EEPROM 写入。ES0616 第 6.1 节的 256 字节长度提前释放和第 6.3 节的主机写后 NACK 均有模型注入测试；ACK polling 不重放已可能投递的 payload。

## 稳定性观察与边界

- 第一份识别修复构建记录至设备日志 699.811 秒；最终构建另有独立启动与健康采样至 275.108 秒，两轮非连续计时，具体终点见 JSON。
- 两份已采集日志均只有一次工具触发的启动记录，未见后续 panic、WDT、stack canary 或 NFC Warning/Error。
- NFC 10 KiB 栈最低剩余 3160 字节（空闲初始化/轮询观察），不是满业务负载栈验收。稳定阶段 free heap 约 2043 KiB，internal 最低约 36 KiB。
- 常态主循环 work p95 上界 1 ms、p99 上界 2 ms；日志中仍存在周期性约 110 ms 的 max_work，初始未识别构建也存在同量级峰值。不能把分位数当作最大时延或实时保证，本轮未对其做归因/优化。
- 监控器多次报告串口断开重连，设备 uptime 连续；不能将串口采集链路异常忽略为零，也不能据此宣称 MCU 重启。计数及缺失采样见 JSON。
- RS485-1 持续有 slave=1 FC04 空响应超时；UDP 未配置允许来源而拒收、UART0 用作控制台导致 RS485-3 禁用也有日志。本轮未宣称这些现场链路均通过。
- 尝试访问 HTTP 状态接口超时，因此没有将网络请求/认证 Web/NFC 维护 API 计为通过。
- **未完成**：实体 iPhone/Android 与天线的 RF 往返、全部 BLE 对等业务、20+ 组态交互、IP/485/组态写入后的实际回读/落盘/重启、NFC 备份恢复、离场/重入、断电与勘误电气条件。当前验收结论为“实板 I²C、完整签名、安全会话与 Mailbox 初始化通过”，不等于 NFC 全业务验收或 7×24/零故障承诺。

## 证据

- 初轮拒绝日志：`log/hardware/nfc/st25dv16kc-f3-bringup-2026-11-11.log`
- K/KC 识别修复观察：`log/hardware/nfc/st25dv16k-f3-fixed-2026-11-11.log`
- 最终完整刷写：`log/hardware/nfc/st25dv16k-f3-final-flash-2026-11-11.log`
- 最终启动/健康记录：`log/hardware/nfc/st25dv16k-f3-final-2026-11-11.log`
- 汇总计数：`log/hardware/nfc/st25dv16k-f3-verification-2026-11-11.json`
- 驱动依据与代际说明：`docs/hardware/nfc-st25dv16kc.md`

最终收尾：87 次 Host 用例执行均通过（NFC 17、BLE/NFC 分发 5、BLE 广播 22、核心 27、IO 两型号 16）；F3 单元测试目标编译、fmt 与 diff 检查通过。日志采集已正常关闭，结束采集时未发送复位命令。
