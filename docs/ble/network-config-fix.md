# BLE 手持机网络配置修复

## 调查结论

对照 `MCA_F16V2_1_F48_BLE/BleMsgDeal.cpp`、`modbus_slave.h` 和 Android
`metuory-wireless-management-app-1.0.78` 源码后，确认协议中有两种字段顺序：

- 手持机 READ_IP 响应按 `IP → 掩码 → 网关` 解析。
- Android `DeviceFragment` 的实际 FC16 请求按 `IP → 掩码 → 网关` 发送；
  `sendWriteIPCMD` 的 Java 形参名称与调用实参相反，不能据形参名判断线序。
- 标准 holding register 布局为 IP 2247–2250、掩码 2251–2254、网关
  2255–2258。
- 旧 MCA 自定义 0xCD/0xCE 载荷使用 `IP → 掩码 → 网关`。

之前多个注释和本地修复报告把 READ_IP 顺序描述成 `IP → 网关 → 掩码`，
而 Android 写请求也被误当成寄存器的自然顺序，导致读出显示错位，写入后掩码
与网关被互换。另一个问题是常规 Apply 只重配运行中的 netif，并不复位设备；
手持机网络写入需持久化后复位以使配置完整生效。

## 修复

- 修正网络寄存器常量，使掩码为 2251、网关为 2255，并同步输入寄存器映射。
- BLE 手持机 FC16 写入按真实 Android 线序直接写入寄存器；拒绝超出 0–255
  的 octet 值，避免再次交换掩码和网关。
- FC16 成功后同步写入 NVS，只有持久化成功才排队请求设备复位。
- READ_IP 与旧式 0xCE 响应统一返回 `IP → 掩码 → 网关`；旧式 0xCD 按参考
  固件的同一顺序写入。
- 保留 Modbus TCP/RTU 标准寄存器顺序，不对其他总线入口做 Android 专用转换。

## 验证状态

- `cargo test --bin gateway --no-run`：通过，测试目标编译成功。
- `cargo fmt --check`：仓库还有 `src/modbus/rtu_runtime.rs` 和
  `src/modbus/tcp_server.rs` 的既存格式差异；本次涉及的三个 Rust 文件已单独
  rustfmt。
- 串口启动回归和真实设备 Modbus 寄存器读回已完成；Android 手持机 BLE 写入
  仍需在客户端连接后做一次端到端确认，记录见
  [`log/ble/network_config_2026-09-23.md`](../../log/ble/network_config_2026-09-23.md)。
