# 固件编译与客户交付指南

> 适用产品：ESP32-S3 工业网关固件 v2.2.1
>
> 适用硬件：ESP32-S3 rev 0.2、8MB Flash、2MB PSRAM
>
> 交付方式：乐鑫 Flash Download Tool 首次烧录/返厂恢复，以及 Web OTA 升级

## 1. 交付原则

- 客户不接触 Rust ELF、源码、`partitions.csv` 或命令行构建环境。
- 首次烧录包必须包含 Bootloader、编译后的分区表和 factory 应用镜像。
- 后续普通升级优先使用 Web OTA，避免清除 NVS 中的网络、端口和业务配置。
- Flash 参数固定为 **DIO / 40MHz / 8MB**，禁止使用 QIO / 80MHz。
- 每个交付包必须附版本说明和 SHA-256 校验文件。
- 发布前必须完成真实设备烧录、启动、TCP、BLE、Web 和 OTA 回归。

## 2. 构建环境

生产构建机需要：

- ESP-IDF v5.5.4
- Xtensa Rust `esp` 工具链
- `ldproxy`
- `espflash`
- 项目仓库及锁定的依赖

进入项目后检查工具：

```bash
cd /Users/takumi/Workspace/idf
. "$HOME/export-esp.sh"

rustc --version
cargo --version
ldproxy --version
espflash --version
```

## 3. 发布前检查

确认工作区和版本：

```bash
git status --short
git log -1 --oneline
git rev-parse --short HEAD
```

正式交付应从已提交且经过验证的 commit 构建。若 `git status --short` 有未提交源码变更，构建产物必须标记为测试包，不能作为正式客户版本。

执行静态检查：

```bash
cargo check
cargo check --features f3
cargo check --features f4
cargo test --bin gateway --no-run
git diff --check
```

编译输出必须为 0 error、0 warning。

## 4. 清理配置缓存

`sdkconfig.defaults`、Flash 参数、分区或 ESP-IDF 配置发生变化后，必须清除对应 release 配置缓存：

```bash
rm -rf target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*
```

这一步不会删除源码和设备数据，只会强制重新生成 ESP-IDF release 构建配置。禁止复用仍包含 QIO/80MHz 的旧 release 缓存。

## 5. 编译 Release 固件

```bash
cargo build --bin gateway --release
```

核心产物：

| 产物 | 路径 | 说明 |
|---|---|---|
| Bootloader | `target/xtensa-esp32s3-espidf/release/bootloader.bin` | 首次烧录文件 |
| 分区表 | `target/xtensa-esp32s3-espidf/release/partition-table.bin` | 编译后的 8MB OTA 分区表 |
| 应用 ELF | `target/xtensa-esp32s3-espidf/release/gateway` | 仅供构建和内部调试，不交给客户烧录 |

确认生成配置：

```bash
rg 'CONFIG_ESPTOOLPY_FLASHMODE="dio"' \
  target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*/out/sdkconfig

rg 'CONFIG_ESPTOOLPY_FLASHFREQ="40m"' \
  target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*/out/sdkconfig

rg 'CONFIG_ESPTOOLPY_FLASHSIZE="8MB"' \
  target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*/out/sdkconfig
```

三项必须分别得到 `dio`、`40m`、`8MB`。任一不符都禁止发布。

## 6. 生成客户 BIN 文件

使用 release ELF 生成仅应用镜像：

```bash
espflash save-image --chip esp32s3 \
  --flash-mode dio \
  --flash-freq 40mhz \
  --flash-size 8mb \
  target/xtensa-esp32s3-espidf/release/gateway \
  gateway-factory.bin
```

生成后必须检查应用镜像不超过单个 OTA 分区 `0x240000`（2,359,296 字节）：

```bash
app_bytes=$(stat -f '%z' gateway-factory.bin) # Linux 使用: stat -c '%s'
test "$app_bytes" -le $((0x240000))
```

2026-08-13 的完整业务 release 基线为 `1,626,928B`，约占 OTA 槽 69.0%，剩余
`732,368B`。体积回归应与该基线比较；debug ELF 不作为交付体积指标。

同一个 `gateway-factory.bin` 可以作为 Web OTA 的应用镜像内容，但交付包中应复制并使用不同文件名区分用途：

```text
gateway-factory.bin   首次烧录或返厂恢复，写入 0x20000
gateway-ota.bin       Web 系统维护页面上传，不指定 Flash 地址
```

禁止把包含 Bootloader/分区表的整片合并镜像上传到 Web OTA。

## 7. 交付目录

建议目录结构：

```text
gateway-v2.2.1-<commit>/
├── flash-download-tool/
│   ├── bootloader.bin
│   ├── partition-table.bin
│   └── gateway-factory.bin
├── ota/
│   └── gateway-ota.bin
├── SHA256SUMS.txt
├── RELEASE_NOTES.txt
└── 客户烧录说明.pdf
```

在项目根目录执行以下命令可生成基础交付目录：

```bash
release_id="gateway-v2.2.1-$(git rev-parse --short HEAD)"

mkdir -p "$release_id/flash-download-tool" "$release_id/ota"
cp target/xtensa-esp32s3-espidf/release/bootloader.bin \
  "$release_id/flash-download-tool/bootloader.bin"
cp target/xtensa-esp32s3-espidf/release/partition-table.bin \
  "$release_id/flash-download-tool/partition-table.bin"
cp gateway-factory.bin \
  "$release_id/flash-download-tool/gateway-factory.bin"
cp gateway-factory.bin "$release_id/ota/gateway-ota.bin"
```

复制后应确认两个应用镜像的 SHA-256 一致；它们只是交付用途和文件名不同。

`RELEASE_NOTES.txt` 至少记录：

- 产品名称和固件版本
- Git commit
- 构建日期
- 适用硬件版本
- Flash 参数：DIO / 40MHz / 8MB
- 已验证功能和已知限制

## 8. SHA-256 校验

macOS/Linux：

```bash
cd "$release_id"
shasum -a 256 \
  flash-download-tool/bootloader.bin \
  flash-download-tool/partition-table.bin \
  flash-download-tool/gateway-factory.bin \
  ota/gateway-ota.bin > SHA256SUMS.txt
```

Windows 在交付目录中可逐个执行：

```powershell
certutil -hashfile flash-download-tool\bootloader.bin SHA256
certutil -hashfile flash-download-tool\partition-table.bin SHA256
certutil -hashfile flash-download-tool\gateway-factory.bin SHA256
certutil -hashfile ota\gateway-ota.bin SHA256
```

客户烧录前应核对文件校验值，防止传输损坏或使用错误版本。

## 9. Flash Download Tool 配置

启动工具时选择：

```text
ChipType: ESP32-S3
WorkMode: Develop
LoadMode: UART
```

添加并勾选以下三行：

| 文件 | 地址 |
|---|---:|
| `bootloader.bin` | `0x0000` |
| `partition-table.bin` | `0x8000` |
| `gateway-factory.bin` | `0x20000` |

工具参数：

```text
SPI SPEED: 40MHz
SPI MODE:  DIO
FLASH SIZE: 8MB
BAUD: 460800
```

如果客户电脑或 USB 转串口链路不稳定，将 BAUD 降为 `115200`。不要提高到 QIO/80MHz。

## 10. 烧录场景

### 10.1 新设备或返厂恢复

1. 备份需要保留的业务配置。
2. 在 Flash Download Tool 中点击 `ERASE`。
3. 勾选三个 BIN 文件并确认地址。
4. 点击 `START`，等待 `FINISH`。
5. 只按 `RST/EN` 正常复位，不要按住 `BOOT` 启动。

全擦会删除 NVS、端口配置、逻辑配置、DO 状态、OTA 状态、BLE Mesh 数据和存储文件。

### 10.2 保留配置的普通升级

不要使用 Flash Download Tool 全擦。登录设备 Web 页面：

```text
系统维护 -> 固件升级 -> 选择 gateway-ota.bin
```

上传完成后设备自动重启。新镜像稳定运行 30 秒后才会被标记为有效；确认前异常复位会触发 OTA 自动回滚。

### 10.3 串口覆盖但保留 NVS

此方式仅供技术支持人员使用。Flash Download Tool 不应点击 `ERASE`，但旧 `otadata` 可能仍指向 `ota_0/ota_1`，导致新写入的 factory 不启动。因此客户现场普通升级仍应使用 Web OTA。

## 11. 连接失败处理

如果工具提示无法连接：

1. 关闭串口监控、日志工具和其他烧录程序。
2. 确认选择了正确 COM 口。
3. 按住 `BOOT`。
4. 短按 `RST/EN`。
5. 松开 `BOOT`。
6. 再点击 `START`。

烧录完成后，串口必须被工具释放。端口长期被旧进程占用也会表现为设备无法连接。

## 12. 出厂验收

启动日志必须包含：

```text
Boot SPI Speed : 40MHz
SPI Mode       : DIO
SPI Flash Size : 8MB
Loaded app from partition at offset 0x20000
```

Web OTA 后应从 `0x260000` 或 `0x4A0000` 启动。

至少检查：

- Web 登录、设备信息、网络、端口、I/O 和系统状态
- Modbus TCP `502/503/504/5002`
- 标准 FC03 最大数量 125
- 手持机 BLE 连接、心跳和完整属性读取
- NFC 状态、备份和恢复
- OTA 上传、30 秒确认、再次复位不回滚
- DI/DO/AI/AO 与实际硬件
- 无 `Failed to create task`、stack canary、panic 或非计划重启

Modbus RTU 必须使用独立 USB-RS485 适配器和真实总线验证，烧录/日志串口不能替代 RS485 测试。

## 13. 发布判定

满足以下条件才允许交付：

- Release 编译和测试构建通过，0 warning。
- 三个 Flash Download Tool 文件地址明确且 SHA-256 已记录。
- 在目标硬件完成一次完整串口烧录。
- TCP、BLE、Web、NFC、OTA 和 I/O 回归通过。
- RTU 已具备实物测试记录，或在交付限制中明确注明未覆盖。
- 7x24 稳定性只能由真实长期浸泡数据证明，短期启动测试不得替代。

相关文档：

- [烧录与 OTA 指南](FLASH.md)
- [开发构建说明](build.md)
- [实机验证记录](LOOP.md)
