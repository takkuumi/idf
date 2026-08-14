# 端口性能审计记录（2026-08-13）

## 目标

在不改变 Modbus TCP/RTU、BLE 手持机、Web、NFC 和 IO 业务协议的前提下，
收紧各端口请求的调度延迟和异常外设造成的最坏阻塞时间。

## 发现

- Modbus TCP 由 20ms main-loop tick 驱动，单请求有 0..20ms 额外调度延迟。
- TCP 每连接每 tick 只处理一帧，流水请求吞吐受 tick 人为限制。
- Web 无连接时每 100ms 轮询 accept，新连接最坏额外等待近 100ms。
- BLE 业务请求和 notification 每 100ms 处理，手持机连续配置体感迟缓。
- F3/F4 MCP23017 I2C 单次交易超时为 1000 tick，外设异常可阻塞共享
  main-loop/TCP 近 1s。
- FC03 连续读每字都重复登记 CONFIG/STORAGE RCU 读者，125 words 会
  放大原子操作数量，且同一响应可跨越多个快照。
- RTU 主站已由 `uart_wait_tx_done` 确认发送完成，之后仍固定等待 2ms；
  最后一次失败后还会额外等待 100ms。

## 处理

- main-loop 网络基准改为 5ms；TCP 先于 I2C/ADC 热路径执行。
- DI 仍为 20ms 去抖，AI/AO 仍为 100ms，不放大硬件轮询负载。
- DO dirty 消费改为 5ms，只在值变化时写硬件，10s 兜底周期保持不变。
- TCP 单连接每 tick 最多处理 4 个流水请求，并增加服务器级每 tick 业务总预算；
  发送背压时立即让出，避免多个连接叠加后阻塞 5ms main-loop。
- Web accept 空闲轮询改为 10ms。
- BLE 业务处理改为 10ms，广播恢复 5s 和心跳 10s 按真实时间保持。
- MCP23017 I2C 超时改为显式 20ms 转 FreeRTOS ticks。
- FC03 在整个请求期间持有同一 CONFIG/STORAGE RCU 快照，保留标准 125 words。
- RTU 移除冗余 2ms 方向等待，最后一次失败不再等待下一次重试间隔。
- main-loop 每 60s 输出最大工作耗时与 5ms deadline miss 计数，用于实机闭环。
- 实机首轮 8 客户端压测触发 `EMFILE (errno 23)`；根因是 ESP-IDF 仍使用
  `CONFIG_LWIP_MAX_SOCKETS=10`。按 15 个固定峰值需求加重连余量改为 20。
- Web 客户端设置 `TCP_NODELAY`；每秒 uptime INFO 改为每分钟，完整栈表改为
  每 10 分钟，避免串口日志造成 70ms 级 main-loop 尖峰。
- TCP accepted/closed 连接日志降为 DEBUG，避免短连接压力下串口日志锁进入实时路径。
- RTU 主站失败 WARN 按 10s 限频并报告 suppressed 数量；错误计数寄存器仍逐次累加。

## 静态验证

- `cargo check --bin gateway`：通过，0 warning。
- `cargo check --bin gateway --features f3`：通过，0 warning。
- `cargo check --bin gateway --features f4`：通过，0 warning。
- `cargo test --bin gateway --no-run`：通过。
- `git diff --check`：通过。

## 实机压测边界

## 实机结果（ESP32-S3 rev 0.2，8MB Flash，2MB PSRAM，192.168.51.140）

最新预算调整前后均使用标准有效地址 `0x0894`、83 words（PC DeviceMMP 窗口）验收：

- 502/503/504/5002 单连接 20 次：平均 9.5/10.5/10.1/10.2ms，P95 12.0/14.5/10.9/13.8ms。
- 8 个持久连接 × 50 次 FC03：400/400 成功，平均 19.75ms，P95 32.68ms，P99 97.27ms，最大 129.12ms。
- Web `/login` 30 个短连接：平均 23.84ms，P95 29.20ms，最大 31.84ms。
- FC03 125 words 响应长度 259 字节；FC03 非法地址正确返回异常响应，不计为性能失败。
- 空闲遥测：free heap 约 2055KB，minimum heap 约 2036KB；无 stack canary、pthread 创建失败、EMFILE 或重启。

压力期间主循环遥测曾记录 5.2~19.6ms 最坏工作时间，说明 8 路同时发送大帧时仍存在调度尾延迟；
首轮将服务器级预算收紧为每 5ms 最多 2 个请求；LOOP33 根据分阶段实测进一步
收紧为 1 个，以优先保障 7×24 运行时的主循环 deadline、连接公平性和 WDT 安全。

## TCP 工业健壮性加固

- 已建立连接显式启用 TCP keepalive：空闲 30s、探测间隔 10s、连续 3 次失败回收半开连接。
- 半帧接收设置 30s 绝对截止时间；响应发送设置 5s 绝对截止时间，防止 slowloris/慢接收端永久占槽。
- 客户端和监听端口采用轮转游标；每个客户端每轮最多消耗一个业务预算，避免高负载下固定下标饥饿。
- 每轮最多 accept 2 个连接，拒绝连接和非法 MBAP 日志限频，连接/SYN 洪泛不会拖垮 main-loop。
- 监听 accept 或重绑出现异常时，1s 后自动恢复，失败后 5s 退避重试；已建立客户端不随监听器恢复丢失。
- 环网二层环路、广播风暴和 RSTP/ERPS 保护不属于 TCP Server 能力，现场交换机必须启用生成树/环网协议及广播抑制。

本轮为验证 keepalive/监听自愈曾使用合并镜像从 `0x0` 写入真实设备，导致设备 NVS 恢复默认值；
后续量产/现场升级必须只写应用分区或使用项目烧录脚本，禁止用合并镜像覆盖 `0x9000` NVS。

## 后续验收边界

交付前仍建议在真实业务流量下测量：

- 502/503/504/5002 同时连接，8 客户端持续 FC03/FC06/FC10。
- FC03 125 words 和 FC10 123 words 的平均/P95/P99 响应时间。
- TCP + BLE + Web + RTU + DI/DO/AI/AO 并发时的丢包、队列丢弃和 WDT/stack 水位。
- 拔除 F3/F4 I2C 外设时 TCP 仍可响应，且不发生非计划重启。

## 固定缓冲与复制收敛（2026-08-13）

- FC03/04 后端直接把最多 125 个寄存器编码到最终 PDU 缓冲，移除约 250B
  `heapless::Vec<u16>` 中间栈对象和第二次序列化遍历；FC03 整批仍只登记一次
  CONFIG/STORAGE RCU 读者，响应快照一致性不变。
- BLE TX 从 2KB 连续字节队列改为 8 个 272B 固定帧槽。ATT 分片只推进槽内
  `offset`，不再将整个剩余队列 `rotate_left`，也不再复制到 497B 临时分片数组。
- BLE RX 使用 4 个 512B 静态重组槽；BTC 回调完成帧后只向 main-loop 队列传递
  1B 槽索引，不再复制完整 512B 请求对象。连接 epoch 和断线清理语义保持不变。
- Web 请求方法、路径、Cookie、请求行和 header 行改为固定容量；仅保留业务实际
  使用的 Cookie/Content-Length，不再为最多 32 个 header 分配键值 String。
- Web URL/form 解码使用固定字节缓冲并在完成后校验 UTF-8，中文设备名、厂家名和
  位置字段兼容性保持不变；通用 JSON 响应使用固定 128B 字符串。

## LOOP31 固件实机只读回归（2026-08-13）

- 硬件：ESP32-S3 rev 0.2、8MB Flash、2MB PSRAM；静态 IP `192.168.51.221`。
- factory 镜像完整加载，W5500、BLE GATT、UDP、NFC、Web、RTU master/slave、
  Modbus TCP 四端口均启动；约 9.9 秒进入 5ms main-loop。
- FC03 `0x0880 + 125 words` 在 502/503/504/5002 全部成功，四端口返回相同首尾值
  `0x009D/0x0013`。`0x0800 + 125` 跨未映射地址时四端口均返回标准异常响应。
- Web 使用默认 `admin/admin123` 登录；系统、网络、端口、IO、运行状态、NFC 六个
  查询接口均为 HTTP 200，中文系统名称未出现 UTF-8 截断或替换。
- 240 秒观察期无重启、stack canary、pthread 创建失败、EMFILE、WDT 或任务停滞；
  总 free heap 约 2054KB，minimum heap 约 2038KB。最后一分钟 main-loop
  `max_work=4626us`、`deadline_miss=0`。
- 前三分钟曾见 `max_work=9637us/deadline_miss=4`，现增加 debug-only 九阶段峰值
  统计以区分 TCP、AI/AO、DI、DO、持久化、ETH、BLE、事件和 housekeeping。
- 总 heap 主要由 PSRAM 构成，不能证明 pthread/DMA 余量；新增 internal SRAM
  当前值和历史最低值，每分钟输出并在低于 32KB 时告警。
- 首次烧录被调试端提前终止，bootloader 报 factory segment 尾部 `0xffffffff` 并
  安全回退 ota_0。完整重烧后 factory 全部六个 segment 校验并正常启动。

## 分阶段热点收敛（LOOP33）

- 压力首分钟：`max_work=14264us`、`deadline_miss=181`；阶段峰值 TCP `12254us`、
  DI `14064us`、persist `4330us`，BLE `282us`、AI/AO `848us`。
- 压力结束后的空闲分钟：TCP 峰值约 `899..1048us`，DI 仍约 `4216..5258us`，
  证明 TCP 峰值来自并发大帧，而 DI 软件 I2C 是常驻周期成本。
- F16 DI 双端口由两次单字节读取合并为一次连续读取；DI LED 由每周期两次写改为
  仅变化时一次连续双字节写。业务位序、采样与去抖保持不变。
- TCP 每 5ms 全局请求预算由 2 收紧为 1；已有 TX flush、超时检查和客户端轮转
  不受预算限制，弱网恢复和8连接公平性保持。
- 同口径 8 客户端 x 50 次 FC03 125 words 实机对比将在完整重烧后补录。

## LwIP PSRAM A/B 与栈遥测收敛（LOOP34）

- 固件：`66aa7693b181`；硬件：ESP32-S3 rev 0.2、8MB Flash、2MB PSRAM。
- 启用 `CONFIG_SPIRAM_TRY_ALLOCATE_WIFI_LWIP=y` 后，LwIP 通用动态对象优先使用
  PSRAM，内部 SRAM 不足时的历史低水位由 `6311B` 提升并稳定在约 `39KB`；
  W5500 DMA 缓冲和 pthread 栈仍保持内部内存分配。
- 同口径并发压力：8 客户端各 50 次 FC03，地址 `0x0880`、125 words，端口
  502/503/504/5002 各两连接，结果 `400/400`；平均 `39.20ms`，最大
  `123.65ms`。并发 20 次认证 `/getsystemstatus` 结果 `20/20`，平均
  `99.93ms`，最大 `152.38ms`。
- 压力后总 heap `2054KB`、历史最低 `2053KB`；内部 SRAM 当前约 `40KB`、
  历史最低约 `39KB`。未见 W5500/LwIP 错误、连接失败、panic、stack canary、
  pthread 创建失败或复位。
- 7 个用户任务全部采样成功，最低剩余栈为 UDP `4300B`；最高占用为 NFC 40%，
  其次 RTU master 39%、UDP 30%、RTU slave 29%，所有任务均满足剩余栈
  `>=1024B` 且占用 `<90%`。
- 每分钟逐任务输出 7 行栈日志会让 debug housekeeping 单次达到约 `66ms`，属于
  诊断串口阻塞而非业务耗时。健康状态现压缩为单行汇总，低水位任务仍逐项 ERROR，
  不改变采样、告警阈值、协议或 IO 行为。
- 默认、F3、F4 `cargo check` 与 `cargo test --bin gateway --no-run` 均通过，
  **0 error, 0 warning**。

## LOOP35 Web 持久化阻塞复盘（2026-08-14）

- 首版 Web 修复曾在 HTTP 请求线程同步调用 `SystemConfig::save_to_nvs`，并在网络
  保存后热重配活动 netif；实机恢复原值后并发连接阶段出现 Web/TCP 暂时失联。
- 根因是 Flash/NVS 写入与 W5500 netif 重配进入通信请求关键路径，不能以短时成功
  掩盖工业系统的尾延迟风险。
- 当前改为 Web 只写 RCU：SystemConfig 由 `CONFIG_DIRTY` 交给 DeviceActor 合并
  持久化，Web 系统信息 blob 由独立 dirty 标志交给同一 Actor；NVS 失败自动保留
  dirty 并重试。网络配置不在线修改活动 netif，重启时统一加载。
- DI/DO 数值排序和 1 基页面标签保持不变；内部 DO 控制地址仍为 0 基，手机/PC/
  Modbus 兼容接口未改变。
