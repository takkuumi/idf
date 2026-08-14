# Web 持久化与并发实机验证（2026-08-14）

## 环境

- 设备：ESP32-S3 rev 0.2，8MB Flash，2MB PSRAM
- 串口：`/dev/cu.usbserial-1430`
- 固件：`e98902fdc136`，ESP-IDF v5.5.4，debug profile
- 应用分区：factory `0x20000`，大小 `1,684,432B`，槽占用 71.40%
- 以太网：W5500，静态地址 `192.168.51.221`

## 烧录与启动

- 使用 `espflash flash --no-skip` 完整写入 bootloader、partition table 和 factory，
  仅擦除 otadata，未擦除 NVS。
- espflash 自然退出码为 0；重启后 bootloader 完整加载 6 个 app segment，未回退
  ota_0/ota_1。
- Web、NFC、BLE、RTU master/slave、UDP multicast 及 Modbus TCP
  `502/503/504/5002` 均正常启动。

## 配置持久化闭环

1. 快照系统信息、网络标识和三路 RS485 原始值。
2. 写入测试值，等待 DeviceActor 异步 NVS 合并窗口超过 2 秒。
3. 重启设备，确认系统信息、SN、位置、BLE 名称和三路端口参数全部恢复。
4. 写回原始值并等待异步落盘，再完整烧录/启动后复核，无测试数据残留。
5. 静态 IP 启动后的 Web 网络状态保持 `dhcp=0`；修复前会被通用 GOT_IP 事件
   错误改为 `dhcp=1`。

## 并发结果

- 8 条 Modbus TCP 长连接，四端口各 2 条。
- 每条连接连续执行 50 次 FC03，起始地址 `0x0880`、数量 125 words。
- 同时执行 20 次认证 `/getsystemstatus` 请求。
- 结果：Modbus `400/400`、Web `20/20` 成功；事务号、MBAP、功能码、长度和
  250 字节寄存器数据段均通过校验。
- 总耗时 `2.761s`；延迟 min `9.06ms`、avg `51.25ms`、P95 `114.73ms`、
  max `134.18ms`。

## 稳定性遥测

- 持续观察至 300 秒：heap `2054KB`，历史最低 `2018KB`。
- internal SRAM `40KB`，历史最低 `35KB`。
- 7 个受监控任务低栈数为 0，最低剩余栈 `4300B`，最高使用率 61%。
- 压力结束后的空载分钟：TCP 峰值 `830us`，deadline miss 1。
- 未发现 panic、stack canary、pthread 创建失败、W5500/LwIP 错误、服务失联或
  非计划复位。

## 未覆盖边界

- RTU1 未连接真实从站，日志中的 short response 为现场接线条件导致；仅验证两路
  UART 和 master/slave 任务正常启动。
- 本轮没有执行 OTA、手机 BLE 交互或真实 RTU 从站业务闭环，这些项目不能由 TCP
  和 Web 测试替代。
