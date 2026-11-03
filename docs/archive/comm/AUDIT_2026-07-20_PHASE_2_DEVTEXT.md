# 审计报告: 阶段 2 — 设备文本 NVS 持久化 (2026-07-20)

- **审计对象**: P0-C 设备文本 (Android 1.0.78 0x1388-0x1B77) NVS 持久化
- **审计范围**:
  - `src/config.rs`: 新增 FUNC_COUNT (0x08FC) + TEXT_META_BASE/COUNT/DATA_BASE 常量
  - `src/device/mod.rs`: NVS_KEY_DEV_TEXT/DEV_TEXT_MAGIC + load/save 函数 + init() 加载
  - `src/bus/backends.rs`: write_hold_reg 写 device_text 后立即调 request_save_device_text
  - `/tmp/host-sync-test/src/write_result_semantics.rs`: 6 个 stage-2 新增测试
- **审计依据**:
  - `docs/comm/PM_GAP_ANALYSIS_2026-07-20.md` §1.2 P0-C
  - `docs/comm/ARCH_PHASE_2026-07-20.md` §3 阶段 2 (精简版)
- **验收手段**:
  - `cargo check` (default / f3 / f4) 全 0 errors
  - `cargo test --target x86_64-apple-darwin --test write_result_semantics` 全 27 tests 通过

## 1. 改动清单

### 1.1 寄存器常量 (`src/config.rs`)

```rust
// ---- 设备功能区 ----
pub const FUNC_COUNT: u16 = 0x08FC;  // 复用 device_config::stored_count

// ---- 设备文本区 ----
pub const TEXT_META_BASE: u16 = 0x1388;  // 文本条目数 (1 reg)
pub const TEXT_META_COUNT: u16 = 2;       // count + total_bytes
pub const TEXT_DATA_BASE: u16 = 0x138A;  // UTF-16LE 数据起始
```

### 1.2 NVS 持久化 (`src/device/mod.rs`)

| 项 | 实现 |
|----|------|
| NVS key | `dev_text` (blob, 4000 bytes) + `dev_text_mag` (u16, magic=0xDE54) |
| 加载 | `load_device_text_from_nvs()` 在 init() 时调 |
| 保存 | `save_device_text_to_nvs()` 在 write_hold_reg(device_text 段) 后调 |
| 触发器 | `request_save_device_text()` 同步保存, 不走 Actor (写频率低) |
| Magic 校验 | 0xDE54, 校验失败回退到空 2000 字 |

### 1.3 写入路径 (`src/bus/backends.rs`)

`write_hold_reg` 中 DEVICE_TEXT_BASE (0x1388) → 0x1B77 (6999) 范围写:
1. RCU RMW 更新 STORAGE.snap.device_text[idx]
2. **NEW**: 调 `crate::device::request_save_device_text()` 立即持久化

### 1.4 测试覆盖 (6 个新增)

| 测试 | 验证 |
|------|-----|
| test_text_layout_meta_at_1388_1389 | 元数据在 0x1388/0x1389, 数据从 0x138A 起 |
| test_text_data_capacity | 数据区 3996 字节 |
| test_text_round_trip_utf16 | "你好世界" UTF-16LE 写入读回正确 |
| test_text_le_encoding | u16 BE/LE 转换正确, NVS LE 与 Modbus BE 链路无误 |
| test_layout_func_count_matches_mca | FUNC_COUNT = 0x08FC |
| test_android_register_addresses_all_defined | 综合验证 Android 期望地址全部已定义 |

## 2. 已知未实施

按 PM 报告 P0-A (Device Function Config TLV) 不在本阶段实施, 因为:
- 0x08FC+ 与 MCA `SLAVE_DEVICE_CONFIG` (2300) 地址重叠, 直接复用 device_config::stored_count 即可
- Android READ_DEVICE_FUNCTION_COUNT (0xB0) 读 0x08FC 返 0, Android 显示"无自定义功能"
- Android 退化为基础 IO 界面, 仍可读写 SN/IP/RS485 等

P0-A 实现需要:
- 设计独立 func_config buffer (避开 0x08FE+ 冲突, e.g. 0x1400-0x1600)
- TLV 编码 (5 种 item 类型: IOControl/RS485/Analog/RS485Analog/RS485Custom)
- 文本引用解析 (与 device_text 联动)

预计工作量: 2-3 周. 暂不实施, 列入阶段 4 (P2).

## 3. Android 业务流支持现状

| Android 命令 | tx_id | 当前状态 | 备注 |
|------------|------|---------|------|
| READ_SN (0x20) | 0x20 | ✅ 完整 | 阶段 1 NVS 持久化 |
| WRITE_SN (0x21) | 0x21 | ✅ 完整 | 阶段 1 Persist |
| READ_LOCATION (0x30) | 0x30 | ✅ 完整 | |
| WRITE_LOCATION (0x31) | 0x31 | ✅ 完整 | |
| READ_MAC (0x40) | 0x40 | ✅ 完整 | |
| READ_BLUETOOTH_ID (0x50) | 0x50 | ✅ 完整 | |
| WRITE_BLUETOOTH_ID (0x51) | 0x51 | ✅ 完整 | 阶段 1 Persist |
| READ_DEVICE_PRODUCT (0x60) | 0x60 | ✅ 完整 | |
| READ_IP (0x70) | 0x70 | ✅ 完整 | |
| WRITE_IP (0x71) | 0x71 | ✅ 完整 | 阶段 1 Apply (网络生效) |
| READ_FW_VERSION (0x80) | 0x80 | ✅ 完整 | |
| READ_HARDWARE_INFO (0x81) | 0x81 | ✅ 完整 | |
| READ_COM_INPUT_IO_STATUS (0x90) | 0x90 | ✅ 完整 | FC=01 @ 0x0000 |
| READ_COM_OUTPUT_IO_STATUS (0x91) | 0x91 | ✅ 完整 | FC=01 @ 0x0200 |
| WRITE_COM_OUTPUT_IO_STATUS (0x92) | 0x92 | ✅ 完整 | FC=05 @ 0x0200+i |
| WRITE_COM_OUTPUT_MULTI_IO_STATUS (0x93) | 0x93 | ✅ 完整 | FC=0F |
| REPORT_COM_INPUT_IO_STATUS (0x94) | 0x94 | ❌ 未实施 | 阶段 3 (P1-B) |
| READ_RS485_*_CONFIG (0xA0/2/4) | 0xA0+ | ✅ 完整 | FC=03 @ 0x08A6/AB/B0 |
| WRITE_RS485_*_CONFIG (0xA1/3/5) | 0xA1+ | ✅ 完整 | FC=10 + 阶段 1 Persist |
| READ_DEVICE_FUNCTION_COUNT (0xB0) | 0xB0 | ⚠️ 部分 | 返 0 (无 func config) |
| WRITE_DEVICE_FUNCTION_COUNT (0xB1) | 0xB1 | ⚠️ 部分 | 写 device_config.stored_count |
| READ_DEVICE_FUNCTION_CONFIG (0xB2) | 0xB2 | ❌ 未实施 | 阶段 4 (P0-A) |
| WRITE_DEVICE_FUNCTION_CONFIG (0xB3) | 0xB3 | ❌ 未实施 | 阶段 4 (P0-A) |
| READ_DEVICE_TEXT_COUNT (0xB4) | 0xB4 | ✅ 完整 | 0x1388+2 |
| WRITE_DEVICE_TEXT_COUNT (0xB5) | 0xB5 | ✅ 完整 | 阶段 2 NVS persist |
| READ_DEVICE_TEXT_DATA (0xB6) | 0xB6 | ✅ 完整 | 0x138A+ |
| WRITE_DEVICE_TEXT_DATA (0xB7) | 0xB7 | ✅ 完整 | 阶段 2 NVS persist |
| READ_CONTROL_ADDRESS (0xB8) | 0xB8 | ✅ 完整 | FC=03 @ 0x0000 |
| WRITE_CONTROL_ADDRESS (0xB9) | 0xB9 | ✅ 完整 | FC=10 |
| MODBUS_COMMAND (0xC0) | 0xC0 | ✅ 完整 | 透传 |
| READ_RS485_EXECUTE_RESULT (0xC1) | 0xC1 | ⚠️ 未使用 | Android 定义但未调用 |
| READ_RS485_INDEX_VALUE (0xD0-DF) | 0xD0+ | ⚠️ 透传 | Android 用 FC=04 @ 地址 |
| READ_RS485_CUSTOM_INDEX_VALUE (0xE0-EF) | 0xE0+ | ⚠️ 透传 | Android 用 FC=03 @ 地址 |

## 4. 审计结论

- ✅ **设备文本 NVS 持久化已建立**, Android WRITE_DEVICE_TEXT_* 命令写入后保留.
- ✅ **寄存器布局 100% 对齐 Android**, 0x1388/0x1389/0x138A 全对齐.
- ✅ **未引入新基础设施** (沿用 STORAGE RCU + 单 blob NVS 模式).
- ⚠️ **P0-A 设备功能配置 TLV 暂未实施**, 列为阶段 4 工作.
- ⚠️ **实机回归待办**: espflash + Android WRITE_DEVICE_TEXT_* 端到端验证.
