# ESP32-S3 工业网关 (esp32s3-iot-gateway)

文档导航见 [文档索引](index.md)。

基于 ESP-IDF v5.5 + Rust 重构的工业控制系统固件。

## 硬件
- **MCU**: ESP32-S3R2 (Xtensa LX7 双核 240MHz, 512KB 物理 SRAM, **2MB Quad PSRAM**, 8MB Flash)
- **以太网**: W5500 over SPI3
- **RS485 #0 (主)**: UART1 (GPIO45/46/7)
- **RS485 #1 (从)**: UART2 (GPIO42/41/8)
- **DI/DO**: PCA9555 I2C 扩展 (SDA=35, SCL=36, LED 板 SDA=38, SCL=37)
- **AI**: ADC1_CH0-5 (GPIO1-6), 12-bit
- **AO**: LEDC_CH0-3 (GPIO15-18), 5kHz PWM
- **电源使能**: GPIO21 (LED), GPIO33 (RELAY)

详细引脚分配见 [引脚映射](pinmap.md)

## 架构

**main_loop + 有界后台任务**:
- main_loop: 5ms 调度周期，负责 IO、BLE、以太网心跳和 Modbus TCP
- DeviceActor: 串行 NVS/Flash 持久化
- 每个启用的 RS485 端口独立运行 Master/Slave 任务
- HTTP、NFC、UDP 及 BLE 使用固定栈和有界缓冲；任务由健康监控统一检查

合并到 main_loop 的模块: ai-sample, ao-output, di-scan, do-output, eth-heartbeat

详细架构见 [架构说明](architecture.md)

完整 Flash、SRAM、PSRAM、任务栈和协议地址布局见
[整体内存布局](MEMORY_LAYOUT.md)。

## 功能
- ✅ Modbus TCP (502/503/504/5002): FC=01/02/03/04/05/06/15/16
- ✅ Modbus RTU Master/Slave: 19200 8N1
- ✅ BLE GATT 通知: Android 手持机兼容 (metuory-wireless-management-app-1.0.78)
- ✅ AI 6 通道 100ms 采样, 滑动平均
- ✅ AO 4 通道 PWM 输出
- ✅ DI 16 通道去抖扫描
- ✅ DO 16 通道事件驱动输出
- ✅ NVS 配置持久化
- ✅ RCU 无锁共享状态
- ✅ Web OTA 升级（factory/ota 分区、回滚确认）

## 编译

```bash
cargo build
```

正式客户交付请使用 Release 构建和固定交付清单，详见
[固件编译与客户交付指南](DELIVERY_BUILD.md)。

## 烧录

```bash
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    --bootloader target/xtensa-esp32s3-espidf/debug/bootloader.bin \
    --partition-table partitions.csv --partition-table-offset 0x8000 \
    --target-app-partition factory --erase-parts otadata \
    --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
    target/xtensa-esp32s3-espidf/debug/gateway
```

详细烧录指南见 [烧录指南](FLASH.md)

## 监控

```bash
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

## 测试

- 单元测试编译: `cargo test --bin gateway --no-run`（ESP-IDF 目标不在主机执行）
- Modbus TCP: [手工测试工具](../tests/manual/test_modbus_network.py)
- BLE Android 兼容: [协议流程](ble/BLE_ANDROID_FLOW.md) 和 [手持机测试计划](testing/handheld-network-config.md)

## 测试日志

所有测试日志在 `log/` 目录:
- `log/sessions/` - 启动/编译日志
- `log/hardware/` - 引脚核对
- `log/ble/` - BLE 兼容性测试
- `log/modbus/` - Modbus TCP 测试
- `log/unit/` - 单元测试
- `log/SUMMARY_2026-07-22.md` - 最新工作总结

## 角色协作

- 产品经理: 对照 metuory-wireless-management-app-1.0.78 提出缺失功能
- 高级 Rust 开发工程师: 实施功能与修复 BUG
- 高级测试工程师: 持续测试
- 架构说明见 architecture.md
- 工业软件审计专家: 审计每次实施
