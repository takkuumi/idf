# 生产环境稳定性保障方案

## 已发现的崩溃问题

### 已修复的崩溃

| # | 崩溃现象 | 根因 | 修复 |
|---|---------|------|------|
| 1 | `device init ok` 后 `LoadProhibited` (读取 NULL) | `esp_reset_reason()` 在后台线程运行时调用导致竞态 | 移到 `device::init()` 之前执行 |
| 2 | `device init ok` 后 `InstrFetchProhibited` (PC=0，函数指针损坏) | `catch_unwind` 在 ESP-IDF 上不稳定 | 去掉 catch_unwind，直接调用 |
| 3 | `process_loop` 线程静默崩溃 | 4096 字节栈溢出 (log::info! + Mutex 操作) | 移除独立线程，改为 main loop 轮询 |
| 4 | Task watchdog 触发 (main loop 阻塞 10s+) | process_tick 内部死循环未返回 | process_tick 改为单次调用 |
| 5 | BLE 响应从未到达 Android | send_ble_frame 写入 BINARY_TX 但无人消费 | process_tick 优先消费 BINARY_TX |
| 6 | 连接后 25 秒 Android 主动断开 | send_indicate 在 GATT 回调中调用导致死锁 | 数据队列 + main loop 发送 |

### 未修复的残余风险

#### 1. 11 个线程使用默认 3KB 栈（严重）

以下线程没有显式设置 `.stack_size()`，使用 ESP-IDF 默认 ~3KB：

```
wifi/mod.rs:108      - Wi-Fi 线程
io/di.rs:45          - DI 扫描线程
io/do_.rs:40         - DO 输出线程
ethernet/w5500.rs:296 - 以太网心跳线程
channel/ai.rs:40     - AI 采样线程
channel/ao.rs:38     - AO 输出线程
modbus/rtu_master.rs:57  - RTU 主站线程
modbus/rtu_slave.rs:31   - RTU 从站线程
modbus/tcp_server.rs:39  - TCP 监听线程
modbus/tcp_server.rs:56  - TCP 连接处理线程
```

**风险**: 3KB 栈对 Rust 线程过小，`log::info!` + 字符串格式化 + FFI 调用很容易溢出。
**修复方案**: 每个 `.spawn()` 添加 `.stack_size(8192)`。

#### 2. 11 个 unwrap/expect 调用点

这些调用点如果触发会直接 panic，导致系统重启：

```bash
# 分布在 device/mod.rs, bus.rs, config.rs 等
# 主要是 NVS 操作和 Mutex 锁
```

**风险**: NVS 损坏或 Mutex 死锁时直接 panic。
**修复方案**: 替换为 `?` 操作符 + 错误处理，或 `unwrap_or_default()`。

#### 3. 连续 core dump 损坏 NVS

每次崩溃后，ESP-IDF 尝试保存 core dump 到 flash，但 core dump 分区配置损坏，失败后仍会耗用 flash 写入寿命。

**风险**: Flash 磨损 + NVS 区域被 core dump 覆盖。
**修复方案**: 在 `sdkconfig.defaults` 中启用 core dump 分区，或禁用 core dump。

## 生产环境保障措施

### 1. 线程栈大小 - 立即修复项

```rust
// 所有 .spawn() 必须设置栈大小
std::thread::Builder::new()
    .name("task-name".into())
    .stack_size(8192)  // 最小 8KB
    .spawn(move || { ... });
```

建议值:
| 线程类型 | 建议栈大小 | 原因 |
|---------|-----------|------|
| IO 扫描 (DI/DO) | 8192 | log::info! + I2C 操作 |
| 模拟量 (AI/AO) | 8192 | ADC/LEDC 操作 |
| Modbus RTU | 16384 | CRC + UART + 协议处理 |
| Modbus TCP | 16384 | Socket + 多连接 |
| 以太网 | 8192 | SPI + LwIP |
| 配置监听 | 16384 | NVS + CRC + 日志 |

### 2. 无 panic 设计

```rust
// 错误处理原则
// ❌ 不要：
let v = some_result.unwrap();
let v = some_result.expect("message");

// ✅ 要：
let v = some_result?;  // 或
let v = some_result.unwrap_or_default();
let v = some_result.unwrap_or_else(|e| {
    log::error!("fallback: {}", e);
    Default::default()
});
```

### 3. 多层 watchdog 保护

```
主循环喂狗 (100ms) ──→ Task WDT (10s 超时)
        ↓
健康检查 (1s/次) ──→ 线程 stall 检测
        ↓
复位计数 ──→ 连续复位超过 N 次 → 进入安全模式
```

### 4. 安全模式

当系统检测到连续崩溃时:
```
正常模式 → 检测到崩溃 → 保存复位原因
    ↓                      ↓
重启次数 < 5           重启次数 >= 5
    ↓                      ↓
正常启动              安全模式启动
                         ↓
                    只启动最小功能
                    (以太网 + 基础 Modbus)
                    等待 OTA 恢复
```

### 5. 内存保护

```rust
// PSRAM 分配策略 (已在 sdkconfig 中配置)
CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=4096   // <4KB 走 SRAM
CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=16384 // 保留 16KB SRAM

// 大数组必须走堆分配
// ❌ 不要：
let data = [0u16; 1500];  // 3000 bytes on stack

// ✅ 要：
let data = vec![0u16; 1500];  // 堆分配
```

### 6. 生产部署检查清单

- [ ] 所有 `.spawn()` 有 `.stack_size(8192)` 
- [ ] 无 `.unwrap()` / `.expect()` 在生产路径中
- [ ] health::check_all() 在主循环中调用
- [ ] 复位计数持久化 + 连续复位检测
- [ ] Panic hook 记录 backtrace
- [ ] Core dump 分区已配置
- [ ] NVS A/B 双 blob 保护
- [ ] 看门狗已启用 (CONFIG_ESP_TASK_WDT)
- [ ] Modbus 异常超时处理
- [ ] BLE 断线自动重广播

### 7. TODO 优先级

| 优先级 | 事项 | 影响 |
|-------|------|------|
| P0 | 所有线程加 stack_size(8192) | 防止线程栈溢出崩溃 |
| P0 | 去掉 unwrap/expect | 防止 NVS/Mutex panic |
| P1 | 配置 core dump 分区 | 崩溃时可分析 |
| P1 | 连续复位检测 + 安全模式 | 避免启动循环 |
| P2 | 内存压力测试 | 验证 7×24 稳定性 |
| P2 | 网络断线自动恢复 | 工业现场可靠性 |
