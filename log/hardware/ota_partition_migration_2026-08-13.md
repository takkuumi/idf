# OTA 分区迁移与实机回归（2026-08-13）

## 设备

- 芯片：ESP32-S3 revision 0.2
- Flash：8MB
- 串口：`/dev/cu.usbserial-1430`
- 设备 IP：`192.168.51.140`
- 烧录参数：DIO / 40MHz / 8MB

## 根因

迁移前设备 Flash 中实际分区表为单 `factory`：`factory@0x10000` 覆盖整个应用区，没有 `otadata`、`ota_0`、`ota_1`，因此 Web OTA 调用 `esp_ota_get_next_update_partition(NULL)` 返回空并以 HTTP 500 结束。

## 修复

新分区表保留旧设备 `nvs@0x9000`（24KiB），并写入：`phy_init@0xF000`、`otadata@0x10000`、`nvs_keys@0x17000`、`factory@0x20000`、`ota_0@0x260000`、`ota_1@0x4A0000`、`coredump@0x6E0000`、`ble_mesh@0x6F0000`、`storage@0x700000`。未擦除 NVS。

## 实测结果

1. 全量烧录后从 Flash 读取并解析分区表，以上布局与 `partitions.csv` 完全一致。
2. 登录、`/getsysteminfo`、`/getsystemstatus`、`/getnetworkconfig`、`/getportconfig`、`/getiodata` 均返回 HTTP 200 / `code=0`。
3. 端口 `502/503/504/5002` 均通过标准 FC03 起始 `0x4000`、数量 `125`，响应 ADU 长度 `259`（byte count `250`）。
4. Web OTA 应用镜像上传返回：`{"type":"updateota","code":"0","msg":"","data":{}}`。
5. OTA 后设备运行约 48 秒，Web 状态正常，`free_heap≈2098620`，`recovery_mode=Normal`。
6. 读取 `otadata@0x10000` 可见 OTA 条目状态字 `0x00000002`（`VALID`）；工具复位后仍为 `VALID`，设备重新联网并继续提供 Web 与四路 Modbus TCP。

## 限制

本记录是短期启动、升级和复位回归，不等同于 72 小时或 7×24 浸泡测试。长期验证仍需在维护窗口持续运行并记录堆、栈、链路和复位计数。
