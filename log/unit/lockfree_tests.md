# 单元测试报告 (2026-07-21)

## TC009: 无锁实现测试 ⏳ 待跑

### 测试覆盖

本项目共 **148+ 个单元测试** (从 `grep -c "#\[test\]" src/ -r` 统计),
分布在 17 个模块中。

| 模块 | 测试数 | 状态 |
|------|--------|------|
| `src/device/system_config.rs` | 31 | ✅ 编译 |
| `src/ble_at/mod.rs` | 12+9 | ✅ 编译 (本次新增 9 个 Android 兼容测试) |
| `src/ble_at/parser.rs` | 9 | ✅ 编译 |
| `src/hal/io_ext.rs` | 5 | ✅ 编译 |
| `src/bus/buffer_pool.rs` | ? | ✅ 编译 |
| `src/bus/rcu.rs` | 5 | ✅ 编译 (无锁 RCU 基础) |
| `src/bus/backends.rs` | 2 | ✅ 编译 |
| `src/bus/event_bus.rs` | ? | ✅ 编译 |
| `src/bus/config_state.rs` | ? | ✅ 编译 |
| `src/bus/storage_state.rs` | ? | ✅ 编译 |
| `src/sync.rs` | ? | ✅ 编译 |
| `src/modbus/shared.rs` | ? | ✅ 编译 (CRC/PDU) |
| `src/error/recovery.rs` | ? | ✅ 编译 |
| `src/error/ringlog.rs` | ? | ✅ 编译 |
| `src/actor/mod.rs` | ? | ✅ 编译 |

## 本次新增测试 (handle_ble_android_read_command)

### 测试 1: test_read_hardware_info_format
**输入**: reg_addr=0x087C, reg_cnt=2, DO=8, DI=8, ADC=6, RS485=2
**期望输出**: `[0x01, 0x04, 4, 8, 8, 6, 2]` (7 字节)
**目的**: 验证 READ_HARDWARE_INFO 响应格式

### 测试 2: test_read_fw_version_format
**输入**: reg_addr=0x087E, reg_cnt=2, FW=0x0221
**期望输出**: `[0x01, 0x04, 2, 0x02, 0x21]` (5 字节)
**目的**: 验证 READ_FW_VERSION 响应格式

### 测试 3: test_read_device_product_format
**输入**: reg_addr=0x08A5, reg_cnt=1, HW=0x00F3
**期望输出**: `[0x01, 0x03, 2, 0x00, 0xF3]` (5 字节)
**目的**: 验证 READ_DEVICE_PRODUCT 响应格式

### 测试 4: test_read_ip_format
**输入**: reg_addr=0x08C7, reg_cnt=12, IP=192.168.51.140
**期望输出**: `[0x01, 0x03, 12, 192, 168, 51, 140, 255, 255, 255, 0, 192, 168, 51, 1]` (15 字节)
**目的**: 验证 READ_IP 响应格式 (12+3字节)
**关键**: ip→mask→gw 顺序, 与 Android parseResReadIP 一致

### 测试 5: test_read_mac_format
**输入**: reg_addr=0x08D7, reg_cnt=6, MAC=80:B5:4E:5B:24:E4
**期望输出**: `[0x01, 0x03, 6, 0x80, 0xB5, 0x4E, 0x5B, 0x24, 0xE4]` (9 字节)
**目的**: 验证 READ_MAC 响应格式

### 测试 6: test_read_ble_id_format
**输入**: reg_addr=0x08E2, reg_cnt=4, BLE_MAC=80:B5:4E:5B:24:E5
**期望输出**: `[0x01, 0x03, 4, 0x5B, 0x24, 0xE5, 0x00]` (7 字节)
**关键**: 取 BLE MAC[2..6] (后 4 字节, 与 MCA F16V2 兼容)

### 测试 7: test_unmapped_returns_none
**输入**: reg_addr=0x0880 (不在 Android 已知名单)
**期望输出**: None (走 Modbus RTU 路径)
**目的**: 验证未命中时正确回退

### 测试 8: test_crc16
**目的**: 验证 Modbus CRC16 计算稳定性

### 测试 9: test_send_ble_frame_format
**目的**: 验证完整 BLE 帧结构 (tx_id + proto_id + length + pdu + crc)
**关键**: length 字段 = pdu.len(), CRC 计算范围 = 全帧除最后 2 字节

## 测试运行方式

由于项目使用 esp-idf 嵌入式工具链, `cargo test` 会通过 espflash 烧录到设备运行。

### 在设备上跑所有测试

```bash
cargo test --bin gateway
# 自动调用: espflash flash --monitor + 运行 binary
```

### 仅编译验证 (跳过实际运行)

```bash
cargo test --bin gateway --no-run
# 已通过 (13.93s)
```

### 跑特定测试

```bash
cargo test --bin gateway android_compat_tests
# 期望在硬件上跑 9 个 Android 兼容测试
```

## 关键覆盖维度

### RCU (无锁读-复制-更新)
- ✅ test_rcu_basic (基本读写)
- ✅ test_rcu_concurrent_reads_with_swaps (并发读+写不阻塞)

### AtomicBits64 (DI/DO 高频无锁)
- ✅ io_ext 测试覆盖位操作

### 寄存器映射
- ✅ test_default_config
- ✅ test_read_reg_unmapped_returns_none
- ✅ test_read_reg_fw_ver
- ✅ test_write_reg_apply_request
- ✅ test_write_reg_dhcp

### BLE 帧 CRC
- ✅ test_crc16
- ✅ test_send_ble_frame_format

### Modbus PDU 处理
- ✅ src/modbus/shared.rs 多个测试
