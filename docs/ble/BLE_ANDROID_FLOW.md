# Android BLE 连接后数据获取流程分析

## Android 端连接后的命令序列

根据 `metuory-wireless-management-app-1.0.78` 源码分析:

### 1. BLE 连接成功 (`BLEDataSyncManager.onStateConnected`)
```java
sendReadHeartbeatCMDStatusHandle(model);  // 发送心跳
```

### 2. 心跳响应 (`onHeartbeatChanged`) - 关键路径
```java
first = addDeviceDataSync(mac, model);  // 首次连接返回 true
if (first) {
    readHardwareInfo(model);  // 首次连接 → 读硬件信息
}
```

### 3. 硬件信息响应 (`onHardwareInfoRead`)
触发所有其他读取:
- FW Version, IP, SN, Location, MAC, Product (HW), BT ID
- DI, DO, ADC
- RS485-1/2/3 配置
- Device Function Count

## 关键 Bug: 心跳响应格式错误

### Android 端期望 (parseHeartbeat):
```java
item.setSlaveAddress(buffer[0]);   // data[0] = slave
item.setRunStatus(buffer[1]);      // data[1] = runStatus
item.setTerminal(buffer[2..]);      // data[2..] = terminal (16 bytes)
```

### MCA 的心跳响应 (DealHeartBeat):
```c
HeartBeatbuf[0..1] = HeartBeat_SN (u16)
HeartBeatbuf[2..5] = 0
HeartBeatbuf[6] = 0x01 (unit)
HeartBeatbuf[7] = 0x11 (func)
HeartBeatbuf[8] = 0x07
HeartBeatbuf[9] = 0xFF
HeartBeatbuf[10..15] = Esp32ChipID (6 bytes)
HeartBeatbuf[16..17] = CRC
// 总计 18 字节
```

### 我们之前的错误 (修复前):
```rust
// 错误: 使用 Modbus RTU 格式包装心跳响应
let mut rsp = [unit, func=0x11, hb_hi, hb_lo];  // 只有 4 字节
// 加 CRC → 6 字节 Modbus RTU
// 包装 BLE 帧 → 12 字节
```

但 Android 期望 18 字节的 MCA 自定义格式 (包含 HeartBeat_SN、unit、func、chip_id 等)。

### 修复后:
```rust
// 正确: 使用 MCA 原始格式 (18 字节)
let mut rsp = [
    HeartBeat_SN:2,  // 递增序列号
    0,0,0,0,         // 4 字节 0
    unit,             // 单元 ID
    0x11,             // func
    0x07, 0xFF,       // 状态标志
    chip_id:6,        // BLE MAC 后 6 字节
    CRC:2             // Modbus CRC16
];
```

## 修复后的同步流程

| 步骤 | Android 命令 | 设备响应 | 状态 |
|------|--------------|----------|------|
| 1. Connect | - | BLE 广播激活 | ✓ |
| 2. Heartbeat (FC=0x11) | READ_HEARTBEAT | 18 字节 MCA 格式 | ✓ 修复 |
| 3. Hardware Info (FC=0x04, 0x087C) | READ_HW_INFO | QI/ADC count | ✓ |
| 4. FW Version (FC=0x04, 0x087E) | READ_FW_VERSION | FW=0x00DD, Date=0x0615 | ✓ |
| 5. IP (FC=0x03, 0x08C7) | READ_IP | IP+Mask+GW | ✓ |
| 6. SN (FC=0x03, 0x0894) | READ_SN | "ESP32S3-UNKNOWN-00" | ✓ |
| 7. Location (FC=0x03, 0x089D) | READ_LOCATION | "GW-ESP32S3" | ✓ |
| 8. MAC (FC=0x03, 0x08D7) | READ_MAC | 80:B5:4E:5B:24:E7 | ✓ |
| 9. HW Version (FC=0x03, 0x08A5) | READ_PRODUCT | 0x0100 | ✓ |
| 10. BT ID (FC=0x03, 0x08E2) | READ_BLUETOOTH_ID | "Mesh" | ✓ |
| 11. DI (FC=0x01, 0x0000) | READ_COM_INPUT_IO | DI bits | ✓ |
| 12. DO (FC=0x01, 0x0200) | READ_COM_OUTPUT_IO | DO bits | ✓ |
| 13. ADC (FC=0x04, 0x0080) | READ_ADC | AI values | ✓ |
| 14. RS485-1/2/3 (FC=0x03) | READ_RS485_N_CONFIG | 5 words each | ✓ |
| 15. Function Count (FC=0x03, 0x08FC) | READ_DEVICE_FUNCTION_COUNT | 200 bytes | ✓ |

## 为什么之前安卓连上后没读到信息

**根因**: 心跳响应格式错误 (4 字节 Modbus RTU 而不是 18 字节 MCA 格式)
- Android 解析心跳时, 看到 buffer=[byte_count=4, hb_hi, hb_lo, crc_lo, crc_hi]
- 错误地把 byte_count=4 当作 slave, hb_hi 当作 runStatus
- terminal 只有 3 字节 (应该是 16 字节)
- 触发 `onHeartbeatChanged` 失败或解析异常
- 因此不调用 `readHardwareInfo`
- 所有后续读取都不会触发
- 用户看到"信息没有加载"

## 修改文件
- `src/ble_at/mod.rs`: 修改心跳响应为 MCA 原始 18 字节格式
- `src/device/mod.rs`: 不再触发设备重启 (修复 BLE 断开问题)
- `src/bus.rs`: `read_coil` 在 0x0000-0x001F 别名读取 DI
