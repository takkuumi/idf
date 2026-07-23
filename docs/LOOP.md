# 系统持续开发集成 (LOOP.md)

> 最后更新: 2026-07-23 (LOOP4: metuory 11 条读路径 length-prefix 格式修复)
> 详细进度: `log/SUMMARY_2026-07-22.md`

## 项目背景

此系统是开发一款基于ESP-IDF的 工业控制系统。
原有一套C++开发的系统（MCA_F16V2_1_F48_BLE），运行不稳定，现基于 rust + esp-idf 重构。

- ESP-IDF 源码: `/Users/takumi/Workspace/esp-idf` (禁止修改)
- 原 C++ 系统: `/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE` (禁止修改)
- 手持机源码: `/Users/takumi/Workspace/metuory-wireless-management-app-1.0.78` (禁止修改)

## 系统迭代 - 5 角色

| 角色 | 职责 |
|------|------|
| 产品经理 | 对照 MCA_F16V2_1_F48_BLE + metuory-wireless-management-app-1.0.78 提出缺失功能 |
| 高级 Rust 开发 | 实施功能与修复 BUG |
| 高级测试 | 测试 + 提出问题 |
| 高级系统架构 | 架构把关 (`docs/ARCHITECTURE.md`) |
| 工业软件审计 | 审计每次实施 |

## 任务完成清单

| # | 任务 | 状态 | 关键产出 |
|---|------|------|----------|
| 1 | heapless 升级 0.9.3 | ✅ | `Cargo.toml` |
| 2 | 手持机显示 IP/MAC/BLE_ID | ✅ | `handle_ble_android_read_command` (LOOP2) |
| 3 | 硬件信息确认 | ✅ | `docs/pinmap.md` 重写 (ESP32-S3R2) |
| 4 | 无锁测试 + 栈估算 | ✅ | 127 测试 + 架构合并消除栈风险 |
| 5 | Modbus TCP 完整测试 | 🟡 | FC=03/04 通过; FC=06/10 写响应丢失 (后续) |
| 6 | log/ 目录 + 详细日志 | ✅ | 7 子目录 + SUMMARY |
| 7 | mesh 清理 | ✅ | 死代码已删 |
| 8 | 引脚核对 | ✅ | pinmap.md 1:1 对齐 |
| 9 | 性能测试 | ✅ | Modbus TCP 11s 全部响应 |
| 10 | 7×24 不间断运行 | 🟡 | 1 小时长稳测试中 |
| 11 | 5 角色协作 | ✅ | 完整推进 |
| 12 | **BLE ID 写入 0x08E2 路由修复** | ✅ (LOOP3) | commit `d9654f7` |
| 13 | **MBAP.length 兼容 pymodbus** | ✅ (LOOP3) | commit `22a6e0a` |

## LOOP3 关键修复 (2026-07-23)

### 问题 1: BLE ID 写入 0x08E2 被错误路由到 BLE MAC

**根因**:
- metuory 1.0.78 WRITE_BLUETOOTH_ID (0x51) 通过 BLE Modbus FC=10 写 `0x08E2` (4 寄存器)
- 期望更新 `cfg.ble_name` (蓝牙 ID 显示字段)
- 旧代码 `HOLD_BT_ADDR_BASE = 0x08E2` 把 0x08E2 路由到 `cfg.ble_mac`
- 写入被错误地修改 BLE MAC, 而 `ble_name` 永远不变
- 读取看似正常, 是因为 `handle_ble_android_read_command` 自定义读 handler 直接返回 `cfg.ble_name`

**修复** (`d9654f7`):
- `HOLD_BLE_NAME_BASE: 4000 → 0x08E2` (metuory 期望地址)
- `HOLD_BT_ADDR_BASE: 2274 → 0x0FA4` (用户区 4004, 保留兼容)
- `read_reg/write_reg`: BLE_NAME 优先 BLE_MAC 检查
- BLE_NAME 写返回 `Persist` (而非 Apply)
- 6 个新增回归测试: `test_ble_name_at_metuory_addr_is_persist` 等

### 问题 2: MBAP.length 响应字段不包含 unit_id, pymodbus 3.x 解析失败

**根因**:
- Modbus TCP 标准 (Modbus_Application_Protocol_V1_1b3 §4.1) 规定:
  `MBAP.length = unit_id(1) + func(1) + data(N) = 2 + N`
- 旧实现 `mbap_len = resp_pdu.len() (= 1+N)` 导致 pymodbus 3.8.6 解析时
  把 func 误认为 unit_id, 报错 `Unable to decode frame: byte_count N > length of packet N`

**修复** (`22a6e0a`):
- `src/modbus/tcp_server.rs: mbap_len = 1 + resp_pdu.len()`
- pymodbus 3.8.6 验证: 5 个 RS485 寄存器读正确 (0x3000, 1, 0, 1000, 20)

## LOOP4 metuory 读路径 length-prefix 格式 (2026-07-23)

### 背景
metuory Android 端所有 `parseXxxItem` 使用 length-prefix 格式:
```
buffer[0] = length
data[1..1+length] = 实际数据
```
但本系统 FC=03/04 走标准 Modbus 路径返回 `[func][byte_count][data]` 格式
导致 metuory 解析失败 → UI 显示空/异常

### 修复 (commit a10a9ef, src/ble_at/mod.rs)
在 `handle_ble_android_read_command` 新增 5 个读 handler:

| 命令 | 地址 | FC | 响应格式 |
|------|------|-----|----------|
| READ_SN         | 0x0894, 9 | 0x03 | [18][SN bytes] |
| READ_LOCATION   | 0x089D, 8 | 0x03 | [16][location bytes] |
| READ_ADC        | 0x0080, n | 0x04 | [2n][BE u16 × n] |
| READ_COM_INPUT  | 0x0000, n | 0x02 | [n/8 bytes][packed bits] |
| READ_COM_OUTPUT | 0x0200, n | 0x01 | [n/8 bytes][packed bits] |
| READ_RS485_VALUE       | 0x1000+, n | 0x04 | [2n][BE u16 × n] |
| READ_RS485_CUSTOM_VALUE| 0x1000+, n | 0x03 | [2n][BE u16 × n] |

### 端到端验证 (11/11 全部通过)
- SN='ESP32S3-UNKNOWN-00' (18 bytes ASCII)
- LOCATION='GW-ESP32S3' (16 bytes ASCII)
- BT_ID='Mesh' (8 bytes ASCII)
- MAC=80:B5:4E:5B:24:E7
- IP=192.168.51.140 / 255.255.255.0 / 192.168.51.1
- FW=00DD0615 (2.2.1.1557) HW=10100402 (F16/16DI/16DO/4AI/2RS485)
- ADC=4 通道, COM I/O=16 位, RS485 idx0=5 寄存器

### 新增测试 (5 个回归测试 + 5 个 test helper)
- test_android_parse_sn_length_prefix
- test_android_parse_location_length_prefix
- test_android_parse_adc_length_prefix
- test_android_parse_com_input_length_prefix
- test_android_parse_rs485_value_length_prefix

### 已知问题 (与本 LOOP 独立)
Modbus TCP 写响应连接重置 (READ 路径正常, 写成功后响应未送达)
源: TCP 服务器 write_all/flush 失败
影响: metuory 写入后无回执, 但数据已落 RCU → 下次读仍能看到新值
LOOP5 待排查 W5500/lwIP + std::net::TcpStream 在 RCU RMW 后的 write 行为

## 待解决问题

### BLE 写入 (metuory → 0x08E2) 仍需端到端验证
- 代码修复 ✅, 单元测试 ✅, BLE 路径需手持机实测
- metuory 写入流程: WRITE_BLUETOOTH_ID (0x51) → BLE Modbus FC=10 → `try_handle_binary_protocol` →
  `handle_modbus_rtu` → `write_multi_regs_pdu` → `backends::write_hold_reg(0x08E2+i, v)`
- 修复后: 写入 `cfg.ble_name[0..8]` (BE), 返回 `Persist`, actor 异步落盘 NVS

## 架构 (Phase 2 完成)

```
main_loop (100ms tick)
├── tick_ai_sample(&hal)        # 100ms, 合并 ai-sample pthread
├── tick_ao_output(&hal)         # 100ms, 合并 ao-output pthread
├── tick_di_scan(&hal)           # 20ms (5 分频), 合并 di-scan pthread
├── tick_do_output(&hal)         # 100ms, 合并 do-output pthread (notify 立即触发)
└── tick_eth_heartbeat()         # 5s (50 分频), 合并 eth-heartbeat pthread

4 个保留 pthread 任务:
- DeviceActor (NVS 持久化, 8KB 栈)
- mb-rtu-master (Modbus RTU 主站)
- mb-rtu-slave (Modbus RTU 从站)
- mb-tcp-listen (Modbus TCP)
```

详细架构: [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)

## 注意事项

- 烧录: `espflash`, 串口 `/dev/cu.usbserial-1430`, 强制重置
- 禁止修改 esp-idf / MCA / metuory 源码
- 严禁抄袭 MCA/metuory 代码 (只参考业务)
- 所有决策需要我审批时 (用户睡觉中) 自动处理

## 烧录

```bash
cargo build && espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
```

详细: [`docs/FLASH.md`](FLASH.md)

## 测试日志

`log/` 目录:
- `log/README.md` - 测试矩阵
