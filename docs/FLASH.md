# 烧录与 OTA 指南（ESP32-S3 工业网关）

> 最后更新：2026-08-13
>
> 已验证硬件：ESP32-S3、8MB Flash、2MB PSRAM
>
> 默认串口：`/dev/cu.usbserial-1430`

## 1. 固定 Flash 参数

真实设备已验证以下参数，烧录工具不得覆盖为 QIO/80MHz：

- Flash mode：DIO
- Flash frequency：40MHz
- Flash size：8MB
- Bootloader：`0x0000`
- Partition table：`0x8000`
- 首次串口烧录目标：`factory`

QIO/80MHz 会使当前硬件在二级 Bootloader 加载阶段触发
`ets_loader.c:78` / `TG0WDT_SYS_RST` 循环复位。

## 2. 生产分区布局

| 分区 | Offset | Size | 用途 |
|---|---:|---:|---|
| nvs | `0x9000` | `0x6000` | 系统配置（兼容旧设备原地址） |
| phy_init | `0xF000` | `0x1000` | RF 校准 |
| otadata | `0x10000` | `0x2000` | OTA 选择与回滚状态 |
| nvs_keys | `0x17000` | `0x1000` | NVS 密钥预留 |
| factory | `0x20000` | `0x240000` | 串口烧录应用 |
| ota_0 | `0x260000` | `0x240000` | OTA 槽 0 |
| ota_1 | `0x4A0000` | `0x240000` | OTA 槽 1 |
| coredump | `0x6E0000` | `0x10000` | 崩溃转储 |
| ble_mesh | `0x6F0000` | `0x10000` | BLE Mesh NVS |
| storage | `0x700000` | `0x100000` | 文件存储 |

分区源文件为项目根目录的 `partitions.csv`。禁止只依赖设备上遗留的分区表；旧设备可能仍是单 factory 布局，在该布局下 OTA 必然失败。迁移时保留 `nvs@0x9000`，不得全擦，否则会丢失业务配置。

## 3. 标准完整烧录

推荐使用项目命令：

```bash
just flash
```

等价的非交互命令如下：

```bash
cargo build --bin gateway

espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    --bootloader target/xtensa-esp32s3-espidf/debug/bootloader.bin \
    --partition-table partitions.csv \
    --partition-table-offset 0x8000 \
    --target-app-partition factory \
    --erase-parts otadata \
    --flash-mode dio \
    --flash-freq 40mhz \
    --flash-size 8mb \
    target/xtensa-esp32s3-espidf/debug/gateway
```

`--erase-parts otadata` 只清除 OTA 启动选择，使本次串口写入的 factory 镜像立即生效；不会清除 NVS 业务配置。

配置或 Flash 参数变化后，先清理 ESP-IDF 生成缓存：

```bash
rm -rf target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-*
cargo build --bin gateway
```

## 4. 全擦烧录

仅在 Flash 布局迁移或存储损坏时使用：

```bash
just full-flash
```

全擦会不可恢复地删除 NVS 配置、BLE Mesh 数据、OTA 状态和存储文件。执行前必须备份需要保留的数据。普通升级不要全擦。

## 5. 串口监控

```bash
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

- `Ctrl+R`：复位芯片并查看完整启动日志。
- `Ctrl+C`：退出监控并释放串口。
- 烧录前必须退出所有串口监控进程，否则可能出现 `Failed to connect to the device`。

若连接失败，先确认端口占用：

```bash
lsof /dev/cu.usbserial-1430
espflash list-ports
```

必要时手动进入下载模式：按住 BOOT，短按 RST/EN，再松开 BOOT。

## 6. Web OTA 测试

生成“仅应用”OTA 镜像，禁止使用包含 Bootloader/分区表的 merge 镜像：

```bash
cargo build --bin gateway
espflash save-image --chip esp32s3 \
    --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
    target/xtensa-esp32s3-espidf/debug/gateway \
    /tmp/gateway-ota.bin
```

登录 Web 系统维护页上传 `/tmp/gateway-ota.bin`，或者直接调用：

```bash
curl --fail --show-error \
    -H 'Cookie: ESPSESSIONID=<登录返回的会话>' \
    -H 'Content-Type: application/octet-stream' \
    --data-binary @/tmp/gateway-ota.bin \
    http://<设备IP>/updateota
```

成功响应：

```json
{"type":"updateota","code":"0","msg":"","data":{}}
```

完整 OTA 验证必须覆盖：

1. 上传后设备切换到非当前 OTA 槽并重启。
2. 新镜像启动状态为 `PENDING_VERIFY`。
3. 固件稳定运行 30 秒后调用 ESP-IDF 接口确认镜像为 `VALID`。
4. 再次复位仍从同一 OTA 槽启动，不能回滚到旧镜像。
5. Web、TCP 502/503/504/5002、BLE、RTU 和 IO 状态在升级后继续可用。

## 7. 启动验收

启动日志至少应确认：

```text
Boot SPI Speed : 40MHz
SPI Mode       : DIO
SPI Flash Size : 8MB
```

同时检查无以下异常：

- `Guru Meditation Error`
- `Stack canary watchpoint triggered`
- `Failed to create task`
- `abort()` / 非计划性复位

单次启动和 OTA 闭环不能替代 72 小时以上的持续浸泡测试；7×24 能力需要长期压力数据证明。
