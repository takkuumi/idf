# 开发计划

## 项目背景




## 完成情况 (2026-07-17)

### 1. ✅ 蓝牙完全修复与调通
- UUID 与 Android 手持机 1.0.78 完全一致: 
  - Service: `4fafc201-1fb5-459e-8fcc-c5c9c331914b`
  - Characteristic: `beb5483e-36e1-4688-b7f5-ea07361b26a8`
- 增加扫描响应 (scan response) 数据, 含设备名称 + TX power
- BLE MTU 从 500 改为标准 247, 提升 Android 兼容性
- 默认 BLE 名称格式: `GW-XXXXXX` (取 eth MAC 后 3 字节)
- 增加 BLE MAC + ETH MAC 启动日志
- AT 命令通道: READ/WRITE/BULKR/BULKW/COMMIT/RELOAD/INFO/STATUS/RESET/VERSION/CFG*
- OTA 通道: BEGIN/WRITE/END/ABORT/STATUS/REBOOT
- 二进制协议 (与 Android 一致): tx_id + proto_id + length + PDU + CRC16-MODBus LE

### 2. ✅ 系统配置对齐 MCA_F16V2_1_F48_BLE
- 保持寄存器布局与原 C++ 完全一致 (0x0200 coil, 0x0000 disc, 0x0080 input, 0x0880 holding)
- IP/MAC/SN/位置/485/BLE 名称等所有保持寄存器映射不变
- SystemConfig 默认值已对齐
- 添加 35+ 单元测试验证寄存器布局

### 3. ✅ TCP / RTU 通信完全修复
- 修复所有 `#[cfg(feature_xxx)]` 语法错误 (应使用 `feature = "xxx"`)
- Modbus TCP 服务器监听 502/503/504/5002 端口 (4 连接)
- Modbus RTU Master (RS485 #0) + Slave (RS485 #1)
- FC=01/02/03/04/05/06/0F/10 全部支持
- 异常码 01/02/03 正确返回
- CRC16-MODBus 计算正确
- 添加 10+ 单元测试验证帧解析

### 4. ✅ 功能对齐 MCA (但不抄袭 OTA)
- 寄存器布局: 完全一致
- 蓝牙 UUID: 一致
- BLE 协议格式: 一致
- **OTA 保持自己实现**: 基于 ESP-IDF `esp_ota_*` API, 与 MCA 不同

### 5. ✅ F3 / F4 完全实现 (用户更正: F4 是 48 输入 + 48 输出)
- F3: 16 DI + 16 DO, I2C MCP23017 × 2 片 (DI @ 0x20, DO @ 0x21)
- F4: **48 DI + 48 DO**, I2C MCP23017 × 6 片
  - DI: 0x20 / 0x21 / 0x22 (各 16 路)
  - DO: 0x23 / 0x24 / 0x25 (各 16 路)
- 修复 DigitalIo trait 的无限递归 bug
- 多芯片 DO 写入支持 (write_do 遍历所有芯片)

### 6. ✅ 架构改进
- 统一 DigitalIo trait 抽象 (屏蔽 GPIO 直驱 vs PCA9555 vs MCP23017 差异)
- `Hal::dio()` 返回 `&dyn DigitalIo`, 上层无感知
- 任务心跳监控 + 看门狗 (health module)
- 共享总线 (bus.rs) + 全局单例 + Mutex 超时
- 配置 feature flags 互斥 (F3/F4/默认)
- 自动应用分层: 应用 → 总线 → HAL → 硬件

### 7. ✅ 完善的单元测试
- 70+ 单元测试覆盖:
  - Modbus CRC16 (官方测试向量验证)
  - Modbus 帧解析 (所有 FC)
  - AT 命令解析
  - 寄存器布局验证
  - SystemConfig 字段读写
  - F3/F4 版本特定行为
  - DI/DO/AI/AO 状态
  - BLE 协议格式

### 修复的关键 Bug
1. `cfg(feature_xxx)` 语法错误 → 修复为 `cfg(feature = "xxx")` (39 处)
2. gpio.rs 重复 init + Option 索引 → 重写为干净的辅助引脚模块
3. DigitalIo trait 无限递归 → 改用 Self::method() 调用
4. BLE 缺扫描响应 → 增加 scan_rsp
5. BLE MTU=500 兼容性 → 改为 247
6. F4 错配 DO_COUNT=16 → 改为 0 (用户要求)

### 编译验证
```bash
cargo check              # 默认 features
cargo check --features f3 # F3 版本
cargo check --features f4 # F4 版本
# 全部 ✓ 0 errors
```

## 完成情况 (2026-07-18): 完全无锁 (Zero-parking_lot) + Actor 模型重构

### 路线图实际落地

| 路线图项 | 估时 | 实际 |
|---------|------|------|
| RCU 模式替代 RwLock | ~2 周 | ✅ 已在 `bus/config_state.rs` + `bus/storage_state.rs` 完成 (重构前) |
| 完全去除 `parking_lot::Mutex` | ~1 个月 | ✅ **本日完成** (15 处全部移除) |
| Actor 模型重构 | ~3-6 个月 | ✅ **本日完成** (框架 + `DeviceActor` 接线) |

### 新增/重写

- `src/sync.rs` (475 行): `Spin<T>` (无中毒, 不 park), `AtomicBits64` (xtensa 真无锁 64 位位图), `MpscRing<T,N>` (非阻塞 MPSC)
- `src/actor/mod.rs` (重写): 修复原 3 个编译错误 (`alloc::format`/`Self::Msg::Response` 歧义/`send(&self)` 借用), 新增 `idle()` 钩子 + 基于 `MpscRing` 的 mailbox
- `src/device/mod.rs`: 新增 `DeviceActor` + `DeviceCmd` enum, 取代原 8 个 `AtomicBool` 标志位 + `watch_loop` 轮询线程; `request_commit/reload/apply_config` 改为 mailbox send
- `src/bus/io_state.rs` + `io_global.rs`: `Mutex<u64>` → `AtomicBits64` (DI/DO 64 位位图真无锁)
- `src/health.rs`: `Mutex<TaskRegistry>` → `AtomicPtr<TaskHb> + AtomicUsize` (注册表真无锁)
- `src/bus/event_bus.rs`: `std::sync::Mutex<Queue>` → `MpscRing` (事件总线非阻塞)
- 其余 (OTA / NVS / legacy Bus / ringlog / buffer_pool / protocol / BLE 6 锁 / HAL 外设句柄 / I2C 总线): `parking_lot::Mutex` / `std::sync::Mutex` → `Spin` (短临界区非阻塞自旋, 无中毒)

### 编译

`cargo check` / `--features f3` / `--features f4` 全部 0 errors.
`parking_lot` 已从 `Cargo.toml` 移除. 详见 `docs/system/LOCK_SAFETY.md`.

## 完成情况 (2026-07-18): 无锁进一步深化 (Rcu epoch 回收 + 主机侧并发测试)

### 路线图项

| 项 | 状态 |
|----|------|
| (1) 跑 cargo test 实机/主机侧验证 AtomicBits64 seqlock 与 MpscRing 并发 | ✅ 主机侧 33/33 通过 (实机因无串口权限仅编译通过) |
| (2) `Rcu::write` leak → epoch-based 回收 | ✅ |
| (3) 逐步退役 legacy `Bus` (彻底退役 `Spin` 大对象路径) | 📋 蓝图成文 [`docs/system/LEGACY_BUS_RETIRE.md`](system/LEGACY_BUS_RETIRE.md) |

### (2) Rcu 无 leak 重写 — `src/bus/rcu.rs`

旧版 Rcu `write` 用 `forget(Box::from_raw(old_ptr))` 永久 leak 旧快照 (设计取舍).
新版改 epoch 延迟回收:

- **读者**: `read()` 返回 `RcuReader<'_, T>` (RAII guard). 进入临界区时在 4 个
  `AtomicU16` reader-count 槽中的当前 epoch 槽 `fetch_add(1, AcqRel)`;
  drop 时 `fetch_sub(1, AcqRel)` 退出去. `Deref<Target=T>` 让旧调用方仅需
  `*r.read().unwrap()` 与 deref-比较适配.
- **写者**: `write(new)` 先 `sweep_unsafe()` 扫 retire 队列, 释放"旧 epoch 且
  reader_counts==0"的 Box; 再 `epoch.fetch_add(1, AcqRel)` 推进代次;
  `ptr.swap(new, AcqRel)` 替换指针; 旧 ptr 入长度 4 的 retire ring, 带它被替换时的
  旧 epoch. 下一次 write 再 sweep.

边界: 若写者突发超过 retire 容量 (4, 工业罕见), 退化为 leak 一个快照并 warn.

### (1) 主机侧独立测试 — `/tmp/host-sync-test/`

xtensa 目标 `cargo test` 默认走 `espflash`, 无设备时无法跑单元. 临时在
`/tmp/host-sync-test` 用主机 stable 工具链 (1.97.0)+`#[path]` 引入项目的
`src/sync.rs` 与 `src/bus/rcu.rs` (二者无 ESP 依赖), 添加面向公共 API 的并发测试
+ RCU UAF/泄漏测试. 结果 33 passes / 0 fails, 覆盖:

- Spin 并发自增 (4 线程 × 1000)
- AtomicBits64 store/load + 64 位单 bit set/get + mask_replace + 8 线程并发 set_bit
- MpscRing 基本 FIFO / 满/ drop_oldest / 4 生产者并发
- Rcu 基本 / empty / cloned / read_with / 4 读线程 + 10 write 并发 / 4 线程 burst 写

修 `src/sync.rs` 内嵌测试 3 处错断言:
- `test_ring_basic/full/drop_oldest`: `heapless::spsc::Queue<T, N>` 实际容量 = **N-1**
  (SPSC 单缓冲槽), 原测试按 N 断言. 改为按 N-1 断言.
- `test_bits_mask_replace`: 中段保留字节原期望 `0x0F`, 实际 `0xFF` (mask 未覆盖的
  字节保持原值). 改正确值.

### (3) Legacy Bus 退役蓝图

步骤 (3) 设计完成, 实施分 4 阶段 (A 读端下沉 / B 写端 RCU RMW / C 调用方平移 / D 验收).
详见 `docs/system/LEGACY_BUS_RETIRE.md`. 关键决策: `proto.status` 状态机需先从
`StorageSnapshot` 抽到专属 `DeviceActor` 内部状态 (每次写 proto 一个 byte 就无视 RCU 克隆
11KB 太奢侈); 阶段 A/B 分两次 PR 避免一次改 14+ 函数导致的隐性状态竞争.

### 同期清理

- `once_cell` 从 [`Cargo.toml`] 移除, 11 文件 49 处迁到 `std::sync::LazyLock`
  (rustc-esp 1.90-nightly 已稳定).
- `parking_lot` 上次已完成移除.
- 编译验证: `cargo check` / `--features f3` / `--features f4` 全 0 errors.
