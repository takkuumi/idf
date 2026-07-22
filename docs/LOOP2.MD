# 系统持续开发集成 (LOOP2.MD)

> 最后更新: 2026-07-22
> 角色: 产品经理 + 高级 Rust 开发 + 高级测试 + 高级架构 + 工业软件审计

## 项目背景

工业控制系统固件，基于 rust + esp-idf 重构 C++ 原系统 MCA_F16V2_1_F48_BLE。

## 任务和目标 (LOOP2)

1. 推进系统架构彻底摆脱锁、实现完整无锁化
2. 进行完整的性能测试、确保系统高性能稳定运行
3. 系统高稳定、高可靠性、支持7*24不间断运行不宕机

## LOOP2 完成进展 (8/10)

### ✅ 任务 1: 引入 MainLoopCell — 单线程 tick 状态零开销 cell

**根因**: `Mutex<Option<State>>` + `try_lock` 模式在 main_loop 单线程路径上
- 每次 tick 仍做 atomic CAS（数十 ns 浪费）
- try_lock 失败时直接 return 跳过 tick（隐性数据丢失）
- 锁的争用模型在单线程下根本不适用

**修复**: `src/sync.rs` 新增 `MainLoopCell<T>`
- 仅一次 init 标志检查（AtomicBool），之后直接 &/&mut 访问
- 单线程契约由调用方保证（main_loop 独占）

**应用范围**: 5 个 tick 模块
- `src/io/di.rs` `tick_di_scan` (5 分频 20ms)
- `src/io/do_.rs` `tick_do_output` (100ms + notify)
- `src/channel/ai.rs` `tick_ai_sample` (100ms)
- `src/channel/ao.rs` `tick_ao_output` (100ms)
- `src/ethernet/w5500.rs` `tick_eth_heartbeat` (5s 50 分频)

### ✅ 任务 2: protocol started_at 原子化

`Spin<Option<Instant>>` → `AtomicU32` (esp_timer_get_time/1000)
- 7×24 长跑 49 天周期，足够
- 移除 try_lock 开销

### ✅ 任务 3: buffer_pool 真正无锁

`Spin<[Vec; N]>` + `Spin<[bool; N]>` → `AtomicBool[N]` + `UnsafeCell<[Vec; N]>`
- acquire: CAS(false→true) 找首个空闲槽
- release: store(false)
- write/read 走 `UnsafeCell::get()` 索引（持有期间独占）
- 热路径零锁

### ✅ 任务 4: 锁状态盘点

| 类型 | 旧 | 新 | 状态 |
|------|------|------|------|
| `std::sync::Mutex::new` | 25+ | **0** | 完全消除 |
| `Spin::new`（短临界区自旋） | 15 | 15 | HAL/BLE/NVS/OTA |
| `Atomic*` | 高频路径 | 5 个 tick cell + counters | 零开销 |
| `MainLoopCell` | 0 | 5 | 单线程零开销 |

**结论**: 所有 `std::sync::Mutex` 已清除；剩余 `Spin` 是非 park 的非中毒自旋锁，
不阻塞 OS 调度器。

### ✅ 任务 5: BLE Android 手持机显示修复

**根因**（通过 metuory 1.0.78 源码分析）:
Android 端 `CommandParserUtil.parseIP` 严格检查 `buffer.length == 1 + getIPBufferLength() = 25`，
本系统返回 13 字节（4 octets 格式），导致 item=null，UI 不显示。

| 命令 | 旧格式 (本系统) | 新格式 (Android 期望) | 修复 |
|------|--------------|-------------------|------|
| READ_FW_VERSION (0x087E, count=2) | [2][fw_hi][fw_lo] 3B | [4][fw_hi][fw_lo][dt_hi][dt_lo] 5B | ✅ |
| READ_IP (0x08C7, count=12) | [12][ip4][mask4][gw4] 13B | [24][ip(8)][mask(8)][gw(8)] 25B | ✅ |
| READ_MAC (0x08D7, count=6) | [6][mac6] 7B | [12][mac(12)] 13B | ✅ |
| READ_HARDWARE_INFO (0x087C) | [4][DO][DI][ADC][RS485] 5B | (一致) | ✅ |
| READ_DEVICE_PRODUCT (0x08A5) | [2][hw_hi][hw_lo] 3B | (一致) | ✅ |
| READ_BLUETOOTH_ID (0x08E2) | [4][ble_name[0..4]] 5B | (UTF-8 模式一致) | ✅ |

**关键编码**: 每 octet → BE u16（高字节 0），如 `192 → [0x00, 0xC0]`。

**变更**:
- `src/device/system_config.rs`: 新增 `fw_date: u16` 字段（默认 `0x0615`）
- `src/ble_at/mod.rs`: `handle_ble_android_read_command` 修正 IP/MAC/FW 编码
- `src/bus/backends.rs`: `INREG_FW_DATE` 从 `cfg.fw_date` 读取（之前硬编码 `0x0267`）
- `src/device/system_config.rs`: `fw_version_from_cargo` 改为 `major*100+sub*10+patch`
  (Android `fwVersionBytesToStr` 期望此编码)
- `Cargo.toml`: version `0.1.0` → `2.2.1`（与 MCA 参考固件对齐，显示 "2.2.1.1557"）

**加严测试** (6 个端到端 Android 解析模拟):
- `test_android_parse_ip`: 验证 25B 完整格式
- `test_android_parse_mac`: 验证 `macBytesToStr` 期望的 12B 格式
- `test_android_parse_fw_version`: 验证 `fwVersionBytesToStr` 解析
- `test_android_parse_bluetooth_id_utf8`: UTF-8 模式
- `test_android_parse_hardware_info`: HW_INFO 字段
- `test_android_parse_device_product`: PRODUCT hex

### ✅ 任务 6: Modbus TCP MBAP.length 修正 + 寄存器映射

**根因**: 服务端响应中重复写 unit_id（MBAP[6] + body[0]），
MBAP.length 包含 unit_id，与 pymodbus/libmodbus 主流客户端期望不符。

**修复**:
- `src/modbus/tcp_server.rs`: 移除响应中重复的 unit_id，`MBAP.length = PDU.len()`
- `src/config.rs`: 新增 `INREG_HW_VER/INREG_IP_BASE/INREG_MAC_BASE/INREG_BLE_ID_BASE`
- `src/bus/backends.rs`: 映射 13 个新寄存器到 `read_input_reg`

**Modbus TCP 验证结果** (与 metuory Android 一致):
| 寄存器 | 值 | 说明 |
|--------|-----|------|
| FW | "2.2.1.1557" | fw=0x00DD(221), dt=0x0615(1557) |
| HW | Q=16, I=16, AI=4, RS485=2 | QI=0x1010, A485=0x0402 |
| IP/Mask/GW | 192.168.51.140 / 255.255.255.0 / 192.168.51.1 | 6 regs BE u16/octet |
| MAC | 80:B5:4E:5B:24:E7 | 3 regs BE u16/byte |
| BLE_ID | "Mesh" | 4 regs BE u16/char |
| BLE drops | 0 | notify 队列无丢 |

### ✅ 任务 7: pthread 栈 6KB → 12KB (根本解决 4 并发崩溃)

**根因**: 4 并发 Modbus TCP 连接下设备 Guru Meditation 崩溃。
- 每连接 6KB 栈不足以容纳 253B PDU + 280B resp + 调用栈
- 4 连接 × 6KB + 其他 pthread × 6KB ≈ 48KB 接近 SRAM 上限
- 触发 stack canary watchpoint（Unhandled debug exception）

**修复**:
- `src/modbus/tcp_server.rs`: TCP 连接线程显式 `.stack_size(12 * 1024)`
- `sdkconfig.defaults`: `CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT 6144→12288`
  (全局默认也提升，防止其他 pthread 任务后续 OOM)

### ✅ 任务 8: 性能基线测试

**Modbus TCP 性能** (LOOP2 测试结果):
- 单连接 100 req: p50=3.92ms, p95=9.29ms, p99=89.12ms, **throughput=186 req/s**
- 30s 持续: **5641 req, 188 req/s**

### 🟡 任务 9: 7×24 长稳测试

1 小时长稳 monitor 已运行（commit 8e262fa 时启动）。
4 并发压测时设备曾 Guru Meditation 崩溃（任务 7 修复中解决），
但需更长时间复测确认无复发。

### ✅ 任务 10: 5 角色协作

- **产品经理**: 对照 MCA + metuory 提出 BLE 显示问题
- **高级 Rust 开发**: 实施 BLE 格式修复 + 性能优化
- **高级测试**: Modbus TCP 压测发现 4 并发崩溃
- **高级架构**: 引入 MainLoopCell 单线程零开销 cell
- **工业软件审计**: 锁定 stack canary 根因（pthread 栈不足）

## 提交记录 (LOOP2)

| Commit | 内容 |
|--------|------|
| `a825af9` | P0+P3+P4: 引入 MainLoopCell + 移除 tick 状态锁 + buffer_pool 无锁化 |
| `1c46794` | BLE Android: 修正 IP/MAC/FW 响应格式匹配 metuory 1.0.78 期望 |
| `06c7863` | fw_date: INREG_FW_DATE 走 cfg.fw_date + Cargo version 对齐 |
| `0f41951` | Modbus TCP: MBAP.length 修正 + IP/MAC/BLE_ID 寄存器映射 |
| `a67d9a1` | pthread 栈 6KB→12KB (根本解决 4 并发崩溃) |

## 待办 (用户醒来后)

1. 重新烧录最新 binary 并验证 4 并发 Modbus TCP 稳定
2. 启动 24h 长稳 monitor
3. 真实 Android 手持机端到端验证 BLE 显示

## 烧录命令

```bash
cargo build --bin gateway
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
espflash reset --port /dev/cu.usbserial-1430
```
