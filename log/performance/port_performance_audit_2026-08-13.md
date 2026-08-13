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
- TCP 单连接每 tick 最多处理 4 个流水请求，并增加服务器级每 tick 2 帧总预算；
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
最终固件将服务器级预算收紧为每 5ms 最多 2 个请求，以优先保障 7×24 运行时的公平性和 WDT 安全。

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
