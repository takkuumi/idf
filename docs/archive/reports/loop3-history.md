# 系统持续开发集成 (LOOP3 ~ LOOP8 综合修复记录)

> 最后更新: 2026-07-24 (LOOP8: BLE 原子化收尾 + 无堆分配 + 全编译 0 warnings)
> 详细进度: `log/SUMMARY_2026-07-24.md`

## 项目背景

此系统是开发一款基于ESP-IDF的工业控制系统。原有一套C++开发的系统（MCA_F16V2_1_F48_BLE），运行不稳定，现基于 rust + esp-idf 重构。

- ESP-IDF 源码: `/Users/takumi/Workspace/esp-idf` (禁止修改)
- 原 C++ 系统: `/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE` (禁止修改)
- 手持机源码: `/Users/takumi/Workspace/metuory-wireless-management-app-1.0.78` (禁止修改)

## 系统迭代 - 5 角色

| 角色 | 职责 |
|------|------|
| 产品经理 | 对照 MCA_F16V2_1_F48_BLE + metuory-wireless-management-app-1.0.78 提出缺失功能 |
| 高级 Rust 开发 | 实施功能与修复 BUG |
| 高级测试 | 测试 + 提出问题 |
| 高级系统架构 | 架构把关 (`docs/architecture.md`) |
| 工业软件审计 | 审计每次实施 |

## LOOP8 修复要点 (2026-07-24)

### 任务1: 完全摆脱锁 / 真无锁化

**编译错误根因**: LOOP7 后 `HANDLE_TABLE/GATTS_IF/CONN_ID` 三个静态已由 `Spin<...>` 改为
`[AtomicU16; 4]`/`AtomicU8`/`AtomicU16` (LOOP8 中途), 但 21 处调用方仍按 `Spin` 模式
调用 `.lock()`/`*CONN_ID.lock()` 等, 导致编译失败.

**修复**:

1. **`src/ble_at/mod.rs` 8 处原子化调点**:
   - `try_send_notify`: `*CONN_ID.lock()` → `CONN_ID.load(Acquire)` + sentinel `0xFFFF` 校验
   - `try_send_notify`: `*GATTS_IF.lock()` → `GATTS_IF.load(Acquire)` + sentinel `0xFF` 校验
   - `try_send_notify`: `HANDLE_TABLE.lock()[idx]` → `HANDLE_TABLE[idx].load(Acquire)`
   - `send_notify`: 同上 3 处
   - `process_tick`: 同上 3 处
   - `gatts_event_cb` (CONNECT_EVT/DISCONNECT_EVT): `*CONN_ID.lock() = Some/None` → `.store()`
   - `gatts_event_cb` (CREAT_ATTR_TAB_EVT): `HANDLE_TABLE.lock()` → `[idx].store()`
   - 移除 `crate::config::modbus::rtu_slave::ADDR as MODBUS_ADDR` (BLE 路径未引用)

2. **`src/error/mod.rs`**: `pub use ringlog::{log_error, log_warn, log_critical, module_id, LogEntry, RING_LOG}` → 精简为 `log_warn, module_id` (消除 4 个未引用 re-export 警告)

3. **`src/device_config/mod.rs`**: `pub use types::{DeviceType, DeviceFunction, DeviceFunctionMeta}` → 精简为 `DeviceType`

4. **`src/ble_at/cfg_handlers.rs`**: 移除 `parse_mac` (AT 路径未引用)

5. **`src/modbus/rtu_slave.rs` + `tcp_server.rs`**: 移除未引用的 `exc` / `ModbusBackend` import

6. **`src/hal/mod.rs`**: `use crate::error::{AppError, AppResult};` → `#[allow(unused_imports)] use crate::error::{AppError, AppResult};` (AppError 仅默认版本 GPIO 路径用到, f3/f4 不引用)

7. **`src/device/mod.rs`**: 移除未引用的 `parse_mac` re-export

### 任务2: 性能测试 / 高性能稳定运行

**Modbus 热路径无堆分配化** (LOOP8 中段):

1. **`src/modbus/shared.rs`**:
   - `ModbusBackend` 读方法从 `Vec<bool>/Vec<u16>` → `heapless::Vec<bool, MAX_BITS_PER_READ>` / `heapless::Vec<u16, MAX_REGS_PER_READ>`
   - `MAX_BITS_PER_READ = 2000` / `MAX_REGS_PER_READ = 125` / `PDU_BUF_SIZE = 256` (Modbus 标准上限)
   - `handle_pdu` 签名从 `(B, u8, &[u8]) -> Vec<u8>` → `(B, u8, &[u8], &mut [u8; PDU_BUF_SIZE]) -> usize`
   - 内部 `read_bits_pdu` / `read_regs_pdu` / `write_*` 全部改用调用方提供的栈缓冲区
   - `PduResult` enum 区分成功 (`Ok(body_len)`) 与异常 (`Err(code)`)
   - 0 堆分配/请求 → 消除 7×24 长期堆碎片化风险

2. **`src/modbus/rtu_slave.rs`**: `build_response` 从 `Vec<u8>` → `heapless::Vec<u8, 256>`, `handle_pdu` 调用改传栈缓冲区

3. **`src/modbus/tcp_server.rs`**: `handle_conn` 内联 `handle_pdu` 调用, 删除辅助 `build_pdu` 函数 (10 行 → 4 行)

4. **`src/ble_at/mod.rs`** (`handle_modbus_rtu`): `handle_pdu` 调用改传栈缓冲区

5. **`src/actor/mod.rs`**: Actor 线程栈 8KB → 32KB (实测 commit/reload 时序列化
   11KB StorageSnapshot + 递归 clone 32×32 DeviceConfigTable 需 >12KB 栈, LOOP5 panic 根因).
   `expect("failed to spawn actor thread")` → `match` + `panic!()` 携带真实错误信息.

### 任务3: 7×24 不间断运行不宕机

1. **`src/device/mod.rs`** `commit()`: `write_result.unwrap()` → `if let Some(result) = write_result { ... }` (避免 NVS 不可用时 None.unwrap() panic, 触发整机 reset)

2. **`src/device/mod.rs`** `load_proto_from_nvs`: 移除 `let mut data = [...]` 的 `mut` (data 仅按位置读, 永远不需要 mut)

3. **`src/ethernet/w5500.rs`**: 移除 `let mut b = [0u8; 4]; core::mem::forget(b);` (无意义 — `b` 未使用, `forget` 对 Copy 类型也是 no-op, 双重死代码)

4. **`src/hal/pca9555.rs`**: 移除 `let di1 = (!raw1) as u64;` (该值从未使用, 是 di1_rev 的早期计算错误残留)

5. **`src/ble_at/mod.rs`**:
   - `let fw = APP_VERSION;` (未使用) → `let _fw = APP_VERSION;`
   - `unit: u8` (未使用参数) → `_unit: u8`
   - `conn_id: u16` (未使用参数) → `_conn_id: u16`
   - `let mut acc: u8 = 0;` 在内层 for 循环顶部声明 (跨次循环覆盖) → 移到内层 for 循环内部 (消除 "value never read" 警告)

6. **`src/rs485/port.rs`**: `const portTICK_PERIOD_MS` (FreeRTOS 标识符) → `#[allow(non_upper_case_globals)] const portTICK_PERIOD_MS`

## 验证

### 编译

| feature | errors | warnings |
|---------|--------|----------|
| default | 0 | 0 |
| f3 | 0 | 0 (含 1 个 #[allow] 抑制) |
| f4 | 0 | 0 |

### 主机侧测试 (`/tmp/host-sync-test`)

| 测试套件 | 通过/总数 |
|---------|-----------|
| `sync` 单元测试 (Spin/AtomicBits64/MpscRing/MainLoopCell) | 14/14 |
| `rcu` 单元测试 (基本/empty/cloned/read_with/并发读 + 写/写突发无泄漏) | 5/5 |
| `write_result_semantics` 集成测试 (RegisterWriteResult/布局/文本/序列) | 35/35 |
| **总计** | **54/54** (0 failures) |

并发验证:
- `test_spin_concurrent`: 4 线程 × 1000 次自增 → 4000 ✓
- `test_bits_concurrent`: 8 线程 × 1000 次 set_bit → 无 panic ✓
- `test_ring_mpmc`: 4 线程 × 50 次 enqueue → 至少部分保留 ✓
- `test_rcu_concurrent_reads_with_swaps`: 4 读线程 × 1000 read + 10 write → 无 UAF ✓
- `test_rcu_no_leak_under_writer_burst`: 4 读写混合线程 × 1000 + 1000 write → 无 UAF/leak ✓

## 架构快照 (LOOP8 收尾后)

```
main_loop (100ms tick)
├── tick_ai_sample(&hal)        # 100ms, MainLoopCell<AiState> 零开销
├── tick_ao_output(&hal)         # 100ms, MainLoopCell<AoState> 零开销
├── tick_di_scan(&hal)           # 20ms (5 分频), AtomicBits64 真无锁
├── tick_do_output(&hal)         # 100ms + notify, AtomicBits64 真无锁
└── tick_eth_heartbeat()         # 5s (50 分频), MainLoopCell<EthHbState> 零开销

5 个保留 pthread 任务:
- DeviceActor (NVS 持久化, 32KB 栈, MpscRing mailbox)
- mb-rtu-master (Modbus RTU 主站)
- mb-rtu-slave (Modbus RTU 从站)
- mb-tcp-listen (Modbus TCP, 4 连接 × 20KB 栈)
```

### 同步原语盘点 (LOOP8 收尾后)

| 原语 | 数量 | 类别 | 适用 |
|------|------|------|------|
| `Spin<T>` | 15 | 短临界区自旋 | HAL 外设 / NVS / 环日志 / 缓冲池 / BLE 句柄 / OTA session |
| `AtomicBits64` | 4 | **真无锁** (seqlock + 4×AtomicU16) | DI/DO 64 位位图 |
| `AtomicU8/U16/U32/Bool` | 30+ | **真无锁** | IO 状态/计数/标志 |
| `MpscRing<T,N>` | 4 | 非阻塞 | Actor mailbox + 事件总线 |
| `MainLoopCell<T>` | 5 | 零开销 (单线程) | main_loop tick 状态 |
| `Rcu<T>` | 2 | **真无锁** (RCU RMW) | CONFIG / STORAGE 快照 |
| `parking_lot::Mutex` | **0** | — | 已退役 |
| `std::sync::Mutex` (生产路径) | **0** | — | 已退役 |

### 7×24 稳定性保证

1. **零堆分配热路径**: Modbus TCP / RTU 响应全栈缓冲 (`[u8; 256]`)
2. **零 unwrap/expect 生产路径**: 仅测试代码使用 (剩 2 个 panic hook, 由分级 recovery 兜底)
3. **零编译警告**: 默认 + f3 + f4 features 全 0 警告
4. **54 主机侧测试全通过**: Spin / AtomicBits64 / MpscRing / Rcu 并发无 UAF
5. **NVS 失败降级**: NVS 不可用时 None 不 panic, 走 graceful skip + warn 日志
6. **actor 栈 32KB**: 序列化 11KB snapshot + 32×32 device_config 树不溢出
7. **4 并发 Modbus TCP**: TCP 连接线程栈 20KB 容纳 config_clone 递归 (LOOP5 修复)

## 烧录

```bash
cargo build --bin gateway
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
espflash reset --port /dev/cu.usbserial-1430
```

## 测试日志

`log/` 目录:
- `log/README.md` - 测试矩阵
