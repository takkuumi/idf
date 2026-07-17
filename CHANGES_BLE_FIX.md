# BLE Android 兼容修复总结

## 问题
手持机可以连接 BLE,但读不出设备型号、固件版本、SN、位置、MAC、IP、子网掩码、网关、蓝牙ID、通讯配置、I/O信息、测控执行组态。

## 根本原因
设备的 BLE 响应格式不符合 Android 期望。

**Android 期望格式** (BLE 二进制帧):
```
[tx_id(2 BE)][proto_id(2 BE)][length(2 BE)][pdu_data(N)][crc(2 LE)]
其中 pdu_data = Modbus RTU 响应 (slave + func + body + crc)
```

**设备之前发的格式** (错误):
直接发送 Modbus RTU 字节, 没有 BLE 帧包装, Android 无法解析。

## 修复 (src/ble_at/mod.rs)

### 1. 新增 `send_ble_frame` 函数
将响应包装为 Android 期望的 BLE 帧格式:
```rust
fn send_ble_frame(tx_id: u16, proto_id: u16, pdu_data: &[u8], conn_id: u16) {
    let mut frame = ...;
    frame.extend_from_slice(&tx_id.to_be_bytes());
    frame.extend_from_slice(&proto_id.to_be_bytes());
    let length = pdu_data.len() as u16;
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(pdu_data);
    let crc = modbus_crc16(&frame[..frame.len()]);
    frame.push(crc as u8);
    frame.push((crc >> 8) as u8);
    send_ble_rsp(&frame, conn_id);
}
```

### 2. 修改 `handle_modbus_rtu`
接收 tx_id 和 proto_id 参数, 通过 send_ble_frame 包装响应:
```rust
fn handle_modbus_rtu(frame: &[u8], tx_id: u16, proto_id: u16, conn_id: u16) -> bool {
    ...
    let mut rtu_rsp = vec![slave];
    rtu_rsp.extend_from_slice(&body);
    let crc = modbus_crc16(&rtu_rsp[..rtu_rsp.len()]);
    rtu_rsp.push(crc as u8);
    rtu_rsp.push((crc >> 8) as u8);
    send_ble_frame(tx_id, proto_id, &rtu_rsp, conn_id);
    true
}
```

### 3. 修改 `try_handle_binary_protocol`
从请求中提取 tx_id 和 proto_id, 传递给后续函数:
```rust
let tx_id = u16::from_be_bytes([data[0], data[1]]);
let proto_id = u16::from_be_bytes([data[2], data[3]]);
...
handle_modbus_rtu(pdu, tx_id, proto_id, conn_id)
```

### 4. 添加详细调试日志
在 `try_handle_binary_protocol` 入口打印接收数据, 便于现场排查。

## 验证结果

### 设备寄存器返回 (Modbus TCP, 与 BLE 响应一致):
| 命令 | 地址 | 寄存器数 | 返回数据 |
|------|------|---------|---------|
| SN | 0x0894 | 9 | "ESP32S3-UNKNOWN-00" |
| Location | 0x089D | 8 | "GW-ESP32S3" |
| MAC | 0x08D7 | 6 | 80:B5:4E:5B:24:E7 |
| BT ID | 0x08E2 | 4 | "Mesh" |
| HW Ver | 0x08A5 | 1 | 0x0100 |
| IP | 0x08C7 | 12 | 192.168.51.221 / 255.255.255.0 / 192.168.51.1 |
| FW Ver | 0x087E | 2 | 0x0100 / 0x0615 |
| HW Info | 0x087C | 2 | Q=16, I=16, ADC=4, RS485=2 |

### Android 解析验证 (E2E 模拟测试):
所有 8 个 Android 设备信息命令的 BLE 请求 → 设备处理 → BLE 响应 → Android 解析 完整流程通过。

## 修改文件
- `src/ble_at/mod.rs`: 重构 BLE 响应处理, 添加 send_ble_frame, 修改 handle_modbus_rtu 和 try_handle_binary_protocol, 添加调试日志
- `src/ble_at/mod.rs` (tests): 添加 5 个 BLE 帧格式单元测试

## 烧录
固件已通过 espflash 烧录到 /dev/cu.usbserial-1430, 设备正常运行, BLE 广播激活, Modbus TCP/RTU 工作正常。
