# ESP32-S3R2 任务与内存架构

> 更新：2026-07-30。容量数字来自最终 `cargo build` 生成的 sdkconfig 和 linker map。

## 硬件容量边界

- MCU：ESP32-S3R2，双核 Xtensa LX7 240MHz。
- 片上 SRAM：物理总量 512KB；不能把该数字直接当作 FreeRTOS heap。
- 片外 PSRAM：2MB Quad SPI，80MHz。
- 最终链接 DRAM 段：`0x53700 = 333.75KB`。
- 最终 `.dram0.data + .dram0.bss`：`0x65C9 + 0x7108 = 54,993B (53.70KB)`。
- `.dram0.bss` 结束至 DRAM 段末：`172,280B (168.24KB)`，这是当前连续内部
  DRAM heap 候选区，不等于启动完成后的 `esp_get_free_heap_size()`。
- `CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=65536` 为 DMA/内部专用申请保留 64KB。
- pthread 默认 `stack_alloc_caps` 是 `MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT`；启用
  `FREERTOS_TASK_CREATE_ALLOW_EXT_MEM` 不会自动把 Rust pthread 栈迁到 PSRAM。

## 根因

旧架构用扩大栈掩盖深调用链：DeviceActor 32KB，Modbus TCP 每连接 20KB，BTC/BTU
各 12KB。8 个 TCP 连接会额外申请 160KB internal SRAM，超过本机实际余量，结果在
不同负载下表现为 pthread ENOMEM、heap 碎片或 Stack canary，并非单一任务栈太小。

另一个根因是 BLE GATT 写回调直接在 BTC_TASK 中执行 Modbus、配置 RCU 和持久化
通知。BTC 栈大小因此与业务深度耦合，任何业务扩展都可能重新引入溢出。

## 当前任务模型

所有生产 pthread 栈值集中在 `src/safety/stack_budget.rs`，业务模块禁止硬编码。

| 用户任务 | 栈 | 数量 | 调度职责 |
|---|---:|---:|---|
| main | 32KB | 1 | 20ms tick、TCP/IO、100ms AI/AO/BLE 分频、健康监控 |
| DeviceActor | 16KB | 1 | NVS 串行持久化；3010B blob 已移至 PSRAM 缓冲 |
| mb-rtu-master | 8KB | 1 | RS485 主站 |
| mb-rtu-slave | 8KB | 1 | RS485 从站 |
| udp-mcast | 6KB | 1 | UDP 组播接收 |
| nfc-st25 | 8KB | 1 | NFC 备份/恢复 |
| http-srv | 12KB | 1 | Web 配置与流式 OTA |
| **默认用户任务合计** | **90KB** | **7** | 固定，不随 TCP 连接数变化 |

可选 RTU3 为 8KB，可选 Wi-Fi heartbeat 为 6KB；全部启用时用户任务上限 104KB，
编译期断言限制为不超过 128KB。

主要系统任务的保守配置合计约 48.5KB：BTC 8KB、BTU 8KB、LwIP 8KB、系统事件
4KB、FreeRTOS timer 4KB、W5500 RX 4KB、BT controller 3.5KB、esp_timer 3.5KB、
双核 IPC 2.5KB、双核 Idle 3KB。用户默认任务和这些系统任务合计约 138.5KB。

## 固定连接状态机

Modbus TCP 默认 502/503/504/5002，端口寄存器 2243-2246 持久化后可在运行时重绑；
保留 8 连接上限、5 分钟 idle 回收、2 秒发送背压超时和原 MBAP/PDU 格式。连接建立
只在预留 `Vec<Client>` 中增加状态，不创建 pthread。状态机由 main_loop 每 20ms
非阻塞轮询；一次性预留失败会返回启动错误，运行中不扩容。每个周期限制 accept
数量，写端背压有独立超时，异常客户端不能无限占用调度循环。

连接状态总分配超过 4KB，按当前 `SPIRAM_MALLOC_ALWAYSINTERNAL=4096` 策略进入
PSRAM；socket和 DMA 仍保留在 internal SRAM。NFC 的两个 4KB 工作区也通过 capability
allocator 明确放入 PSRAM，不再依赖 4096B 阈值的边界行为。

## BLE 栈隔离

BTC 回调只执行：GATT 写响应、最多 512 字节分片重组、固定 4 槽 MPSC 入队。
Modbus/手持机兼容命令在 main_loop 消费，原事务 ID、CRC、长度字段和通知帧格式不变。
因此 `metuory-wireless-management-app-1.0.78` 的协议表面不变，而业务调用深度不再
叠加到 BTC_TASK。下行通知按实际协商的 `ATT_MTU - 3` 无堆分片，拥塞/API 失败时
保留队列；手机端 `CommandCodecUtil.decodeList()` 已确认可跨 notification 重组。

## 服务恢复与 OTA 生效门槛

- Web、UDP、NFC、Modbus TCP/RTU 的 socket/I2C 主循环内部退避恢复，不因链路故障退出。
- pthread 启动因瞬时资源不足失败时，main supervisor 每 5 秒只重试未启动服务；已运行
  任务不会重复创建。
- 健康任务停滞只记录并保持其余业务在线，不做软件整机重启。
- 新 OTA 镜像须运行 30 秒、无停滞、全部协议和辅助任务均已启动，才取消 bootloader
  回滚保护。

## 运行时水位闭环

每个真实常驻用户任务首次 `TaskHb::tick()` 捕获自己的 `TaskHandle_t`。main_loop
每 60 秒调用 `uxTaskGetStackHighWaterMark2`，记录 ESP-IDF 定义的“历史最小剩余
字节”。低于 1024B 或使用率达到 90%只记错误和降级诊断，不主动重启。

最终缩栈必须依据真实硬件水位：所有任务在峰值业务下至少保留 1KB且不超过 90%。
编译通过只能证明结构和预算生效，不能代替 72 小时 BLE + 8 TCP + RTU + Web/NFC
并发浸泡测试。
