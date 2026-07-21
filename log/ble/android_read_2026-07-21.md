# BLE Android 兼容性测试 (2026-07-21)

## TC006: IP/MAC/BLE_ID 显示修复

### 问题

手持机连接 BLE 设备后, 在设备详情页看不到:
- IP 地址
- 子网掩码
- 网关地址
- 蓝牙 ID

### 根本原因

Android 手持机 (metuory-wireless-management-app-1.0.78) 通过 BLE 发送 Modbus RTU 协议命令 (FC=03/04) 读取设备信息, 期望的响应格式是 **自定义格式** (`[length_byte][data...]`), 不是标准 Modbus RTU 响应。

我们的代码用 `handle_modbus_rtu` 返回 `[slave][func][byte_count=24][regs(24)][crc]`, Android 无法解析。

### 寄存器地址映射 (已对齐)

| 命令 | type | reg_addr | reg_cnt | data 长度 |
|------|------|----------|---------|----------|
| READ_HARDWARE_INFO | 0x81 | 0x087C | 0x0002 | 5 |
| READ_FW_VERSION | 0x80 | 0x087E | 0x0002 | 3 |
| READ_DEVICE_PRODUCT | 0x60 | 0x08A5 | 0x0001 | 3 |
| READ_IP | 0x70 | 0x08C7 | 0x000C | 13 |
| READ_MAC | 0x40 | 0x08D7 | 0x0006 | 7 |
| READ_BLUETOOTH_ID | 0x50 | 0x08E2 | 0x0004 | 5 |

**所有地址在 SystemConfig::read_reg 中已实现**:
- 0x08C7 (IP): HOLD_IP_BASE
- 0x08CB (mask): HOLD_MASK_BASE
- 0x08CF (gw): HOLD_GW_BASE
- 0x08D7 (MAC): HOLD_MAC_BASE
- 0x08E2 (BT_ADDR): HOLD_BT_ADDR_BASE
- 0x08A5 (HW_VER): HOLD_HW_VER

### 响应格式

按 Android `parseRes*` 函数实现:
- `READ_IP`: `[12][ip(4)][mask(4)][gw(4)]` 13 字节
- `READ_MAC`: `[6][mac(6)]` 7 字节
- `READ_BLUETOOTH_ID`: `[4][ble_id(4)]` 5 字节
- `READ_DEVICE_PRODUCT`: `[2][hw_ver(2)]` 3 字节
- `READ_FW_VERSION`: `[2][fw(2)]` 3 字节
- `READ_HARDWARE_INFO`: `[4][DO][DI][ADC][RS485]` 5 字节

### 代码实现

`src/ble_at/mod.rs::handle_ble_android_read_command`:
- 接收 fc=0x03 或 0x04, 解析 reg_addr 和 reg_cnt
- 命中已知地址则按 Android 格式构造响应
- 未命中则返回 false 走 Modbus RTU 路径

### 验证

- 编译: cargo build OK
- 启动: 设备稳定运行 27s+ 无 panic
- 待验证: 需要手持机实际连接测试 (硬件环境限制)

### 测试脚本 (Python 模拟 Android)

```python
# 用 Python BLE 模拟器发送 READ_IP 命令, 验证响应格式
import struct
import crcmod

# 构造 BLE 帧: [tx_id][proto_id][length][unit][func][reg_addr][reg_cnt][crc]
tx_id = 0x0001
proto_id = 0x0000
unit = 0x01
func = 0x03  # READ03
reg_addr = 0x08C7  # IP
reg_cnt = 0x000C  # 12 regs

length = 4 + 4  # unit + func + data
data = struct.pack('>HH', reg_addr, reg_cnt)
pdu = bytes([unit, func]) + data

frame = struct.pack('>HHH', tx_id, proto_id, length) + pdu
crc = modbus_crc16(frame)
frame += struct.pack('<H', crc)

# 期望响应: pdu = [unit][func=0x03][length=12][ip(4)][mask(4)][gw(4)]
```
