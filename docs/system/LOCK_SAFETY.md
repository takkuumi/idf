# 并发与锁安全分析 (Zero-parking_lot 之后)

> 本文档跟踪 2026-07-18 完成的无锁 (zero-parking_lot) 重构后的并发模型.
> 历史版本 (parking_lot::Mutex 时代) 的描述已废弃；变更摘要见
> [`../archive/reports/changes-summary-history.md`](../archive/reports/changes-summary-history.md)。

## 当前同步原语

全部自实现于 [`src/sync.rs`](../../src/sync.rs), **不依赖 `parking_lot`**.

| 原语 | 替代 | 适用 | 中毒? | park OS? |
|------|------|------|-------|---------|
| [`Spin<T>`](../../src/sync.rs) | `parking_lot::Mutex<T>` / `std::sync::Mutex<T>` | 短临界区 (微秒级: 外设句柄 deref mut, NVS, 大对象借片段) | ❌ | ❌ (CAS 自旋) |
| [`AtomicBits64`](../../src/sync.rs) | `Mutex<u64>` (xtensa 无原生 `AtomicU64`) | 64 位 IO 位图 (DI/DO) | ❌ | ❌ (wait-free 读, CAS 单位写) |
| [`MpscRing<T,N>`](../../src/sync.rs) | `std::sync::Mutex<heapless::spsc::Queue>` | Actor mailbox / 事件总线 (MPSC) | ❌ | ❌ (Spin 串行 enqueue, 不 park) |
| [`crate::actor::{Actor,ActorRef,spawn}`](../../src/actor/mod.rs) | 共享可变状态 + Mutex | 单线程独占的可变状态 (NVS/commit/reload 流程) | ❌ | ❌ (状态由 Actor 线程独占) |
| `Rcu<T>` ([`src/bus/rcu.rs`](../../src/bus/rcu.rs)) | `RwLock` / 大 `Mutex<Struct>` | 11KB / 1KB 读多写少对象 | n/a | ❌ (读 1 load + 1 fetch_add; **epoch 回收无 leak**) |

`Spin` 提供与 `parking_lot::Mutex` 一致的 `.lock()` / `.try_lock()` API + RAII `SpinGuard`,
迁移点仅是类型与初始化器的替换 (`.map(|g| ...)` / `if let Ok(g) = ...lock()` 模式需要适配为新 guard).

## 全部锁位点 (重构后)

| 变量 | 类型 | 位置 | 类别 | 备注 |
|------|------|------|------|------|
| `BUS` (legacy) | `Spin<Bus>` | [`src/bus/mod.rs`](../../src/bus/mod.rs) | 短临界区 | `lock_timeout()` = 1024 轮 try_lock 自旋, 不 park |
| `NVS` | `Spin<Option<EspDefaultNvs>>` | [`src/device/mod.rs`](../../src/device/mod.rs) | 短临界区 | `nvs_lock()` 返回 `SpinGuard` |
| `REGISTRY` | `AtomicPtr<TaskHb> ×16 + AtomicUsize` | [`src/health.rs`](../../src/health.rs) | **真无锁** | 单写者 register, 多读者 check_all |
| `SESSION` (OTA) | `Spin<Option<OtaSession>>` + `AtomicU32 PENDING_TOTAL` | [`src/ota/mod.rs`](../../src/ota/mod.rs) | 准无锁 | pending 已 vacated, session 短临界区 |
| `HANDLE_*` ×3 | `Mutex<[u16;4]>` / `Mutex<Option<_>>` → **`Spin<T>`** | [`src/ble_at/mod.rs`](../../src/ble_at/mod.rs) | 短临界区 | Bluedroid 回调上下文, `try_lock` 不阻塞 |
| `RX_BUFFER` / `TX_BUFFER` / `BINARY_TX` | `Spin<heapless::String/Vec>` | [`src/ble_at/mod.rs`](../../src/ble_at/mod.rs) | 短临界区 | BLE 回调 `try_lock` 全非阻塞 |
| `RING_LOG` | `Spin<RingLog>` | [`src/error/ringlog.rs`](../../src/error/ringlog.rs) | 短临界区 | 写极短, 读 Modbus 命令 |
| `POOL` | `Spin<[_;4]> + Spin<[bool;4]>` | [`src/bus/buffer_pool.rs`](../../src/bus/buffer_pool.rs) | 短临界区 | acquire/release/release 原子 |
| `started_at` (ProtocolState) | `Spin<Option<Instant>>` | [`src/protocol/mod.rs`](../../src/protocol/mod.rs) | 短临界区 | mark_started + uptime_s |
| GPIO `eth_rst`/`rs485_de`/`run_led` | `Spin<PinDriver<Output>>` | [`src/hal/gpio.rs`](../../src/hal/gpio.rs) | 短临界区 | `set_level` 需 `&mut self` |
| LEDC `channels[4]` | `Spin<LedcDriver>` | [`src/hal/ledc.rs`](../../src/hal/ledc.rs) | 短临界区 | `set_duty` 需 `&mut self` |
| ADC oneshot `driver` | `Spin<AdcDriver>` | [`src/hal/adc.rs`](../../src/hal/adc.rs) | 短临界区 | fallback 路径 (无 feature `adc-continuous`) |
| I2C `bus` (io_ext) + `do_cache` | `Spin<I2cBus>` + `AtomicBits64` | [`src/hal/io_ext.rs`](../../src/hal/io_ext.rs) | 短临界区 + **真无锁位图** | do_cache 真无锁 |
| I2C `io_bus`/`led_bus` (pca9555) + `do_cache` | `Spin<SwI2c>` + `AtomicU16` | [`src/hal/pca9555.rs`](../../src/hal/pca9555.rs) | 短临界区 + 真无锁 | do_cache CAS 单位写 |
| `IO.di` / `IO.do_` bits | `AtomicBits64` | [`src/bus/io_state.rs`](../../src/bus/io_state.rs) | **真无锁** | xtensa 4× AtomicU16 + seqlock |
| `IO.ai/ao/sys` | `AtomicU16/U32/U8` 原生原子 | [`src/bus/io_state.rs`](../../src/bus/io_state.rs) | **真无锁** | 6 ch raw/scaled + 4 ch AO + sys |
| `CONFIG` / `STORAGE` | `Rcu<Snapshot>` | [`src/bus/config_state.rs`](../../src/bus/config_state.rs) / [`src/bus/storage_state.rs`](../../src/bus/storage_state.rs) | **真无锁** (读; 1 load+1 fetch_add) | 写 epoch 延迟回收 (无 leak) |
| `IO_EVENTS` | `MpscRing<IoEvent,32>` | [`src/bus/event_bus.rs`](../../src/bus/event_bus.rs) | **非阻塞** | 满 drop_oldest, 不 park |
| `DeviceActor` mailbox | `MpscRing<DeviceCmd,32>` | [`src/device/mod.rs`](../../src/device/mod.rs) | Actor 独占 | NVS/commit/reload 状态由 Actor 线程独占 |

`parking_lot` 依赖已从 [`Cargo.toml`](../../Cargo.toml) 中移除, `rg "parking_lot" src/` 除注释外无残留.

## 死锁分析 (重构后)

锁层级保持不变, 但争用大幅下降:

```
高频 IO (AtomicBits64/原子)     ← 真无锁, 不参与层级
  ↓ (无交互)
BUS (Spin legacy)               ← 最高层级
  ↓
NVS (Spin)                       ← 中间层级 (主线程 + DeviceActor 串行)
  ↓
BLE HANDLE/GATTS_IF/CONN_ID      ← 最低层级 (Bluedroid 回调内)
```

所有 `Spin` 实现非阻塞 + 无中毒: park 不会发生, 持锁线程即使 panic 也会
(panic=abort in release) 整机 reset, 不存在持有线程死锁导致其他线程永久阻塞的场景.

| 代码路径 | 获取顺序 | 风险 |
|---------|---------|------|
| `device::commit()` (DeviceActor 内) | BUS → NVS → BUS | ✅ 安全 (NVS 释放后重新获取 BUS) |
| `device::reload()` (DeviceActor 内) | NVS → BUS | ✅ 安全 (先释放 NVS) |
| `apply_config()` | BUS → NVS | ✅ 安全 |
| Modbus 写 → `request_apply_config()` | mailbox send (不持锁) | ✅ 安全 (异步) |
| `BLE WRITE_EVT` → `handle_modbus_rtu` | HANDLE_TABLE(已释放) → BUS | ✅ 安全 |
| DI/DO/AI 采样更新 | 原子数组 (无 BUS 锁) | ✅ 安全 (无锁) |
| `io_ext.write_do` | do_cache CAS (无锁) + I2C Spin | ✅ 安全 |
| `pca9555.write_do_all` | do_cache CAS (无锁) + I2C Spin | ✅ 安全 |

**当前代码中不存在 ABBA 死锁路径.** `Spin` 的非阻塞语义还消除了原来 `parking_lot`
                在争用时 park 到 OS 而引入的微秒→毫秒级调度器介入.

## 剩余风险与缓解

### 1. Spin 自旋在极高争用下浪费 CPU

`Spin::lock()` CAS 失败后 `spin_loop()` 等待. 临界区已是微秒级, 实测在 ESP32-S3
双核单写者场景下几乎一次命中; 极高争用时主线程会多消耗若干 μs.

**缓解**: 高频热路径 (DI/DO 64 位位图) 已下行至 `AtomicBits64` (真无锁),
仅 fallback / 偶发访问点仍用 `Spin`.

### 2. `lock_timeout()` 1024 轮自旋 vs 100ms try_lock_for

`bus::lock_timeout()` 用 1024 轮 `try_lock` 替代原 `parking_lot::try_lock_for(100ms)`.
在短临界区下几乎一次即命中 (<< 1μs); 极端争用时仍 max 1024 次自旋后返回 None,
语义上等价于 "尝试拿锁, 拿不到就跳过", 不会 park.

### 3. `Rcu<T>` 写入 epoch 回收 (2026-07-18 重构)

`Rcu::write` 旧 `Box` 历史 leak 设计已**废弃**: 现由 4 个 `AtomicU16` reader-count 槽 +
`AtomicU8 epoch` + 长度-4 retire ring 实现惰性回收. 路径:

1. `write(new)`: 先 sweep (回收旧代次读者计数=0 的 Box) → `epoch.fetch_add(1)` →
   `ptr.swap(new)` → 旧 ptr 入 retire 队列, 带旧 epoch.
2. `read() -> RcuReader`: `fetch_add` 在当前 epoch 槽上 +1, load ptr, 返回 RAII guard.
   guard drop 时 `fetch_sub` → 让下次 sweep 可回收.
3. **安全条件**: sweep 只释放 `current_epoch != retired_epoch && reader_counts[retired_idx]==0`
   的 retire 槽.

单一边界: 若写者突发写入超过 retire 容量 (4), 退化为 best-effort leak 1 个快照
   (工业写极少场景不会触发; 模式 `test_rcu_no_leak_under_writer_burst` 验证).

### 4. Actor mailbox 满即丢弃

`ActorRef::send` 满时记 warn 日志 + 丢弃消息 (而非阻塞). `DeviceActor` mailbox 容量 32,
                DeviceCmd 在 Modbus/AT 高频请求下也远低于此. 如果未来出现持续丢弃, 应优先
                扩容 mailbox (改 const N) 或上游限流, 而非引入阻塞.

## 编译验证

```
cargo check                          # 0 errors (27 warnings, 已有未用 import)
cargo check --features f3            # 0 errors
cargo check --features f4            # 0 errors
```

`parking_lot` 已不在 `Cargo.toml` 中, `Cargo.lock` 自动移除其解析项.
