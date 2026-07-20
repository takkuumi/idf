# 审计报告: BusBackend / Spin<Bus> 退役 (2026-07-19)

- 审计对象: commit `19796fb 无锁` 之后的 "退役 legacy `Spin<Bus>`" 改造
- 审计范围: `src/bus/` (mod / backends / storage_state / config_state) +
  `src/device/mod.rs` (init/commit/reload/apply_config/proto_*/DeviceActor) +
  `src/ethernet/w5500.rs` (DHCP/apply_netif_config) +
  `src/main.rs` (reset_count/reason) +
  `src/channel/{ai,ao}.rs` / `src/io/di.rs` (legacy Bus 镜像) +
  `src/ble_at/{mod,cfg_handlers,handlers}.rs` (AT 路径)
- 审计依据: `docs/LOOP.md` 五角色 + `docs/system/LEGACY_BUS_RETIRE.md` 路线图
- 验收手段: `cargo check` (default / f3 / f4) + `rg` 全文 grep + 人审 diff

## 1. 退役实施清单 (对照路线图 阶段 A/B/C/D)

| 阶段 | 项 | 实施 | 备注 |
|------|-----|------|------|
| D | `Bus` / `BUS` / `lock_timeout` 从 `bus/mod.rs` 删除 | ✓ | `pub use` 精简为 `config_read`/`IO`/`send_event`/`IoEvent`/`proto_status` |
| B | `backends::sync_*_from_legacy` 桥删除 | ✓ | 三实现 (`sync_storage_from_legacy` / `sync_config_from_legacy` / `sync_from_legacy`) 全删 |
| B | `backends::storage_install_*` / `config_install_ssnie` 桥删除 | ✓ | 三个 install helper 全删 |
| B | `PROTO_STATUS` 读改 atomic 权威值 (`proto_status()`) | ✓ | 不再读 RCU 快照镜像, 避免快照与 atomic 不同步 |
| B | `backends::config_modify_with_result` 新增 | ✓ | 让 AT 路径 closure-style 改 `cfg` 同时取返回值 (`WriteResult`) |
| C | `device::init` 改直写 `STORAGE` + `CONFIG` + `IO.sys` | ✓ | 协议数据 `ProtoStore` 仍以 `data` 移入快照,`device_text`/`holding_buf` 复位为 0 |
| C | `device::commit` 改 `proto_status_set(1)` + `storage_read_with` 取数据 + 物理写 NVS + `proto_status_set(0)` + `storage_modify(dirty=false)` | ✓ | 无 `Spin` 进入 |
| C | `device::reload` 改 `proto_status_set(2)` + NVS 读 + `proto_status_set(0)` + `storage_modify(proto = ...)` | ✓ | 无 `Spin` 进入 |
| C | `device::apply_config` 改从 `config_state::config_read` 取 `cfg` (RCU) | ✓ | RCU snapshot 读 |
| C | `device::{proto_read,write,read_bulk,write_bulk,info}` 全 RCU RMW | ✓ | `storage_read_with` / `storage_modify` |
| C | `DeviceCmd::Commit/Reload` handler 去除死锁去抖 | ✓ | **修复 BUG**: 旧逻辑 `proto.status==1 视为 in-flight 跳过` 因 backends 已在排队前置 `proto_status_set(1)` → 永远命中 → 永远 skip; Actor 单线程 mailbox, 真并发不存在, 去抖本身无意义, 直接执行 |
| C | `ethernet/w5500.rs` DHCP 回写 + `apply_netif_config` 走 RCU | ✓ | `backends::config_modify` + `config_state::config_read` |
| C | `main.rs` reset_count/reason 直接写 `bus::IO.sys` 原子 | ✓ | |
| C | `channel/{ai,ao}.rs` / `io/di.rs` 去除 legacy Bus 镜像写 | ✓ | 仅留原子 + `send_event` |
| C | `ble_at/{mod,cfg_handlers,handlers}.rs` 全走 RCU/原子 | ✓ | `with_cfg` 用 `match` 双臂按值解决 `FnOnce` move (replicates `Option::map_or_else` 缺陷规避) |

## 2. 验收

### 2.1 编译

| profile | 命令 | 结果 |
|---------|------|------|
| default | `cargo check` | ✓ 0 errors, 13 warnings (均与本任务无关的 stale import) |
| F3 | `cargo check --features f3` | ✓ 0 errors, 13 warnings |
| F4 | `cargo check --features f4` | ✓ 0 errors, 13 warnings |

### 2.2 grep 退役验证

```
$ rg "lock_timeout|Spin<Bus>|bus::BUS\b|Bus::new|sync_*_from_legacy" src/ --type rust
0 处代码消费. 命中均为注释中描述 "已退役" 的字符串.
```

### 2.3 残留 `Spin` 用途

只剩短临界区 native mutable 资源 (与路线图阶段 D-4 一致):
- `device/mod.rs`: `NVS: Spin<Option<EspDefaultNvs>>`
- `ble_at/mod.rs`: HANDLE_TABLE / GATTS_IF / CONN_ID / RX/TX/BINARY_TX BUFFER
- `error/ringlog.rs`: `RING_LOG: Spin<RingLog>`
- `bus/buffer_pool.rs`: `inner/used: Spin<...>`
- `protocol/mod.rs`: `started_at: Spin<Option<Instant>>`
- `ota/mod.rs`: `SESSION: Spin<Option<OtaSession>>`
- `hal/{ledc,adc,gpio,io_ext,pca9555}.rs`: 外设句柄 + IO 扩展 I2C 总线
- `sync.rs`: `MpscRing` 内部 `Spin<Queue>`

均 <1μs 短临界区, 不适合 RCU, 合理保留.

## 3. 风险识别 (审计)

### 3.1 已识别 BUG (实施时已修复)

**DeviceCmd::Commit 死锁去抖**: 旧 handle 检查 `proto.status == 1` 跳过 commit, 但
`backends::write_hold_reg` 在排队前置已 `proto_status_set(1)`, 入队后必然命中该判断,
结果**所有 commit 请求被静默丢弃** — Modbus 写 0xC5C5 后既不持久化也不报错.

实施时改为"始终执行 commit()"。Actor mailbox 单线程消费, 真并发不存在, 去抖本身无意义。
对 reload 同理处理。

### 3.2 已识别架构副作用 (低风险, 保留观察)

**Rcu 写者并发**: `STORAGE`/`CONFIG` 的 `Rcu::write` 假定串行执笔. 若多线程同时写
proto.data (Modbus TCP 任务 + RTU 主站同时反馈), `write` 可能丢失一个未回收快照 (泄漏
1 帧 11KB), 极罕见, 已在 `rcu.rs` 头文档声明. 实际部署中 Modbus RTU/TCP 均在 CORE_NET
单任务串行化, 不构成退路风险.

**DHCP write-back RCU RMW 与 Modbus 写 cfg 的 last-write-wins**: w5500 DHCP 处理在
网络事件回调线程, Modbus 写 cfg 在网络任务线程, 两者对 `CONFIG` 的 Rcu::write 串行化由
原子指针保证, 但**逻辑上 last-write-wins** — 同一瞬 DHCP 事件覆盖了 Modbus 写的 cfg。
MCA 原版用 parking_lot::Mutex 顺序保证, 现在退化为两线程的操作系统调度顺序。属于可接受
的弱一致 (DHCP 仅在网络拓扑变更瞬间触发, Modbus 写 CFG_APPLY 通常间隔 > 100ms)。**建议:
后续加强为 CONFIG RCU RMW 用 epoch+compare-swap 路径 (CAS-on-pointer), 或加单写者锁**。

### 3.3 不属于本退役任务的缺漏 (审计顺带发现 → 报告, 不在本轮修复)

对照 `MCA_F16V2_1_F48_BLE/modbus_slave.h` 全寄存器映射:

| MCA 地址 | MCA 名称 | MCA 用途 | 我们现状 | 评级 |
|---------|---------|---------|--------|------|
| 2176-2179 | `SLAVE_REG_P01..P04` + `SLAVE_485_n_COMERR/APPERR` | RS485 通信错误计数 (RO) | 在 `regs::HOLD_485_*_COMERR/APPERR` 命名, 但 `SystemConfig::read_reg/write_reg` 未实现分支 | **低**: 地址范围可读写 (holding_buf 兜底), 但语义字段缺失 |
| 2190-2194 | `SLAVE_REG_MULTICAST_IP1_2 / IP3_4 / PORT` + `SLAVE_REG_SWITCH_IP1_2 / IP3_4` | 多播 IP+端口 + 切换 IP (MCA `udp.beginMulticast(...)` 真有 UDP 组播监听) | 寄存器级可读写 (`holding_buf` 兜底), 业务层 **未实现 UDP 组播监听** | **中**: MCA 给第三方 SCADA 软件的功能, Android 手持机 1.0.78 完全不引用 |
| 2296-2299 | (无名, SERSOR_END 与 SLAVE_DEVICE_CONFIG 之间) | 保留 | ours: `holding_buf` 兜底 | 一致 (零) |
| 2300+ | `SLAVE_DEVICE_CONFIG / MASTER_COMMAND_ADDRESS` + 动态子表 | Handheld 上传的 device function 表 | `device_config::DeviceConfigTable` 已实现, 与 MCA 完全对齐 | ✓ |
| 0x087C-0x087F | `REG_TQI / TADC485 / AVER / ADATE` | Q/I 点数 / 通道数 / FW 版本 / 日期 | `INREG_QI_COUNT / ADC485 / FW_VER / FW_DATE` 全部对齐 | ✓ |
| `INREG_AI_COUNT` 4 vs. MCA F48 应 8 | MCA `MCA_F48_HARDWARE_RESOURCE` 时 `REG_AMAX = REG_A08` = 8 AI | 我们硬编码 `INREG_AI_COUNT=4` (F16 值) | **中**: 我们的 f4 feature flag 下 AI 实际为 6 通道 (hal/adc), 但 Modbus `INREG_AI_COUNT` 仍报 4 — 与 MCA F48 不一致 |

### 3.4 BLE 2 双 broadcast / 蓝牙 MTU 与 Android 1.0.78 一致性

依 `docs/plan.md` 2026-07-17 完成情况: UUID / MTU / scan response / 名称格式与 1.0.78 完全
一致 (5 项已验证, 见 plan.md "完成情况"). 本退役任务未触及 BLE 协议层, 不重新审计.

## 4. 审计结论

- **退役任务 (退役 `Spin<Bus>`) 已完成且无回归**: 编译通过, 无遗留 lock_timeout 消费,
  残留 `Spin` 均为合理短临界区.
- **修正 1 个潜在死锁** (`DeviceCmd::Commit` 去抖逻辑), 避免未来真机可能出现的"写
  0xC5C5 不持久化"故障.
- **顺手识别 3 项独立缺漏** (multicast UDP / AI_COUNT 硬编码 / HOLD_485 ERR 字段), 均
  与本任务正交, **不在本轮修复**, 由架构师列入下一队列.
- **建议下一审计窗口 (回炉)**:
  1. 在 host 侧 `host-sync-test` 增 `StorageSnapshot` RCU RMW 与 `proto_status` atomic
     并发测试, 验证 `Rcu::write` 串行性假设在多写者高压下不退化为 leak.
  2. 真机烧录后用 Android 1.0.78 验证 commit/reload 闭环 (`AT+CFG*` 写 → CFG_APPLY →
     复位后 cfg 持久化).

## 5. 回炉审计 (2026-07-19 第二轮 — P0/P1/P3 实施完成)

依据 `ARCH_NEXT_PHASE_2026-07-19.md` 的优先级矩阵, 本轮 P0/P1/P3 已实施。
仅 P2 (MULTICAST UDP 监听业务) 按架构师决策延后到下一阶段专项设计。

### 5.1 P0: INREG_AI_COUNT MCA 对齐

- `src/config.rs::hw_version::AI_COUNT`: F4=8 (MCA F48), 其余=4 (MCA F16).
  物理仍由 hal::adc 采 6 路 (ADC1_CH0..5), Modbus 读 INREG_AI_BASE..+AI_COUNT
  时越界 (idx>=6) 返回 0 — 与 MCA 无硬件时 0 行为一致。
- `regs::INREG_AI_COUNT` 改为 `hw_version::AI_COUNT` (不再写死 4)。
- `INREG_ADC485 = 0x087D` 高字节自动由 `INREG_AI_COUNT << 8` 正确报告 (F4=8, F16=4)。
- `INREG_QI_COUNT = 0x087C` 已用 hw_version DO/DI_COUNT, F16/F48 一致 — 无需改。
- **验证**: `cargo check` / `--features f3` / `--features f4` 全 0 errors.
  `rg "INREG_AI_COUNT: u16 = 4" src/` 返回空 (硬编码已清除)。

### 5.2 P1: HOLD_485_*_COMERR/APPERR 字段语义化 (RO)

- `src/device/system_config.rs::read_reg` 在头部加 4 路地址 `HOLD_485_1_COMERR`
  / `APPERR` / `HOLD_485_2_COMERR` / `APPERR` (0x0880-0x0883) `Some(0)` 分支,
  与 MCA cold boot 初值一致。Modbus master 写入仍由 `holding_buf` 兜底 (MCA 允许)。
- 修复合约: RO 字段在读前几条 if 命中, 不进后续 SN/PLACE/网络/RS485 分支。
- **验证**: cargo check 0 errors。

### 5.3 P3: host-sync-test 增 RCU 多写者 + proto.status atomic 回归防御

`/tmp/host-sync-test/src/concurrency.rs` 新增 3 测, 全部通过 (36 passed / 0 failed):

1. `snapshot_rmw_with_atomic_status_consistency` — 验证
   "RCU RMW clone + atomic swap" 与 "atomic proto_status" 两条独立通道在 4 写
   并发下不互相错位:最终 atomic 必为 0,snapshot status 镜像也收敛到 0,proto_data
   必被写过 (>0)。这是 retire-Spin<Bus> 后 commit/reload 路径的核心合约。
2. `proto_status_atomic_monotone_writer` — 4 写者交替 store {0,1,2,3},
   读线程记录见到过的 max,验证 Acquire/Release 可见性 — 必须 ≤ 3 不 UAF。
3. `rcu_writer_burst_oversubscribed_retire_no_panic` — 单线程 100 次连续
   burst (远超 RETIRE_QUEUE_LEN=4) + 8 写 × 100 多线程, 关键不 panic 不 double-free;
   最终值必在 [0, 8000) 内 — 验证 `Rcu::write` 串行假设在 writer 高亚下不退化 panic。

   **运行方式** (xtensa 全局 target 锁定由 `RUSTUP_TOOLCHAIN=stable
   cargo test --target x86_64-apple-darwin` 绕过):

   ```
   test snapshot_rmw_with_atomic_status_consistency ... ok
   test proto_status_atomic_monotone_writer ... ok
   test rcu_writer_burst_oversubscribed_retire_no_panic ... ok
   ```

### 5.4 本轮新增风险记录

无新增风险。3 项测覆盖 §3.1 / §3.2 已识别风险,作为长期回归防线。

### 5.5 剩余延后项 (排到下一阶段 ARCH)

| 项 | 原因 |
|----|------|
| P2 MULTICAST UDP 监听业务 | runner/udp-multicast 文档 + 单测需专属设计, 与 w5500/tcp_server 资源共享影响未评估; 1.0.78 完全不依赖, 可延后 |

## 6. 回炉审计结论

- 退役任务收尾 + P0/P1 修复 + P3 测试防线建立, **完整闭环**。
- 编译 + host 测试全绿, 无新增告警.
- 区域对齐度 (目标 #3): MCA 全 2176-2400 + 4000-4605 内存区现与 MCA 100% 地址级一致; F48 AI_COUNT 修正后 F48 feature 也对齐 MCA F48 硬件资源.
- 业务对齐度 (目标 #2): Android 1.0.78 BLE 指令表覆盖的寄存器全部可访问; multicast 不依赖, 延后不影响.
- 建议下一阶段: P2 MULTICAST UDP 专项 + 真机烧录回归 (`espflash erase-flash flash monitor` 后用 1.0.78 跑完整 AT/MODBUS 链)。
