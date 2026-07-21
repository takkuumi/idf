# 系统架构方案 (2026-07-22)

## 当前问题 (架构层面)

### 资源限制
- **硬件**: ESP32-S3R2, **288KB SRAM**, **2MB PSRAM**
- **当前用户 pthread 任务**: 11 个
- **esp-idf 系统任务**: BLE controller, ETH driver, Wi-Fi (启用), main_task, sys_evt, idle 等
- **pthread 默认栈**: 3KB × 11 任务 = 33KB 用户栈
- **可用 internal heap**: ~200KB

### 栈溢出根本原因

不是单个任务栈太小，而是**架构问题**:

1. **过度线程化**: 11 个独立 pthread 任务
   - 每个任务独立栈，**栈无法共享**
   - 任务间同步用 spin lock / RCU clone
   - 复杂的状态机分布在多个线程中

2. **栈使用不可预测**:
   - Modbus 处理用 `Vec::with_capacity()` 在 pthread 中分配 heap
   - 大量临时 Vec 增加栈帧深度
   - 调用链 30+ 层（backtrace 显示）

3. **heap 碎片化**:
   - pthread 任务创建/删除 + Vec 频繁分配
   - 最终导致 `ENOMEM` 和 `Stack canary watchpoint`

## 新架构: 单主循环 + 关键独立任务

### 设计原则
1. **一切能合并的都合并到 main_loop**
2. **只有需要严格时序的才用独立任务**
3. **零栈分配**: 用 `heapless::Vec` 或栈数组代替 `Vec`

### 任务划分

| 任务 | 当前 | 新架构 | 理由 |
|------|------|--------|------|
| wifi-heartbeat | 独立 pthread | **取消** (或合入 main) | Wi-Fi 内部有健康检查 |
| eth-heartbeat | 独立 pthread | **取消** (合入 main) | ETH 内部有 link 状态 |
| di-scan | 独立 pthread 5ms | **合入 main_loop** (20ms 分频) | 5ms 对光耦过频, 20ms 足够 |
| do-output | 独立 pthread 1ms | **合入 main_loop** (事件驱动 + 100ms poll) | notify() 机制保留 |
| ai-sample | 独立 pthread 100ms | **合入 main_loop** (100ms tick) | 周期与 main 同 |
| ao-output | 独立 pthread 100ms | **合入 main_loop** (100ms tick) | 周期与 main 同 |
| mb-rtu-master | 独立 pthread | **保留** | Modbus RTU 严格时序要求 |
| mb-rtu-slave | 独立 pthread | **保留** | 监听外部请求 |
| mb-tcp-listen | 独立 pthread | **保留** | accept 阻塞 |
| DeviceActor | 独立 pthread 32KB | **保留** (16KB) | 序列化 11KB snapshot 需要 |
| BLE | esp-idf 内部 | **保留** | BLE controller |
| ETH | esp-idf 内部 | **保留** | esp-eth 驱动 |

### 新任务列表 (4 个用户任务)
1. **main_loop** (main_task): 100ms 周期, 处理所有业务逻辑
2. **mb-rtu-master**: Modbus RTU 主站轮询
3. **mb-rtu-slave**: Modbus RTU 从站监听
4. **mb-tcp-{port}**: Modbus TCP 监听
5. **DeviceActor**: 设备状态管理 (16KB 栈)

### main_loop tick 分频

```rust
// main_loop 100ms 主循环
const MAIN_LOOP_PERIOD_MS: u64 = 100;

loop {
    // 每个 100ms 都做
    feed_wdt();
    process_ble_tick();
    process_events();
    
    // 分频执行
    if tick % 1 == 0 { /* 100ms: AI/AO 采样 */ }
    if tick % 5 == 0 { /* 500ms: DI 扫描 */ }
    if tick % 50 == 0 { /* 5s: ETH/Wi-Fi heartbeat */ }
    
    if tick % 10 == 0 { /* 1s: 健康检查, uptime 更新 */ }
}
```

### modbus/shared.rs 改造

将 `Vec<u8>` 改为 `heapless::Vec<u8, N>` 或栈数组 `[u8; N]`:

```rust
// Before:
let mut v = Vec::with_capacity(count as usize);
for i in 0..count { v.push(...); }

// After:
let mut v: heapless::Vec<u16, 128> = heapless::Vec::new();
for i in 0..count { let _ = v.push(...); }
```

## 改造收益

| 指标 | 当前 | 改造后 | 收益 |
|------|------|--------|------|
| 用户 pthread 任务 | 11 | 5 | -55% |
| pthread 栈总量 | ~88KB | ~40KB | -55% |
| 内部 SRAM 占用 | 高 (碎片化) | 低 (稳定) | 减少 OOM |
| 栈溢出风险 | 高 | 极低 | 7×24 稳定 |
| 代码复杂度 | 多线程同步 | 单线程 + tick | 易调试 |

## 实施步骤

### Phase 1: 立即修复 (sdkconfig + stack 调整)
1. sdkconfig PTHREAD_TASK_STACK_SIZE_DEFAULT = 6144
2. 移除用户任务自定义 stack_size (让 esp-idf 用 6KB 默认)
3. ✅ 已完成

### Phase 2: 合并 IO 任务到 main_loop
1. di-scan → main_loop 500ms 分频
2. do-output → main_loop 事件驱动 + 100ms poll
3. ai-sample → main_loop 100ms tick
4. ao-output → main_loop 100ms tick
5. eth-heartbeat → main_loop 5s tick

### Phase 3: Modbus shared 重构
1. handle_pdu 返回 heapless::Vec<u8, 256>
2. read_regs_pdu 用 stack 数组

### Phase 4: 验证
1. 7×24 长稳测试
2. 内存监控 (ESP32 heap_walk)
3. CPU 监控 (esp_timer)

## 不做什么

- ❌ 不修改 esp-idf 源码
- ❌ 不修改 MCA / metuory 参考代码
- ❌ 不为单个任务分配超大栈 (治标不治本)
- ❌ 不引入更多 pthread (会加重问题)
