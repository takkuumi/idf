# 锁安全分析

## 当前使用的锁类型

| 锁变量 | 类型 | 位置 | 是否可中毒 | 超时 |
|--------|------|------|-----------|------|
| `BUS` | `parking_lot::Mutex<Bus>` | bus.rs | ❌ 不可中毒 | 100ms `try_lock_for` |
| `REGISTRY` | `parking_lot::Mutex<TaskRegistry>` | health.rs | ❌ 不可中毒 | 阻塞 `.lock()` |
| `HANDLE_TABLE` | `parking_lot::Mutex<[u16;4]>` | ble_at/mod.rs | ❌ 不可中毒 | 阻塞 `.lock()` |
| `GATTS_IF` | `parking_lot::Mutex<Option<u8>>` | ble_at/mod.rs | ❌ 不可中毒 | 阻塞 `.lock()` |
| `CONN_ID` | `parking_lot::Mutex<Option<u16>>` | ble_at/mod.rs | ❌ 不可中毒 | 阻塞 `.lock()` |
| `RX_BUFFER` | `parking_lot::Mutex<String<512>>` | ble_at/mod.rs | ❌ 不可中毒 | 阻塞 `.lock()` |
| `TX_BUFFER` | `parking_lot::Mutex<String<512>>` | ble_at/mod.rs | ❌ 不可中毒 | 阻塞 `.lock()` |
| `BINARY_TX` | `parking_lot::Mutex<Vec<u8,512>>` | ble_at/mod.rs | ❌ 不可中毒 | **`try_lock()` 不阻塞** |
| `NVS` | `parking_lot::Mutex<EspDefaultNvs>` | device/mod.rs | ❌ 不可中毒 | 阻塞 `.lock()` |

**所有锁都是 `parking_lot::Mutex`，不存在锁中毒问题。** `std::sync::Mutex` 才可中毒。

## 死锁分析

### 锁获取顺序

```
BUS (全局状态)     ← 最高层级
  ↓
NVS (持久化)       ← 中间层级
  ↓
HANDLE_TABLE / 
GATTS_IF / CONN_ID  ← 最低层级 (BLE 内部)
```

### 验证所有代码路径

| 代码路径 | 获取顺序 | 是否有死锁风险 |
|---------|---------|--------------|
| `device::apply_config()` | BUS → NVS | ✅ 安全 (先释放 BUS 再获取 NVS) |
| `device::commit()` | BUS → NVS → BUS | ✅ 安全 (NVS 释放后重新获取 BUS) |
| `device::reload()` | NVS → BUS | ✅ 安全 (先释放 NVS 再获取 BUS) |
| `modbus::write_hold_reg()` → `cfg.write_reg()` → `request_apply_config()` | BUS → ... → NVS | ✅ 安全 (异步请求，不持有锁) |
| `BLE WRITE_EVT` → `handle_modbus_rtu` → `bus::lock_timeout()` | HANDLE_TABLE(已释放) → BUS | ✅ 安全 (HANDLE_TABLE 已释放) |
| `DI 扫描` → `bus::lock_timeout()` → 更新 DI | BUS(仅此一个) | ✅ 安全 |
| `DO 输出` → `bus::lock_timeout()` → 读 DO | BUS(仅此一个) | ✅ 安全 |
| `AI 采样` → `bus::lock_timeout()` → 写 AI | BUS(仅此一个) | ✅ 安全 |

**当前代码中不存在 ABBA 死锁路径。**

## 剩余风险

### 1. BUS 锁争用 (高负载下)

BUS 锁在 10 个以上线程中被使用，每个线程有 100ms 超时：

```
DI 扫描 (5ms周期)   ──→ BUS (20次/秒)
DO 输出 (1ms周期)   ──→ BUS (100次/秒)
AI 采样 (100ms周期)  ──→ BUS (10次/秒)
AO 输出 (100ms周期)  ──→ BUS (10次/秒)
Main Loop (100ms)   ──→ BUS (10次/秒)
Modbus 请求          ──→ BUS (可变)
BLE 请求             ──→ BUS (可变)
```

高并发下，BUS 锁的 100ms 超时可能导致 `lock_timeout()` 频繁返回 `None`。

**缓解措施**:
- 所有 BUS 操作已使用 `lock_timeout()` (非阻塞)
- 超时返回 None 时，操作被跳过而不是阻塞
- 关键路径（Modbus 响应）短暂持有锁

### 2. BINARY_TX 数据丢失

`try_lock()` 在锁被占用时不等待，直接跳过：

```rust
// ble_at/mod.rs
if let Some(mut btx) = BINARY_TX.try_lock() {
    // 发送数据...
}
```

**风险**: 高负载下 BLE 响应可能丢失。

**缓解措施**:
- BINARY_TX 只在 `process_tick` (main loop) 中被消费
- main loop 每 100ms 运行一次
- 大部分情况下锁可用

### 3. NVS 锁阻塞

NVS 使用 `parking_lot::Mutex::lock()` (阻塞，无超时)：

```rust
let nvs = NVS.lock();  // 可能永久阻塞
```

**风险**: 如果另一个线程持有 NVS 锁且被挂起（如 I/O 阻塞），获取锁的线程也会永久阻塞。

**缓解措施**:
- NVS 操作仅在 `watch_loop` 线程和 `apply_config` 中被调用
- 操作本身很快（<50ms）
- watch_loop 使用 `try_lock()` 获取 NVS（不是直接 `lock()`）

## 生产环境锁安全建议

### 1. 严格锁层级 (确保现有设计)

```rust
// 层级 0: 不使用锁的原子操作
let hb = HB_SEQ.fetch_add(1, Ordering::Relaxed);

// 层级 1: BLE 内部锁
let handles = HANDLE_TABLE.lock();
// ... 快速操作 ...
drop(handles);  // 尽快释放

// 层级 2: NVS 锁 (持久化)
let nvs = NVS.lock();
// ... 操作 ...
drop(nvs);

// 层级 3: BUS 锁 (全局状态)
if let Some(mut bus) = bus::lock_timeout() {
    // ... 操作 ...
}  // 自动释放
```

**规则**: 绝不持有一个层级锁的同时获取更高层级锁。

### 2. BUS 锁超时监控

```rust
// 记录 BUS 锁取锁时间戳，用于监控
pub fn lock_timeout() -> Option<parking_lot::MutexGuard<'static, Bus>> {
    let start = std::time::Instant::now();
    let guard = BUS.try_lock_for(Duration::from_millis(100));
    let elapsed = start.elapsed();
    if elapsed > Duration::from_millis(50) {
        log::warn!("[bus] lock contention: {}ms", elapsed.as_millis());
    }
    guard
}
```

### 3. try_lock 路径加日志

```rust
// BINARY_TX 丢数据时告警
if let Some(mut btx) = BINARY_TX.try_lock() {
    // ... 发送 ...
} else {
    log::warn!("[ble_at] BINARY_TX lock contended, data may be lost");
}
```

### 4. 避免长时间持有锁

```rust
// ❌ 不要：持有锁时做耗时操作
let mut bus = bus::lock_timeout().unwrap();
// ... NVS 写入（慢）...
// ... CRC 计算（慢）...

// ✅ 要：数据拷贝出来后释放锁
let data = {
    let bus = bus::lock_timeout().unwrap();
    bus.some_data.clone()  // 快速拷贝
};  // 释放锁
// ... 耗时操作 ...
```

### 5. 使用读锁代替写锁 (如果合适)

如果 BUS 的数据可以被分解为读/写两部分，可以用 `RwLock` 替代 `Mutex`：

```rust
// 当前：所有操作都互斥
pub static BUS: Lazy<Mutex<Bus>> = ...;

// 改进：读操作不互斥
pub static BUS: Lazy<RwLock<Bus>> = ...;

// 读（多个线程可以同时读）
let bus = BUS.read();
let di = bus.di.bits;  // 读操作

// 写（独占）
let mut bus = BUS.write();
bus.di.bits = new_value;  // 写操作
```

**注意**：RwLock 在写操作多的情况下可能比 Mutex 更慢，建议仅在读 > 90% 时使用。

## 结论

| 风险项 | 严重程度 | 当前状态 | 建议 |
|--------|---------|---------|------|
| 锁中毒 | **无** | 全部使用 parking_lot::Mutex | 无需处理 |
| ABBA 死锁 | **低** | 所有代码路径遵循相同层级 | 持续代码审查 |
| BUS 锁争用 | **中** | 100ms 超时 + 非阻塞 | 加锁争用日志监控 |
| BINARY_TX 丢数据 | **中** | try_lock 不等待 | 加丢数据告警 |
| NVS 永久阻塞 | **低** | 操作在专用线程中 | 可加 try_lock 替代 lock |
