# ESP32-S3R2 任务与内存架构

> 更新：2026-08-13。容量数字来自最终 `cargo build` 生成的 sdkconfig 和 linker map。

## 硬件容量边界

- MCU：ESP32-S3R2，双核 Xtensa LX7 240MHz。
- 片上 SRAM：物理总量 512KB；不能把该数字直接当作 FreeRTOS heap。
- 片外 PSRAM：2MB Quad SPI，80MHz。
- 最终链接 DRAM 段：`0x53700 = 333.75KB`。
- 最终 `.dram0.data + .dram0.bss`：`0x65C9 + 0x7108 = 54,993B (53.70KB)`。
- `.dram0.bss` 结束至 DRAM 段末：`172,280B (168.24KB)`，这是当前连续内部
  DRAM heap 候选区，不等于启动完成后的 `esp_get_free_heap_size()`。
- `CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=65536` 为 DMA/内部专用申请保留 64KB。
- `build.rs` 将 2MB PSRAM、64KB internal reserve、20 sockets、DIO/40MHz/8MB、
  NVS 地址和三个 2.25MB 应用分区设为构建硬门槛；配置漂移会直接中止构建。
- pthread 默认 `stack_alloc_caps` 是 `MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT`；启用
  `FREERTOS_TASK_CREATE_ALLOW_EXT_MEM` 不会自动把 Rust pthread 栈迁到 PSRAM。
- 2026-08-13 release raw app 镜像为 `1,626,928B`，占 2.25MB OTA 槽约 69.0%；
  ELF 中静态内部 DRAM 为 `.data 25,821B + .bss 28,032B = 53,853B`。发布配置保持
  `-O3`、fat LTO、单 codegen unit、strip 和 abort panic，未以牺牲实时性换取更小代码。

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
| main | 32KB | 1 | 5ms TCP/DO、10ms BLE、20ms DI、100ms AI/AO、健康监控 |
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
保留 8 连接上限、5 分钟 idle 回收、5 秒发送背压绝对超时和原 MBAP/PDU 格式。连接建立
只在预留 `Vec<Client>` 中增加状态，不创建 pthread。状态机由 main_loop 每 5ms
非阻塞轮询；一次性预留失败会返回启动错误，运行中不扩容。单连接每轮最多处理
4 个流水请求，但全局每轮最多执行 2 个业务请求；accept 每轮最多 2 个，监听器和
客户端采用轮转游标，异常客户端不能饿死其它连接或无限占用调度循环。

监听器异常会在 1 秒后重绑，连续失败按 5 秒退避；连接显式启用 TCP keepalive
（30 秒空闲、10 秒探测、3 次失败），若底层构建不支持细项则降级到应用层超时，
不会拒绝本可正常工作的客户端。

连接状态总分配超过 4KB，按当前 `SPIRAM_MALLOC_ALWAYSINTERNAL=4096` 策略进入
PSRAM；socket和 DMA 仍保留在 internal SRAM。NFC 的两个 4KB 工作区也通过 capability
allocator 明确放入 PSRAM，不再依赖 4096B 阈值的边界行为。

## Modbus 报文与栈边界

Modbus TCP 严格采用标准最大边界：PDU 253B、MBAP `length` 254B、完整 TCP ADU
260B；FC=01/02 支持 2,000 位读取，FC=03/04 支持 125 寄存器读取，FC=0F 支持
2,000 位写入，FC=10 支持 123 寄存器写入。不存在 60 字长度上限，60 仅是旧 PC
工具的常用逻辑块尺寸。

每个 TCP 客户端保留一个完整 260B 接收 ADU 和一个 260B 发送 ADU。处理完首帧后，
同一缓冲中的后续流水字节会前移，超出当前缓冲的字节保留在 LwIP socket 队列，因此
不牺牲流水请求兼容性，同时八连接减少 2KB PSRAM 状态。

FC=01/02/0F 不再将标准最大 2,000 位展开为 `heapless::Vec<bool, 2000>`；读取直接
写入响应位图，写入直接消费请求位图。单次 Modbus 调用由此移除约 2KB 临时栈对象，
仍保留全部标准数量上限。

FC=03/04 同样直接把最多 125 个寄存器编码到最终 PDU，不再构造约 250B 的中间
`heapless::Vec<u16, 125>` 或执行第二次序列化。FC03 整批读取只持有一组
CONFIG/STORAGE RCU 快照，避免同一响应跨配置世代，寄存器地址和返回字节序不变。

## BLE 栈隔离

BTC 回调只执行：GATT 写响应、最多 512 字节分片重组、固定 4 槽所有权转移。
Modbus/手持机兼容命令在 main_loop 消费，原事务 ID、CRC、长度字段和通知帧格式不变。
业务请求和 notification 按 10ms 调度；广播自愈仍为 5s，心跳仍为 10s。
因此 `metuory-wireless-management-app-1.0.78` 的协议表面不变，而业务调用深度不再
叠加到 BTC_TASK。RX 的 4 个 512B 静态槽在完整帧就绪后仅向 main-loop 队列传递
1B 槽索引，不复制请求帧；连接 ID 和 epoch 阻止旧连接数据进入新连接。

下行使用 `8 x 272B` 固定帧环，保留完整业务帧 FIFO 边界；按实际协商的
`ATT_MTU - 3` 分片时只推进槽内 offset，不搬移剩余队列，也不构造临时分片副本。
拥塞/API 失败时保留原帧；手机端 `CommandCodecUtil.decodeList()` 已确认可跨
notification 重组。ESP-IDF Bluedroid 在 API 返回前仍执行其内部必要深拷贝。

## Web 有界解析

HTTP 方法、路径、Cookie、请求行、header 行和表单字段采用固定容量缓冲；解析器
只保留业务使用的 Cookie 与 Content-Length，不再为最多 32 个 header 构造动态键值。
URL/form 解码先写入有界字节缓冲，再一次性校验 UTF-8，中文配置字段保持兼容。
普通请求 body 仍按声明长度分配但硬限 16KB；OTA 保持流式写入，不缓存完整镜像。

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
