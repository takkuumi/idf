# BLE 修复总结 (Android 连上后没读到信息)

## 根本原因分析

### Android 端读取流程 (来自 `BLEDataSyncManager.java`):
1. `onStateConnected` → 发送 `READ_HEARTBEAT` (FC=0x11)
2. `onHeartbeatChanged` → 首次连接 → 调用 `readHardwareInfo`
3. `readHardwareInfo` → 发送 `READ_HW_INFO` (FC=0x04, 0x087C)
4. `onHardwareInfoRead` → 发送所有其他读取 (FW, IP, SN, Location, MAC, HW, BT, DI, DO, ADC, RS485, Function)
5. 每个响应的 `onXXXRead` 更新 UI

**关键**: 如果第 1 步的心跳响应不正确,第 2-5 步都不会触发 → 用户看到"信息没有加载"

## 找到的 Bug 和修复

### Bug 1: 心跳响应格式错误 (最关键)
- **问题**: 心跳响应只有 4 字节 ([unit, func=0x11, hb_lo]),且 pdu_data 不包含 func
- **Android 期望**: pdu_data = [unit, func, data...] (length = 2+N, unit+func 在前)
- **修复**: 改为 pdu_data = [unit, 0x11, hb_hi, hb_lo] = 4 字节

### Bug 2: 写响应与通知顺序错误
- **问题**: GATT 协议要求先发写响应, 再发通知。我们之前先调 `try_handle_binary_protocol` (发通知), 再发写响应
- **修复**: 改为先发写响应, 再处理数据发送通知

### Bug 3: 20ms 延迟 (macOS USB dongle 修复, 对 Android 无用)
- **问题**: `send_ble_rsp` 中有 20ms sleep, 对 Android 不需要,反而可能导致问题
- **修复**: 移除 20ms sleep

### Bug 4: 心跳响应过大 (避免 MTU 问题)
- **问题**: 之前 18 字节 MCA 格式 + BLE 帧 = 26 字节, 超过默认 MTU 23
- **修复**: 简化为 4 字节 pdu_data (12 字节 BLE 帧), 完全在默认 MTU 内

## 修改文件
- `src/ble_at/mod.rs`:
  - `try_handle_binary_protocol` 的心跳处理: pdu_data 改为 [unit, 0x11, hb_hi, hb_lo]
  - `WRITE_EVT` 处理: 先发写响应, 再处理数据
  - `send_ble_rsp`: 移除 20ms 延迟

## Android 端期望的完整流程
```
T+0ms:  connect (GATT 连接)
T+10ms: gatt.requestMtu(512)
T+50ms: gatt.discoverServices()
T+200ms: setCharacteristicNotification(true) → CCCD 写 0x0001
T+250ms: writeCharacteristic(heartbeat) → 写 [tx_id, proto_id, length=2, unit=1, func=0x11, crc]
        设备响应: [tx_id, proto_id, length=4, unit, 0x11, hb_hi, hb_lo, crc]
T+260ms: onCharacteristicChanged(heartbeat) → onHeartbeatChanged
T+270ms: readHardwareInfo → 写 READ_HARDWARE_INFO
... 继续所有读取
```

## 关键 BLE 帧格式
```
请求: [tx_id(2)][proto_id(2)][length(2)][unit(1)][func(1)][data(N)][crc(2)]
       length = unit(1) + func(1) + data(N) = 2 + N

响应: [tx_id(2)][proto_id(2)][length(2)][unit(1)][func(1)][data(N)][crc(2)]
       length = unit(1) + func(1) + data(N) = 2 + N
       其中 data 是 Modbus RTU 响应 (byte_count + data bytes), 不含 CRC
```

## MTU 大小
- 默认: 23 字节 (我们的所有响应 < 23 字节)
- 协商后: 500 字节 (Android 请求 512)
