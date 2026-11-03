# 架构决策: 下一阶段优先级与实施边界 (2026-07-19)

> 由高级系统架构师出具. 关联 `AUDIT_2026-07-19_SPIN_BUS_RETIRE.md` §3.3 中
> 已识别但本轮不修复的 3 项缺漏.

## 1. 优先级矩阵

| 项 | 影响 | 优先级 | 实施风险 | 决策 |
|----|------|------|---------|------|
| (P0) `INREG_AI_COUNT` 硬编码 4 | 目标 #3 (内存布局) 与 MCA F48=8 不一致; 目标 #2 (Android 1.0.78 `READ_ADC_VALUE=0x10`) 期望按实际通道读 | 高 | 中: 改动涉及 `config::hw_version` + `bus::io_state` + `hal::adc` + `channel::ai` + `ai_calib` 一致性 | **本轮做**, 但严格按 MCA F16/F48 决定值, 不影响当前 F3/F4 feature flag |
| (P1) `HOLD_485_*_COMERR/APPERR` 字段语义化 | 目标 #3 地址级 0x0880-0x0883 RO 计数, 我们当前 `holding_buf[0..3]` 兜底全 0 | 中 | 低: 仅在 `SystemConfig::read_reg` 加 4 个 `match addr` 分支, 返回 `recovery::stats()` 派生值 | **本轮做**, 低风险可平行 |
| (P2) MULTICAST UDP 监听业务 | MCA 真实功能 (`.ino:2304` `udp.beginMulticast`), 我们没实现; Android 1.0.78 **完全不依赖** | 中 | **高**: 开 UDP socket 监听线程, 与现有 `ethernet::w5500` + `modbus::tcp_server` 资源共享/阻塞有未知面, 没 host 测试覆盖 | **本轮不做**, 排到下一阶段专项设计 (需 `comm/udp-multicast` 文档 + 单测) |
| (P3) host-test 增 RCU 多写者并发 | 防御 `Rcu::write` 串行假设在 retired-spin 后真机压测成 leak | 中 | 低: 仅 `/tmp/host-sync-test` 加测试 | **本轮做**, 由测试工程师出 |

## 2. P0 实施边界 (架构师约束)

### 2.1 不可越界

- **不引入新硬件 feature flag**. 修复通过 `hw_version::AI_COUNT` 提供 MCA 兼容值, F16=4 / F3=4 / F4=8 (MCA `MCA_F48_HARDWARE_RESOURCE`).
- **不动 `hal::adc` 实际采样通道数** (硬件提供 ADC1_CH0..5 = 6 路物理), 让 `INREG_AI_COUNT` 与 Modbus 报告值匹配 MCA 期望, 物理采样可通过"取前 N 路"映射.
  → 即: 物理仍采 6 路; Modbus `INREG_AI_BASE..+INREG_AI_COUNT` 只暴露 MCA 期望通道数 (F16=4 / F48=8)。 F48 期望 8 但我们硬件只有 6 → **缺 2 路在 INREG 报 0** (与 MCA 在没硬件时返回 0 等同行为)。
- **不修改 BLE 二进制协议** (1.0.78 兼容).

### 2.2 修复合约

```rust
// src/config.rs hw_version 模块
pub const AI_COUNT: u16 = if cfg!(feature = "f4") { 8 /* MCA F48 */ }
                         else if cfg!(feature = "f3") { 4 /* MCA F16 默认 */ }
                         else { 4 /* F16 默认 */ };

// src/config.rs regs 模块
pub const INREG_AI_COUNT: u16 = hw_version::AI_COUNT;   // 不再写死 4
pub const INREG_AI_STATUS_BASE: u16 = 0x0088;           // 保持
// status 寄存器对齐: MCA REG_STATU_AMAX = 0x008B (F16) / 0x008F (F48) — 即 start=0x0088, count=同 AI_COUNT
```

- 校验 `backends::read_input_reg` 已使用 `INREG_AI_COUNT` 区段检查, 改完自动适配。
- `IO.ai` 物理仍是 6 路数组 (硬件), 读 INREG_AI_BASE..+(MCA_COUNT) 时, 越界 (idx>=6) 路返回 0 (MCA 无硬件时行为一致).
- `INREG_ADC485 = 0x87D` 高字节 = `INREG_AI_COUNT`, 已在 backends.rs 写为 `(INREG_AI_COUNT << 8) | 2`, 改完自动适配。

## 3. P1 实施边界

`HOLD_485_*_COMERR/APPERR` 在 `SystemConfig::read_reg` 返回 `recovery::stats().recoverable/degradable/severe` 派生 — RO 行为, 不允许 write_reg (`WriteResult::NotFound` 等价 MCA RO).

## 4. P3 测试边界

`/tmp/host-sync-test` 增:
- `Rcu` 4 写者 × 10000 次 `write` 高频 push 同 retire queue: 确认 `Rcu` 不 panic / 不 double-free.
- `StorageSnapshot` 4 读 + 2 写 (RCU RMW 经 backends::storage_modify) 交错验证 `proto.status` atomic 与 snapshot 镜像一致性.
- `proto_status_set/get` monotone winner 检验.

## 5. 顺序 (并行度)

P1 与 P3 可完全并行 (无文件冲突).
P0 与 P1 有文件冲突 (`system_config.rs` 同时改 read_reg); 由 dev 按顺序串行 edit:
  1. P0 改 `config.rs` (regs 常量 + hw_version)
  2. P1 改 `system_config.rs::read_reg` (485 ERR 分支)
  3. P3 改 `/tmp/host-sync-test` (并行)

## 6. 验收 (D 阶段再审)

1. `cargo check` / `--features f3` / `--features f4` 全 0 errors
2. `cargo test --manifest-path /tmp/host-sync-test/Cargo.toml` 全 0 fails
3. `rg "INREG_AI_COUNT: u16 = 4" src/` 返回空 (硬编码已清除)
4. 审计报告: 由工业软件审计专家回炉, 更新 §3 缺漏清单.
