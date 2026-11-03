# 紧急修复审计报告: Actor 线程栈 (2026-07-20)

## 1. 问题

设备启动后立即 `Guru Meditation Error: Core 0 panic'ed (StoreProhibited)`。

**症状**:
- `[main] device init ok` 日志后立即 panic
- EXCVADDR = 0x0031000c (DROM, 不可写)
- memcpy 函数内部, source=NULL
- 8KB 栈下 100% 复现

## 2. 根因分析

| 步骤 | 现象 |
|------|------|
| 1 | `device::init()` 启动 DeviceActor (LazyLock force) |
| 2 | actor 线程用默认 8KB 栈 spawn |
| 3 | 栈溢出 (8KB 装不下 lazy init 临时变量 + 后续 commit/reload 11KB snapshot clone) |
| 4 | 栈溢出覆盖主线程栈帧 |
| 5 | 主线程调 `nvs.get_u16()` 时, 栈帧指针已破坏 |
| 6 | nvs_get_u16 内部 memcpy 写入被破坏地址 → StoreProhibited |

**关键证据**:
- PROBE-NVS-LOCKED 显示 lock 已成功获取 (NVS 句柄有效, ptr=0x3fca300c, handle=1)
- PROBE-LR-2 在 `nvs.get_u16()` 调用前打印
- 然后在 get_u16 内部 panic
- 32KB 栈: 100% 正常, reset count 持续递增 (45→46→...)

## 3. 修复

`src/actor/mod.rs::spawn()`:
```rust
std::thread::Builder::new()
    .name(name)
    .stack_size(32 * 1024)  // 32KB, 防止 commit/reload 序列化 11KB snapshot 时栈溢出
    .spawn(move || { ... })
```

## 4. 验证

| 指标 | 8KB 栈 | 32KB 栈 |
|------|--------|---------|
| 启动 panic | ✗ 100% | ✗ 0% |
| reset count 持久化 | ✗ | ✓ (45→46→47) |
| W5500 DHCP | ✗ | ✓ 192.168.51.140 |
| BLE GATT 启动 | ✗ | ✓ |
| main loop 持续 | ✗ | ✓ uptime=1s/2s/... |

## 5. 工业教训

- **rust std::thread 默认栈在 esp-idf 上是 8KB**, 不够大
- **actor 涉及大结构体 clone (>4KB) 时必须显式 .stack_size(32KB+)**
- ProtoStore (1500 字) + device_text (2000 字) + holding_buf (2048 字) = 11KB,
  Box<[u16]> clone (to_vec + into_boxed) 需要至少 16KB 中间栈空间
- 默认 8KB 在 commit/reload 路径必然溢出
- **强制规范**: 所有 esp-idf actor 线程 stack_size >= 32KB

## 6. 关联

- 之前 AUDIT_2026-07-19_SPIN_BUS_RETIRE 退役 legacy Bus 时引入了 Actor 模型
- Actor 模型本身正确, 但 spawn 时未指定 stack_size, 留下隐患
- 之前 baseline 19796fb 也有此隐患, 只是当时 stack 中没有大结构体 clone (commit 路径未触发)
- 阶段 1 (Persist) 引入 PersistResetCount → 触发 commit → 触发 actor handle → 触发栈溢出
- 阶段 2 (dev_text persist) 进一步增加栈使用量

**关键洞察**: 19796fb commit 通过了 "测试" 是因为 actor 只在 idle() 中跑, 没真正调用 handle(),
所以栈没溢出. 但本次 Persist 路径强制触发 actor.handle() 暴露了这个隐藏 bug.
