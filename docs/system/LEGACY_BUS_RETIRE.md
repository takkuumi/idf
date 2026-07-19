# Legacy `Bus` 退役蓝图 (逐步 / 阶段化)

> 关联: `#165` 思路第 (3) 项 — "逐步把 legacy Bus 也拆到 Rcu / Atomic*, 彻底退役
> Spin 在大对象路径上的使用". 本文档是阶段化交付的设计蓝图, 不是已完成功能.

## 现状 (2026-07-18 zero-parking_lot 重构后)

`BUS: LazyLock<Spin<Bus>>` (`src/bus/mod.rs`) 是唯一剩下的大对象 `Spin` 用途
(其它 `Spin` 都在微秒级短临界区: BLE 句柄表, NVS, 环日志, 缓冲池等).
`Bus` 结构体内**重复**保存已分别迁到无锁全局的同一份数据:

| `Bus` 字段 | 已无锁的镜像 | 性质 |
|-----------|-----------|------|
| `di` / `do_` | `bus::IO.di` / `.do_` (`AtomicBits64`) | 原子 |
| `ai` / `ao` / `sys` | `bus::IO.ai` / `.ao` / `.sys` (原子) | 原子 |
| `proto` | `bus::STORAGE` `Rcu<StorageSnapshot>` | 1 段写少 |
| `cfg` | `bus::CONFIG` `Rcu<ConfigSnapshot>` | 配置写少 |
| `device_config` | `bus::CONFIG` 内 `ConfigSnapshot.device_config` | 配置写少 |
| `device_text` / `holding_buf` | `bus::STORAGE` `Rcu<StorageSnapshot>` | 11KB 写少 |

`Bus::read_*` / `write_*` 通过 `bus::lock_timeout()` 抢 `Spin` 后访问混合两类:
读 DI/DO/AI/AO/sys = 原子 (走 `&self.di.get_bit(...)`, 已是无锁); 写 proto/holding_buf
是 **实例内部 mutable 修改** (`self.proto.data[idx] = value; self.proto.dirty = true`),
这正是退役 `Spin` 难点: 要改成 RCU read-modify-write (克隆 snap → mutate → write).

## 调用方 (~30 处)

| 文件 | 处数 | 主要操作 |
|------|------|---------|
| `src/modbus/shared.rs` | 8 | read_coils/disc/hold/input, write_coil/reg × 2 (FC=01/02/03/04/05/06/0F/10), **Modbus 热路径** |
| `src/modbus/rtu_master.rs` | 1 | DO 位写入 |
| `src/device/mod.rs` | 10+ | init 加载 proto + cfg; commit/reload/apply_config 持锁更新 proto.status; apply_config 持锁写回 cfg |
| `src/main.rs` | 3 | 启动 reset_count 暂存, main loop 周期上报 |
| `src/io/do_.rs` | 1 | 读 DO 位图输出 |
| `src/channel/ao.rs` | 1 | 读 AO duty |
| `src/ethernet/w5500.rs` | 2 | 读 MAC 配置 |

## 退役策略 (阶段化)

### 阶段 A: BusBackend 热路径下沉 (无锁读端)

把 `BusBackend` (`src/modbus/shared.rs`) **读端** 全部改为无锁自由函数:
读 DI/DO/AI 入 `bus::IO` 原子; 读 holding/proto/cfg 入 `STORAGE`/`CONFIG` 的
`read_with(f)` 闭包. 结果: 高频 Modbus 读不再走 `Spin`.

定义在 `src/bus/backends.rs`:
```
pub fn read_coil(addr: u16) -> Option<bool>     // IO.do_ 原子
pub fn read_disc(addr: u16) -> Option<bool>     // IO.di  原子
pub fn read_input_reg(addr: u16) -> Option<u16> // IO.ai/ao/sys + recovery/health
pub fn read_hold_reg(addr: u16) -> Option<u16>  // STORAGE/CONFIG read_with
```

`BusBackend::{read_coils, read_discrete_inputs, read_holding_registers,
read_input_registers}` 改为直接调用上述自由函数 (单次读无锁, 多连读循环).
读端 FC=01/02/03/04 完全不再走 `Spin`.

### 阶段 B: BusBackend 写端 RCU read-modify-write

写 holding/proto/holding_buf 需要一致性 (e.g. `proto.data[idx] = v; proto.dirty = true`
要原子可见). 改 RCU 写即:

```
pub fn write_hold_reg(addr: u16, value: u16) -> bool {
    // 1. 取 STORAGE 快照 (Arc clone)
    let mut snap = match STORAGE.read_cloned() { Some(s) => s, None => return false };
    // 2. 对 snap 的 device_text / holding_buf / proto 局部改写
    //    (走 `Bus::write_hold_reg` 现有逻辑, 但在 cloned snap 上)
    // 3. STORAGE.write(snap); Rcu::write 原子 swap, 旧值 epoch 回收
}
```

需注意 `proto.status` 状态机 (1=committing, 2=loading, 3=failed): 原代码用
`bus::lock_timeout()` + `self.proto.status = X` 同步. RCU 后多写者并发写 holding
会有写丢失 (last-write-wins); 改用 `DeviceActor` 接管 commit/reload 状态更新
(已在 Actor mailbox 中). `write_hold_reg` 写普通 holding 可能仍需要
"单写者" 保证 — 由 Modbus 任务线程独占可解决.

`SystemConfig` 写回类似: `CONFIG.write(snap)` 配合 `cfg_version` 自增 + `request_apply_config` 递交 DeviceActor.

### 阶段 C: 其余 7 个调用点下沉

按文件接续做相同改造, `device::init()` 改为直接刷 `STORAGE.write` + `CONFIG.write`,
`apply_config` / `commit` / `reload` 的 `proto.status` 状态机走 DeviceActor 消息,
`main.rs` 上报走 IO 原子读. 完成后 `Bus` 结构体本身即可删除, `lock_timeout()` 函数也可删除.

### 阶段 D: 验收

1. `cargo check` / `--features f3` / `--features f4` 全 0 errors
2. host 侧 `host-sync-test` 添加 StorageSnapshot 字段读写并发测试
3. `rg "Spin<Bus>|lock_timeout" src/ --type rust` 返回空
4. `Spin<Bus>` 退役后, `Spin` 仅剩: NVS / BLE 句柄 / 环日志 / 缓冲池 / protocol started_at /
   HAL 外设句柄 — 都是**短临界区, 占用 <1μs**, 是 RCU 不擅长的 native mutable 资源场景,
   合理保留.

## 风险 / 暂未实施原因

- `proto.status` 状态机有写者间因果关系 (Modbus 写 COMMIT=0xC5C5 → DeviceActor
  不应看到 status=1 的两路同步). 阶段 B 需要先把 `proto.status` 从 `STORAGE` 内移到
  `io_state` 单独的 `AtomicU8` 或专属 `DeviceActor` 内部状态, 不进 snapshot, 避免每次
  Rcu 写克隆 11KB 只为改一个 byte.
- `device_config` 的 `write_reg(addr, value)` 是 mutable 写; 阶段 B 需同样 RCU 改造.
- 阶段 A+B 一次变更 14+ 个函数签名/控制流, 易混入未发现的状态竞争. 应**分两次 PR**:
  1) 阶段 A 只读端 (提高测试覆盖, 不动写端, 现存 `Spin` 仍兜底)
  2) 阶段 B 写端 (引入 `DeviceActor::proto_status` 改造 + RCU RMW)

## 当前进度

- [x] 阶段 A 设计 + 函数清单
- [x] 阶段 A 实施
- [x] 阶段 B 设计 + `proto.status` 抽离设计
- [x] 阶段 B 实施
- [x] 阶段 C 调用方下沉
- [x] 阶段 D 验收 + Retire `Bus`

## 落地记录 (2026-07-19)

**全部完成. `Spin<Bus>` / `bus::lock_timeout()` 已从源码中删除.**

- `src/bus/mod.rs`: 删除 `pub struct Bus` / `impl Bus` / `pub static BUS` /
  `pub fn lock_timeout()`. 模块文档更新为"legacy `Spin<Bus>` 已退役".
  `pub use` 精简为只 re-export 仍在使用的 `config_read` / `IO` / `send_event` /
  `IoEvent` / `proto_status`.
- `src/bus/backends.rs`: 删除阶段 A 的双写桥 `sync_storage_from_legacy` /
  `sync_config_from_legacy` / `sync_from_legacy` 与 install 桥
  (`storage_install_proto` / `config_install_snapshot` / `storage_install_snapshot`).
  新增 `config_modify_with_result`, 供 AT 路径 closure-style 改 `cfg` 并拿回返回值.
  `PROTO_STATUS` 读端改读 `proto_status()` (atomic 权威值, 不读快照镜像).
- `src/device/mod.rs`:
  - `init()` 改为直写 `STORAGE` (`storage_write` + `proto_status_set(0)`) + `CONFIG`
    (`config_write`) + `IO.sys.set_fw_version(cfg.fw_version)`; 不再 takes `Spin<Bus>`.
  - `commit()`: 先 `proto_status_set(1)` → `storage_read_with` 取 (data, version, length)
    → NVS 写 → `proto_status_set(0)` + `storage_modify(|s| s.proto.dirty = false)`.
  - `reload()`: `proto_status_set(2)` → NVS 读 → `proto_status_set(0)` +
    `storage_modify(|s| s.proto = ...)`.
  - `apply_config()`: 从 `config_state::config_read` 读 cfg (RCU), 不再过 `Spin`.
  - `DeviceActor::handle` 去除 "status==1 skip commit" 死锁 (`backends` 已在
    排队前 `proto_status_set(1)`, 旧去抖逻辑将不可逆地把所有 commit 请求丢弃).
    Actor mailbox 单线程消费, 去抖本身无意义, 直接执行.
  - `proto_read/write/read_bulk/write_bulk/info` 全部走 `storage_read_with` 与
    `storage_modify`, 不再过 `lock_timeout`.
- `src/ethernet/w5500.rs`: DHCP 回写 IP/mask/gw 改 `backends::config_modify`;
  `apply_netif_config` 改 `config_state::config_read`.
- `src/main.rs`: reset_count / reset_reason 直接写 `bus::IO.sys` 原子.
- `src/channel/{ai,ao}.rs` / `src/io/di.rs`: 去除"legacy Bus 镜像"二次写,
  仅保留 `bus::IO.<field>` 原子 + `send_event`.
- `src/ble_at/{mod,cfg_handlers,handlers}.rs`: AT 路径全部走
  `config_state::config_read` / `backends::config_modify_with_result` /
  `bus::IO` 原子; `with_cfg` 的 FnOnce move 问题用 `match` 双臂按值解决.

### 验收对照

1. `cargo check` / `--features f3` / `--features f4` 全 0 errors (15/13/13 warnings,
   主要是与本任务无关的 `parse_mac` / `modbus::rtu_slave::ADDR` 等 stale imports,
   未在本次修改文件, 保留). ✅
2. `rg "lock_timeout|Spin<Bus>|bus::BUS\b|Bus::new|sync_*_from_legacy" src/ --type rust`
   命中均为注释中描述"已退役"的字符串 (0 处代码消费). ✅
3. 残留 `Spin` 用途仅剩 NVS / BLE 句柄表 / 环日志 / 缓冲池 / protocol started_at /
   HAL 外设句柄 — 短临界区 native mutable, 合理保留 (与本蓝图设计一致). ✅
