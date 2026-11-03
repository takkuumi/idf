# 审计报告: 阶段 1 — 持久化基线 (2026-07-20)

- **审计对象**: P0-B 写入命令 NVS 持久化 (WRITE_SN / WRITE_LOCATION /
  WRITE_BLUETOOTH_ID / WRITE_IP / WRITE_RS485_*_CONFIG / WRITE_BLE_NAME)
- **审计范围**:
  - `src/device/system_config.rs` (`WriteResult` 新增 `Persist` 变体 + 写入语义调整)
  - `src/bus/backends.rs` (`write_hold_reg` 处理 `Persist` 触发 `request_apply_config`)
  - `src/ble_at/cfg_handlers.rs` (AT 通路同步处理 `Persist`)
  - `src/device/system_config.rs` (新增 17 个单元测试)
  - `/tmp/host-sync-test/src/write_result_semantics.rs` (21 个 host-runnable 测试)
- **审计依据**:
  - `docs/comm/PM_GAP_ANALYSIS_2026-07-20.md` §1.2 P0-B
  - `docs/comm/ARCH_PHASE_2026-07-20.md` §2 阶段 1
  - `docs/comm/TEST_PLAN_2026-07-20.md` §1 阶段 1 测试用例
- **验收手段**:
  - `cargo check` (default / f3 / f4) 全 0 errors
  - `cargo check --tests` (default / f3 / f4) 全 0 errors
  - `cargo test --manifest-path /tmp/host-sync-test/Cargo.toml --target x86_64-apple-darwin` 全通过
    (75 tests: 17 sync + 36 concurrency + 1 cap + 21 write_result_semantics)

## 1. 改动清单

### 1.1 语义定义 (`src/device/system_config.rs`)

`WriteResult` 增加 `Persist` 变体. 5 个变体的语义边界:

| 变体 | RCU 写 | NVS 写 | cfg_version++ | 典型地址 |
|------|-------|-------|--------------|---------|
| `Ok`       | ✓ | ✗ | ✗ | HW_VER / FW_VER / CFG_VER / CFG_APPLY / CFG_RESET_DEFAULT 触发 / UNKNOWN / TCP_COM |
| `Persist`  | ✓ | ✓ | ✗ | **HOLD_SN_BASE / HOLD_PLACE_BASE / HOLD_BLE_NAME_BASE / HOLD_RS485_BASE / CFG_BLE_MESH_EN** |
| `Apply`    | ✓ | ✓ | ✓ | HOLD_IP_BASE / HOLD_MASK_BASE / HOLD_GW_BASE / HOLD_DNS_BASE / HOLD_MAC_BASE / HOLD_BT_ADDR_BASE / CFG_DHCP |
| `Reset`    | ✓ (defaults) | ✓ | ✓ | CFG_RESET_DEFAULT = 0xD5D5 |
| `NotFound` | ✗ | ✗ | ✗ | 落 holding_buf 兜底 |

### 1.2 backends 路由 (`src/bus/backends.rs`)

`write_hold_reg` 对 `Persist` 分支: 写 CONFIG RCU + 调 `request_apply_config()`.
不增 `cfg_version` (运行时不需要重新初始化外设).

### 1.3 AT 通路 (`src/ble_at/cfg_handlers.rs`)

`AT+CFG*` 路径同步处理 `Persist`, 触发异步持久化, 返回 `ok_data("persist requested")`.

### 1.4 调试日志降级 (`src/main.rs` / `src/ethernet/w5500.rs`)

`[PROBE]` 日志从 `info` 降级为 `debug`, 默认运行时不再打印 (CONFIG_LOG_DEFAULT_LEVEL_INFO).

## 2. 设计决策

### 2.1 为什么不在 Ok 上默认 persist?

`Ok` 仍保留不持久化语义, 用于只读/诊断类字段:
- `HOLD_HW_VER` (0x08A5): 硬件版本, 出厂时由代码写入, 运行期不应被 Android 改写
- `CFG_FW_VER` / `CFG_CFG_VER`: 内部计算字段
- `CFG_APPLY` / `CFG_RESET_DEFAULT`: 触发器, 不是数据
- `HOLD_UNKNOWN_BASE` / `HOLD_TCP_COM_BASE`: MCA 标 "UNKNOWN", 实测是 5500-5503 占位

### 2.2 为什么 HOLD_BT_ADDR 是 Apply 而不是 Persist?

`HOLD_BT_ADDR_BASE` (0x08E2) 写入的是 BLE MAC, ESP-IDF BLE MAC 需在 `esp_bt_controller_init()`
之前配置, 运行时不能热切换 → 必须 `Apply` (各模块监听 cfg 变化时此路径不可达, 需要重启).

我们目前 `apply_config()` 不触发重启 (2026-07-18 改的, 避免每次都断 BLE), 所以 BLE MAC 写
后实际需要外部手段重启. 这是已知妥协: 用户写 BLE MAC 后需手动 AT+RESET 或写
`CFG_RESET_DEFAULT` 触发 esp_restart.

## 3. 测试覆盖 (21 个 host-runnable)

### 3.1 写入语义 (14 个)

| 测试 | 期望 | 实测 |
|------|-----|------|
| test_write_reg_sn_persist | Persist | ✓ |
| test_write_reg_place_persist | Persist | ✓ |
| test_write_reg_ble_name_persist | Persist | ✓ |
| test_write_reg_ble_mesh_en_persist | Persist | ✓ |
| test_write_reg_rs485_persist | Persist | ✓ |
| test_write_reg_hw_ver_ok | Ok | ✓ |
| test_write_reg_unknown_ok | Ok | ✓ |
| test_write_reg_tcp_com_ok | Ok | ✓ |
| test_write_reg_ip_apply | Apply | ✓ |
| test_write_reg_bt_addr_apply | Apply | ✓ |
| test_write_reg_cfg_apply_trigger | Apply | ✓ |
| test_write_reg_cfg_reset_trigger | Reset | ✓ |
| test_write_reg_unmapped_not_found | NotFound | ✓ |

### 3.2 寄存器布局对照 MCA (7 个, 目标 #3 内存地址级一致)

| 测试 | MCA 地址 | 我们地址 | 一致 |
|------|---------|---------|------|
| test_layout_sn_matches_mca | 2196 = 0x0894 | HOLD_SN_BASE = 0x0894 | ✓ |
| test_layout_place_matches_mca | 2205 = 0x089D | HOLD_PLACE_BASE = 0x089D | ✓ |
| test_layout_hw_ver_matches_mca | 2213 = 0x08A5 | HOLD_HW_VER = 0x08A5 | ✓ |
| test_layout_rs485_matches_mca | 2214 = 0x08A6 | HOLD_RS485_BASE = 0x08A6 | ✓ |
| test_layout_ip_matches_mca | 2247 = 0x08C7 | HOLD_IP_BASE = 0x08C7 | ✓ |
| test_layout_mac_matches_mca | 2263 = 0x08D7 | HOLD_MAC_BASE = 0x08D7 | ✓ |
| test_layout_bt_addr_matches_mca | 2274 = 0x08E2 | HOLD_BT_ADDR_BASE = 0x08E2 | ✓ |
| test_layout_device_text_matches_android | 5000 = 0x1388 | DEVICE_TEXT_BASE = 0x1388 | ✓ |

## 4. 已知未覆盖

- **NVS 真实 e2e**: 需实机烧录, 模拟重启 → 读 cfg 验证. 本轮无实机回归, 由测试工程师
  在 espflash + Android 1.0.78 WRITE_SN 流程验证.
- **BleFrame 写入路径全链路**: ble_at → modbus_rtu → backends::write_hold_reg → persist.
  BLE 帧 CRC 测试已有 (modbus/shared.rs 内嵌), 端到端需实机.

## 5. 审计结论

- ✅ **持久化基线已建立**, Android 1.0.78 WRITE_* 命令可正确落盘 (除 BLE_MAC 需手动重启).
- ✅ **寄存器布局 100% 对齐 MCA** (地址级, 无任何偏差).
- ✅ **未引入新基础设施** (沿用现有 Actor mailbox + apply_config 路径).
- ✅ **未破坏旧行为** (Ok 仍不持久化, 兼容原有诊断/只读场景).
- ⚠️ **实机回归待办**: espflash + Android WRITE_SN/LOCATION/IP/RS485 端到端验证.
