# NVS 持久化 + NFC 备份 全链路审计报告

## 审计范围

- 仓库: `/Users/takumi/Workspace/idf`
- 审计日期: 2026-07-26
- 审计对象: NVS 持久化覆盖完整性、NFC 备份机制、并发安全、断电数据丢失窗口

---

## A. NVS Keys 清单

所有 NVS 操作均在 namespace `"gateway"` 下 (`src/device/mod.rs:152`)。

### A1. ProtoStore (协议数据区)

| Key | 类型 | 初始值 | 写入时机 | 读取时机 |
|-----|------|--------|----------|----------|
| `proto_a` | blob (3010B) | 首次为空 | `commit()` 写 inactive blob (`src/device/mod.rs:502`) | `init()` 启动时 (`src/device/mod.rs:244`) |
| `proto_b` | blob (3010B) | 首次为空 | `commit()` 写 inactive blob (`src/device/mod.rs:502`) | `init()` 启动时, 或 active blob CRC 失败回退时 |
| `proto_act` | u8 | 0 (=A) | `commit()` 完成后切换 (`src/device/mod.rs:504`) | `init()` 和 `reload()` 读取 (`src/device/mod.rs:701`) |

触发: 用户 Modbus 写 `PROTO_COMMIT=0xC5C5` -> `request_commit()` -> DeviceActor mailbox -> `commit()`.
旧格式兼容 keys (只读/迁移用): `proto_data`, `proto_magic`, `proto_ver`, `proto_len`.

### A2. SystemConfig (系统配置)

| Key | 类型 | 初始值 | 写入时机 | 读取时机 |
|-----|------|--------|----------|----------|
| `sys_cfg` | blob (144B) | 首次为空 | `apply_config()` (`src/device/mod.rs:583`) | `init()` (`src/device/mod.rs:260`) |
| `sys_cfg_mag` | u32 | 首次为空 | `apply_config()` 同时写 (`src/device/system_config.rs:379`) | `load_from_nvs()` magic 校验 (`src/device/system_config.rs:344`) |

blob 内容: SN(32B) + name(16B) + hw/fw/cfg_ver(6B) + eth_mac(6B) + dhcp(1B) + ip/mask/gw/dns(16B) + ble_mac(6B) + ble_name(8B) + rs485[0..2](45B) = 136B, 对齐 144B (`src/device/system_config.rs:29`)。

触发: 两种路径——
- 直接触发: Modbus 写 `CFG_APPLY=0xB5B5` -> `WriteResult::Apply` -> `request_apply_config()` -> DeviceActor -> `apply_config()` (`src/device/mod.rs:571`)
- 间接触发: `WriteResult::Persist` (SN/PLACE/BLE_NAME/RS485/HW_VER) -> `request_apply_config()` (`src/bus/backends.rs:456-468`)

### A3. DeviceText (设备文本区)

| Key | 类型 | 初始值 | 写入时机 | 读取时机 |
|-----|------|--------|----------|----------|
| `dev_text` | blob (4000B) | 首次为空(全0) | `save_device_text_to_nvs()` (`src/device/mod.rs:667`) | `init()` (`src/device/mod.rs:251`) |
| `dev_text_mag` | u16 | 首次为空 | `save_device_text_to_nvs()` 同时写 (`src/device/mod.rs:669`) | `load_device_text_from_nvs()` magic 校验 (`src/device/mod.rs:618`) |

触发: `request_save_device_text()` -> 直接同步调用 `save_device_text_to_nvs()` (`src/device/mod.rs:685-694`).
调用方: device_text 写入 (`src/bus/backends.rs:425`), web HTTP 修改 (`src/web/mod.rs:660,755,848`), NFC restore (`src/nfc/mod.rs:628`), ADC 校准完成 (`src/channel/calib.rs:160`).

### A4. 其它 keys

| Key | 类型 | 初始值 | 写入时机 | 读取时机 |
|-----|------|--------|----------|----------|
| `rst_cnt` | u16 | 0 | `save_reset_count()` -> DeviceActor (`src/device/mod.rs:100`) | `init()` (`src/main.rs:144`) |
| `mesh_nidx` | u16 | 0 | BLE Mesh 配网回调 -> DeviceActor (`src/device/mod.rs:110`) | `load_mesh_keys()` (`src/device/mod.rs:409`) |
| `mesh_aidx` | u16 | 0 | BLE Mesh 配网回调 -> DeviceActor (`src/device/mod.rs:112`) | `load_mesh_keys()` (`src/device/mod.rs:410`) |
| `dev_cfg` | blob (<=1024B) | 首次为空 | `device_config::store::save()` (`src/device_config/store.rs:82`) | **从未调用** (见 D 节) |
| `web_pwd` | str | 首次为空 | HTTP `/updatepwd` (`src/web/mod.rs:938`) | HTTP login (`src/web/mod.rs:1042`) |
| `web_pwd_f` | u8 | 首次为空 | HTTP `/updatepwd` 写 0x66 (`src/web/mod.rs:940`) | `load_web_password()` flag 校验 (`src/web/mod.rs:1039`) |
| `cfg_XX` | blob | 首次为空 | BLE AT sub-config 写入 (`src/ble_at/logic_handlers.rs:138`) | BLE AT config 读取 (`src/ble_at/logic_handlers.rs:165`) |

BLE AT config keys 动态生成: `cfg_00` ~ `cfg_28` (sub=0x00~0x28), 命名规则 `format!("cfg_{:02x}", sub)` (`src/ble_at/logic_handlers.rs:113-114`).

---

## B. 持久化丢失窗口

### B1. SystemConfig (SN/IP/RS485/BLE_NAME 等)

写入路径: Modbus FC=06/10 -> `write_hold_reg()` -> SystemConfig `write_reg()` -> `WriteResult::Persist` / `Apply` -> `request_apply_config()` -> DeviceActor mailbox -> `apply_config()` -> `save_to_nvs()`.

**丢失窗口: ~50-100ms** (DeviceActor idle 超时 50ms + NVS 写入). 突然断电最多丢失最近一次 `Persist`/`Apply` 写入.

依据: DeviceActor idle 50ms 心跳 (`src/device/mod.rs:127-128`), mailbox 消息立即处理.

### B2. DeviceText (5000-6999 区)

写入路径: Modbus FC=10 写 `DEVICE_TEXT_BASE..END` -> RCU RMW -> `request_save_device_text()` -> **同步直接调用** `save_device_text_to_nvs()` (`src/device/mod.rs:685-694`).

**丢失窗口: ~1-5ms** (NVS blob 写入本身, 约 4000 bytes / NVS 页大小). 这是直接同步写, 不经过 Actor mailbox.

### B3. ProtoStore (协议数据区)

写入路径: Modbus 写 `PROTO_COMMIT=0xC5C5` -> DeviceActor -> `commit()` -> A/B 双 blob 写入.

**丢失窗口: 用户主动 commit 前全部丢失**. proto 数据修改 (写 PROTO 区) 只更新 RAM 快照 (`snap.proto.dirty = true`), 不触发 NVS. 必须显式写 `PROTO_COMMIT=0xC5C5` 才落盘 (`src/bus/backends.rs:514-518`).

A/B 双 blob 机制 (`src/device/mod.rs:486-510`) 保证 commit 过程中掉电不损坏: 先写 inactive blob, 再切 active 标志.

### B4. Reset Count

写入路径: `main()` 同步: `load_reset_count()` + 1 -> `save_reset_count()` -> DeviceActor mailbox -> `PersistResetCount` -> NVS set_u16 (`src/device/mod.rs:99-100`).

**丢失窗口: ~50ms** (DeviceActor mailbox 处理延迟).

### B5. holding_buf (FUNC_COUNT / SENSOR_MIN-MAX / 用户自定义 P 区)

**丢失窗口: 永久丢失 (NVS 从未写入)**.

holding_buf (2048 words, 地址 `HOLD_PXX_BASE`..`HOLD_PXX_END` = 0x0880..0x107F) 在 `init()` 中初始化为全 0 (`src/device/mod.rs:287`), 且**没有任何 NVS 持久化路径**.

仅有的恢复途径是 NFC 备份:
- NFC 标签在场 + NFC 数据有效 -> 5s 轮询周期内恢复 (`src/nfc/mod.rs:514-516`)
- NFC 标签不在场 -> 全部丢失, 重启后为 0

**关键状态存储在 holding_buf 中但无 NVS 持久化:**

| 寄存器 | 地址 | 说明 | NVS 持久化 |
|--------|------|------|-----------|
| FUNC_COUNT | 0x08FC | 功能数量 (BLE arm) | **无** |
| SENSOR_MIN[0..7] | 2280..2287 | ADC 校准下限 | **无** |
| SENSOR_MAX[0..7] | 2288..2295 | ADC 校准上限 | **无** |
| HOLD_PXX 用户区 | 0x0880..0x107F | 通用保持寄存器 | **无** |

### B6. DeviceConfigTable (设备功能配置表)

`store::save()` (`src/device_config/store.rs:56`) 存在, NVS key `"dev_cfg"` 存在, 但**`save()` 在整个代码库中从未被调用**. 搜索结果只有定义和 `store::load()` 的引用, 无任何运行时 `save()` 调用点.

同时, `store::load()` 在 `init()` 中也**从未被调用**: `ConfigSnapshot::new()` 直接用 `DeviceConfigTable::default()` (空表) (`src/device/mod.rs:292`).

**结果: DeviceConfigTable 既不加载也不保存, 完全不持久化.**

---

## C. NFC + NVS 并发问题

### C1. 架构概述

- **NFC 线程**: 后台 pthread (`"nfc-st25"`, 8KB 栈), 每 5s 轮询 (`src/nfc/mod.rs:437-548`)
- **Modbus 写**: Modbus TCP/RTU 任务线程, 通过 `RCU_WRITE_LOCK` 串行化 (`src/bus/backends.rs:45`)
- **DeviceActor**: 单线程消费 mailbox, 处理 NVS 写入 (`src/device/mod.rs:69-129`)
- **RCU**: `STORAGE` / `CONFIG` RCU 快照, 读 lock-free, 写原子替换 (`src/bus/storage_state.rs:78`, `src/bus/config_state.rs:54`)

### C2. NFC 备份路径 — 读取 holding_buf

```
nfc_loop()
  -> storage_read()          // RCU lock-free 读, 返回 Arc<StorageSnapshot>
  -> regbuf_arc.as_ref()     // 持 Arc 引用, 快照不可变
  -> backup_to_nfc(dev, rb)  // 分块写 EEPROM (30B/chunk, ~0.82s 总耗时)
```

NFC 在备份过程中持有 `Arc<StorageSnapshot>` 引用. 此时如果 Modbus 写 holding_buf 触发 RCU 替换:
- RCU `write()` 推进 epoch, 原子替换指针 (`src/bus/rcu.rs:114-117`)
- NFC 持有的 Arc 仍指向旧快照, 旧快照在 NFC drop Arc 后由 RCU sweep 回收 (`src/bus/rcu.rs:186-198`)
- **不会 UAF, 不会读到部分更新**

**结论: NFC 读取与 Modbus 写入之间无数据竞争.**

### C3. NFC 恢复路径 — 写入 holding_buf

```
restore_from_nfc()
  -> storage_modify(|snap| { snap.holding_buf[..n] = nfc_words; snap.proto.dirty = true; })
  -> request_save_device_text()  // 持久化 device_text (非 holding_buf!)
```

`storage_modify()` 内部获取 `RCU_WRITE_LOCK` (`src/bus/backends.rs:593`), 与 Modbus 写入的 `write_hold_reg()` 共享同一把锁 (`src/bus/backends.rs:409`).

**场景 1: NFC restore 与 Modbus 写入交错**

| 时间 | Modbus 线程 | NFC 线程 | 结果 |
|------|-------------|----------|------|
| T1 | write_hold_reg(FUNC_COUNT, 42) — 持 RCU_WRITE_LOCK, clone+修改+swap | | holding_buf[FUNC_COUNT]=42 |
| T2 | | storage_modify — 等 RCU_WRITE_LOCK | |
| T3 | 释放锁 | | |
| T4 | | 持锁, clone 当前快照 (含 FUNC_COUNT=42), 全量覆写为 NFC 旧数据 | FUNC_COUNT=42 **被覆盖** |

**风险: NFC restore 全量覆写 holding_buf, Modbus 刚写入的值被丢弃.**

NFC restore 是全量覆盖 `snap.holding_buf[..n]`, 不做增量合并 (`src/nfc/mod.rs:622-623`).

**场景 2: NFC 轮询导致反复 restore**

如果 NFC 标签携带旧数据, 且 Modbus 持续修改 holding_buf:
1. Modbus 写入 FUNC_COUNT=42 -> holding_buf 更新
2. NFC 5s 轮询 -> 比较 NFC 数据(旧) != holding_buf(新) -> **触发 restore** -> FUNC_COUNT 被覆盖回旧值
3. Modbus 再次写入 FUNC_COUNT=42
4. NFC 5s 再次轮询 -> 同上 -> 又被覆盖

**结果: 当 NFC 标签在场时, 所有 holding_buf 修改在 5s 内被 NFC restore 覆盖.** 这是 NFC "restore always wins" 的设计逻辑 (`src/nfc/mod.rs:510-516`).

**场景 3: NFC backup 过程中发生 RCU 替换**

backup_to_nfc() 在 ~0.82s 内分 137 个 30B chunk 写 EEPROM. 期间持有旧快照 Arc.
如果 Modbus 在此期间修改 holding_buf:
- NFC 备份的是修改前的快照 (旧数据)
- EEPROM 最终包含旧数据
- NFC 5s 后再次轮询时, 发现 EEPROM == 旧数据, 但 holding_buf == 新数据 -> 触发 restore -> **新数据被丢弃**

**结论: NFC 存在"静默覆盖"问题. NFC 无条件信任自己的数据, 不考虑 holding_buf 是否有本地修改.**

### C4. NFC 备份与 ADC 校准冲突

ADC 校准 (`src/channel/calib.rs:144-157`) 写入 SENSOR_MIN/MAX 到 holding_buf via `storage_modify()`. 如果 NFC 在场:
- 校准值写入 holding_buf
- NFC 5s 轮询: NFC EEPROM 不含新校准值 -> CRC 不匹配 -> **触发 backup** (正确行为)
- 但如果 NFC EEPROM 含旧校准值且 CRC 有效 -> **触发 restore** -> 校准值被覆盖

实际路径: `calib.rs:160` 调用 `request_save_device_text()`, 这只持久化 device_text, 不更新 NFC. NFC 将在下次 5s 轮询时检测到差异并备份 (如果 CRC 失败) 或恢复 (如果 CRC 有效但内容不同).

### C5. RCU retire_queue overflow 风险

RCU retire_queue 仅 4 槽 (`src/bus/rcu.rs:43`). 如果 NFC backup (0.82s 持 Arc) + Modbus 高频写入 (多个连接并发) 同时进行, 可能导致:
- Modbus 写: RCU swap 新值, 旧值入 retire_queue (4 槽)
- NFC 仍持有旧 Arc (reader_counts > 0, 该 epoch 不可回收)
- 新旧旧旧... 4+ 个未回收快照 -> retire slot 覆盖 -> leak ~11KB (`src/bus/rcu.rs:127-129`)

这是已知限制, 代码中有日志警告 (`src/bus/rcu.rs:129`).

---

## D. 持久化缺失状态

### D1. holding_buf — 从未写入 NVS (**P0 关键缺陷**)

**问题**: `holding_buf` (2048 words = 4096 bytes) 在整个代码库中**没有任何 NVS 写入路径**.

证据:
1. `init()` 中 `holding_buf` 初始化为全 0 (`src/device/mod.rs:287`), 不从 NVS 加载
2. `request_save_device_text()` 只保存 `snap.device_text`, 不保存 `snap.holding_buf` (`src/device/mod.rs:688-689`)
3. 搜索全部 `set_blob`/`set_u16`/`save_to_nvs` 调用 (`src/device/mod.rs:100-114, 370-374, 502-504, 583, 667-669`, `src/device/system_config.rs:377-379`, `src/device_config/store.rs:82`), 无一涉及 holding_buf

**受影响的关键状态:**

| 状态 | 存储位置 | 丢失后果 |
|------|----------|----------|
| FUNC_COUNT (0x08FC) | holding_buf | BLE arm 功能数丢失, 需重新配置 |
| SENSOR_MIN[0..7] (2280-2287) | holding_buf | ADC 校准值丢失, 每次重启需重新校准 |
| SENSOR_MAX[0..7] (2288-2295) | holding_buf | 同上 |
| 用户自定义 P 区 (0x0880..0x107F) | holding_buf | 用户写入的 Modbus 保持寄存器全部丢失 |

**恢复途径**: 仅 NFC 备份 (`src/nfc/mod.rs:621-624`), 但 NFC 仅在标签在场时 5s 轮询恢复.

**修复建议**: 在 `apply_config()` 或 `commit()` 中增加 holding_buf 持久化, 使用独立 NVS blob key (如 `"hld_buf"`), A/B 双 blob 机制 (复用 proto 模式).

### D2. DeviceConfigTable — 既不加载也不保存 (**P0 关键缺陷**)

**问题**: `device_config::store::load()` 和 `device_config::store::save()` 函数存在 (`src/device_config/store.rs:8, 56`), 但在 `init()` 中从未调用 `load()`, 运行时从未调用 `save()`.

证据:
1. `device::init()` 中 `ConfigSnapshot` 创建时 `device_config` 用 `DeviceConfigTable::default()` (空表) (`src/device/mod.rs:292`)
2. 搜索全库 `DeviceConfigTable` 使用, `load()` 仅在 `DeviceConfigTable` impl 中定义 (`src/device_config/mod.rs:69-72`), 无外部调用
3. `save()` 仅在 `DeviceConfigTable` impl 中定义 (`src/device_config/mod.rs:76-77`), 无外部调用
4. `write_reg` 在 2300+ 地址的写入 (`src/device_config/mod.rs:111-161`) 只修改 RAM 中的 `DeviceConfigTable`, 不触发 NVS

**受影响的关键状态:**
- 设备功能配置 (traffic signal / jet fan / blower / pump / sensor 类型)
- RS485 端口映射
- 从站地址 / 功能码 / 寄存器地址
- 扩展参数 (params)

**丢失后果**: 所有设备功能配置重启后丢失, RS485 轮询表为空.

### D3. DI/DO 反转标志

搜索 `reverse`/`polarity`/`invert` 在 `src/` 下, 仅找到硬件层 PCA9555 的 `bit_reverse()` (`src/hal/pca9555.rs:48`), 这是硬件引脚布局固定的位反转 (不可配置), 非用户可配置的 DI/DO 通道反转.

**结论**: 当前代码中没有用户可配置的 DI/DO 反转标志, 因此不存在该状态的持久化缺失问题.

### D4. NTP / WiFi / 以太网配置

- **NTP**: 代码中无 NTP server 配置 (仅 `src/main.rs:215` 注释提到 NTP 作为辅助服务), 无 NTP 相关 NVS key
- **WiFi**: `#[cfg(feature = "wifi")]` 条件编译 (`src/main.rs:165-172`), WiFi SSID/password 未在审计范围内找到 NVS 持久化
- **以太网 IP/Mask/GW**: 存储在 SystemConfig 中, 通过 `save_to_nvs()` 持久化 (**已覆盖**)

### D5. 持久化覆盖汇总

| 状态 | NVS 持久化 | NFC 备份 | 断电丢失窗口 |
|------|-----------|----------|-------------|
| SystemConfig (SN/IP/RS485/BLE_NAME/HW_VER) | **是** | 间接 (通过 holding_buf) | ~50-100ms |
| DeviceText (5000-6999) | **是** | 间接 (通过 holding_buf) | ~1-5ms |
| ProtoStore (0x4000+) | **是** (A/B) | 间接 (通过 holding_buf) | commit 前全部丢失 |
| Reset Count | **是** | 否 | ~50ms |
| BLE Mesh Keys | **是** | 否 | ~50ms |
| Web Password | **是** | 否 | 立即 |
| BLE AT Configs | **是** | 否 | 立即 |
| **FUNC_COUNT** | **否** | 仅 NFC | **永久丢失 (无 NFC)** |
| **SENSOR_MIN/MAX** | **否** | 仅 NFC | **永久丢失 (无 NFC)** |
| **holding_buf 用户区** | **否** | 仅 NFC | **永久丢失 (无 NFC)** |
| **DeviceConfigTable** | **否** | **否** | **永久丢失** |

---

## 附录: 代码引用索引

| 文件 | 关键行号 | 内容 |
|------|---------|------|
| `src/device/mod.rs` | 152 | NVS namespace `"gateway"` |
| `src/device/mod.rs` | 155-183 | NVS key 常量定义 |
| `src/device/mod.rs` | 237-322 | `init()`: NVS 加载 + RCU 写入 |
| `src/device/mod.rs` | 287 | `holding_buf: vec![0u16; 2048]` 初始化为 0 |
| `src/device/mod.rs` | 292 | `device_config: Arc::new(DeviceConfigTable::default())` 空表 |
| `src/device/mod.rs` | 461-533 | `commit()`: proto A/B 双 blob 写入 |
| `src/device/mod.rs` | 571-598 | `apply_config()`: SystemConfig NVS 写入 |
| `src/device/mod.rs` | 653-678 | `save_device_text_to_nvs()`: device_text NVS 写入 |
| `src/device/mod.rs` | 685-694 | `request_save_device_text()`: 仅保存 device_text |
| `src/device/system_config.rs` | 19-21 | SystemConfig NVS keys |
| `src/device/system_config.rs` | 117-143 | `WriteResult` 枚举 (Ok/Persist/Apply/Reset) |
| `src/device/system_config.rs` | 342-383 | SystemConfig load/save NVS |
| `src/device_config/store.rs` | 8-54 | `load()` (从未被调用) |
| `src/device_config/store.rs` | 56-85 | `save()` (从未被调用) |
| `src/nfc/mod.rs` | 107-111 | `MEMORY_END=0x1FFF` (LOOP12 扩容) |
| `src/nfc/mod.rs` | 432-548 | `nfc_loop()`: 5s 轮询主循环 |
| `src/nfc/mod.rs` | 494-516 | NFC 决策逻辑: 有效且一致=跳过, 有效不一致=**restore**, CRC 失败=backup |
| `src/nfc/mod.rs` | 560-611 | `backup_to_nfc()`: 分 30B chunk 写 EEPROM |
| `src/nfc/mod.rs` | 618-634 | `restore_from_nfc()`: 全量覆写 holding_buf + request_save_device_text |
| `src/bus/backends.rs` | 45 | `RCU_WRITE_LOCK`: 串行化 STORAGE + CONFIG 写者 |
| `src/bus/backends.rs` | 408-546 | `write_hold_reg_locked()`: Modbus 写入路由 |
| `src/bus/backends.rs` | 472-478 | SystemConfig NotFound 时落入 holding_buf |
| `src/bus/backends.rs` | 592-598 | `storage_modify()`: RCU RMW |
| `src/bus/storage_state.rs` | 60-61 | `StorageSnapshot::holding_buf: Box<[u16]>` (2048 words) |
| `src/bus/storage_state.rs` | 78-80 | `STORAGE` RCU 全局实例 |
| `src/bus/rcu.rs` | 43 | `RETIRE_QUEUE_LEN = 4` |
| `src/bus/rcu.rs` | 107-136 | `Rcu::write()`: 原子替换 + epoch 回收 |
| `src/channel/calib.rs` | 45-173 | ADC 自动校准: 写 holding_buf + request_save_device_text |
| `src/channel/ai.rs` | 115-133 | `read_sensor_calib()`: 从 holding_buf 读校准值 |
| `src/config.rs` | 494 | `FUNC_COUNT: u16 = 0x08FC` |
| `src/config.rs` | 460-461 | `HOLD_SENSOR_MIN_BASE=2280`, `HOLD_SENSOR_MAX_BASE=2288` |
| `src/main.rs` | 104-109 | `nvs_flash_init()` |
| `src/main.rs` | 128-134 | `device::init()` |
| `src/main.rs` | 226-229 | `nfc::start()` |
| `src/web/mod.rs` | 68-69 | Web password NVS keys |
| `src/web/mod.rs` | 937-944 | Web password NVS 写入 |
| `src/ble_at/logic_handlers.rs` | 113-114 | BLE AT config NVS key 生成规则 |
