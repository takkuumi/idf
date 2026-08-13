# PC 配置协议实机回归 (2026-08-13)

## Modbus TCP

设备 `192.168.51.140` 的 `502/503/504/5002` 四个端口均通过:

- FC03 读取 83-word 端口设置区 `2196..2278`。
- FC04 读取硬件元数据、AI 输入；FC01 读取 X/Y 位区。
- 逻辑区 `2300` 读取 224 words，文本区 `5000` 读取 639 words。
- `4222=0x55AA` 解锁后，83 words 按 `60+23` 写回并严格一致。
- Y0 同值 FC05 写回不改变输出状态。
- 标准 FC03 最大数量 125 可正常工作，不再受 60-word 限制。

最终输出: `ALL PC MODBUS TCP COMPATIBILITY CHECKS PASSED`。Web OTA 后再次执行，结果相同。

## Modbus RTU 限制

固件已启动 UART1 主站与 UART2 从站；UART2 参数为 TX GPIO42、RX GPIO41、DE GPIO8、slave id 1、9600 8N1。当前电脑只有 `/dev/cu.usbserial-1430` 烧录/日志串口，没有独立 USB-RS485 适配器，因此未完成 PC 工具 RTU 实物闭环，不能声称 RTU 已实机验证。
