# 手持机 BLE 协议实机回归 (2026-08-13)

## 环境

- 设备: ESP32-S3 rev 0.2, BLE MAC `80:B5:4E:5B:24:E6`
- 广播名: `Mesh001`, macOS CoreBluetooth 显示目标 `GW-C5`
- Service: `4FAFC201-1FB5-459E-8FCC-C5C9C331914B`
- Characteristic: `BEB5483E-36E1-4688-B7F5-EA07361B26A8`

## 结果

实机读取 18 项全部通过:

- 连接主动心跳和请求心跳均返回 `slave=1, runStatus=1, terminal=BLE_MAC[6]`。
- 硬件信息: DO=16, DI=16, AI=4, RS485=2。
- 固件、SN、位置、产品类型、MAC、完整 BLE 名称窗口和 IPv4 参数正确。
- DI、DO、AI、三路 RS485、功能数量和文本元数据读取正确。

最终输出: `ALL BLE HANDHELD READ CHECKS PASSED count=18`。Web OTA 后再次执行，结果相同。

## 结论

Android `parseHeartbeat()` 所需的完整 10-byte PDU 已覆盖主动、周期和请求应答三条路径，连接后的首次属性同步不再被短心跳阻断。
