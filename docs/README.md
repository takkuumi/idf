# ESP32-S3 工业网关 (esp32s3-iot-gateway)

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

详细引脚分配见 [docs/pinmap.md](pinmap.md)

## 架构

**4 个用户 pthread + main_loop tick**:
- main_loop: 100ms 主循环, 调度所有业务
- DeviceActor: NVS 持久化
- mb-rtu-master/slave: Modbus RTU
- mb-tcp-listen: Modbus TCP

合并到 main_loop 的模块: ai-sample, ao-output, di-scan, do-output, eth-heartbeat

详细架构见 [docs/ARCHITECTURE.md](ARCHITECTURE.md)

完整 Flash、SRAM、PSRAM、任务栈和协议地址布局见
[整体内存布局](MEMORY_LAYOUT.md)。

## 功能
- ✅ Modbus TCP (502): FC=03/04/06/16
- ✅ Modbus RTU Master/Slave: 19200 8N1
- ✅ BLE GATT 通知: Android 手持机兼容 (metuory-wireless-management-app-1.0.78)
- ✅ AI 6 通道 100ms 采样, 滑动平均
- ✅ AO 4 通道 PWM 输出
- ✅ DI 16 通道去抖扫描
- ✅ DO 16 通道事件驱动输出
- ✅ NVS 配置持久化
- ✅ RCU 无锁共享状态
- ⏳ OTA 升级 (TODO)

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

详细烧录指南见 [docs/FLASH.md](FLASH.md)

## 监控

```bash
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

## 测试

- 单元测试: `cargo test --bin gateway --no-run` (需在设备上跑)
- Modbus TCP: `python3 -c "import socket; ..."` 见 [log/modbus/tcp_test.md](log/modbus/tcp_test.md)
- BLE Android 兼容: 见 [log/ble/android_read_2026-07-21.md](log/ble/android_read_2026-07-21.md)

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
- 高级系统架构师: 架构层把关 (见 ARCHITECTURE.md)
- 工业软件审计专家: 审计每次实施
