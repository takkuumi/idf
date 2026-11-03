# 工业化架构审查与整改记录

日期：2026-07-29 至 2026-07-30

## 审查边界

- 目标固件：ESP32-S3R2、512KB 物理 SRAM、2MB Quad PSRAM、8MB Flash。
- SDK：本地只读 ESP-IDF v5.5.4。
- 兼容基线：原 C++ 固件和 `metuory-wireless-management-app-1.0.78`，均只读。
- 本轮未烧录硬件；本文的“通过”仅代表源码对照、静态检查和交叉编译通过。

## P0 根因与整改

1. 任务栈来自 internal SRAM，旧 TCP 每连接创建 pthread 会随连接数线性耗尽 SRAM。
   初步合并后的单个 16KB TCP 任务仍在真机出现 `pthread ENOMEM`；最终取消该任务，
   四个 listener 和八个预分配连接由 20ms main-loop 非阻塞状态机管理。
2. BLE BTC 回调曾直接执行配置/Modbus/NVS 深调用。现在回调仅做有界重组入队，业务在
   main-loop 执行；下行按协商 ATT MTU 分片且保留外层帧。
3. `AtomicBits64` 原伪 seqlock 不支持多写者。现在 CAS 获取唯一奇数写序，避免 BLE、
   TCP、RTU、Web 并发写 DO 时半写或丢位。
4. W5500 DMA staging 原不可变 static 被裸指针写入，属于 Rust 未定义行为。改为
   `UnsafeCell` 并由 FreeRTOS mutex 串行；事件先注册再启动 Ethernet。
5. Web OTA 原 BufReader 拆回 TcpStream 会丢失预读的固件头。现在完整 reader 直接进入
   OTA 流，单次 read 最长 1 秒并持续喂 WDT。
6. OTA 原进入 main 即确认镜像有效。现在全部服务启动、运行 30 秒且无停滞后才取消回滚。
7. ST25DV 原混用 0x53/0x57 地址、密码和会话寄存器错误。按官方协议使用双地址、17 字节
   密码展示、0x2004 SSO、30 字节 EEPROM 事务和 4096B 兼容快照；I2C 从不稳定的
   约 250kHz 恢复为原系统的约 100kHz，两个 4KB 工作区显式驻留 PSRAM。
8. `MainLoopCell` 原安全接口可从 `&self` 返回可逃逸 `&mut T`，属于引用别名 UB。
   改为 CAS 独占借用的闭包 API，递归/并发访问立即拒绝，RAII 保证释放。
9. UDP `ip_mreq` 原用 `from_be_bytes`，在小端芯片内存中把 `239.0.0.1` 反转成
   `1.0.0.239`，对应真机 `EADDRNOTAVAIL(125)`；现按 `in_addr` 内存语义修复并加测试。

## 业务兼容结论

- 手机协议外层仍为 `tx_id | protocol_id | length | PDU | CRC16-LE`。
- 手机 `CommandCodecUtil.decodeList()` 使用静态累计状态，可跨多次 notification 重组；
  ATT 分片不会改变事务号、长度或 CRC。
- GATT UUID、广播名称 m/M 前缀、寄存器地址和九条 NFC NDEF 文本顺序未改变。
- TCP 默认端口仍为 502/503/504/5002；2243-2246 写入现在真实持久化并动态重绑。
- OTA 三个 app 分区均为 0x240000，Web 上传上限与目标槽一致。

## 内存与任务预算

- 默认用户任务栈：90KB；全可选任务：104KB；编译期上限：128KB。
- 已知主要系统任务预算：36KB，另保留 64KB internal heap 给 DMA/内部专用申请。
- 当前 linker map：DRAM 段 0x53700；data+bss 54,993B；bss 后连续候选 172,280B。
- 每 60 秒采集真实 FreeRTOS stack high-water mark；低于 1KB只告警和降级，不软件重启。

## 仍需真机闭环

以下项目不能由交叉编译证明，必须在维护窗口完成后才能声明 100% 和 7x24：

1. 手机 1.0.78 全配置导入/导出、BLE MTU 23/500、断连重连和大帧读写。
2. 八路 TCP 长连接、四端口动态重绑、RTU 主从、UDP、Web 并发。
3. NFC 新标签/旧 C++ raw 标签、掉电中断写、CRC 拒绝和 NDEF 手机读取。
4. OTA 正常升级、断流、超长、验签失败、30 秒确认与 bootloader 回滚。
5. 连续 72 小时峰值压力，记录所有任务最小剩余栈、minimum free heap 和 reset reason。
