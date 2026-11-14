# 文档索引

## 当前项目文档

- [项目概览与功能](README.md)
- [架构与任务资源边界](architecture.md)
- [构建指南](build.md) · [Release 交付指南](DELIVERY_BUILD.md)
- [GitHub CI 与发布](CI.md)
- [烧录与 OTA](FLASH.md)
- [用户手册](user-manual.md)
- [引脚映射](pinmap.md) · [内存布局](MEMORY_LAYOUT.md)
- [双光纤调查与验收边界](hardware/fiber-analysis.md)
- [NFC K/KC 驱动与芯片依据](hardware/nfc-st25dv16kc.md) · [2026-11-11 实板验证](testing/nfc-hardware-verification-2026-11-11.md)
- [验证说明](verification.md) · [持续集成记录](LOOP.md)
- [2026-10-30 生产核查](testing/production-audit-2026-10-30.md)
- [当前待办](todo.md)

## 按主题分类

- [`ble/`](ble/)：BLE Android 协议、修复和兼容流程。
- [`testing/`](testing/)：手持机和网络配置验证计划。
- [`system/`](system/)：系统设计专题和历史变更说明。
- [`hardware/`](hardware/)：硬件识别、NFC/LoRa 接口和现场验收边界。
- [`archive/`](archive/)：已被当前源码/LOOP 记录取代的旧报告、开发计划、通信审计和旧版开发指南。
- [`../tests/manual/`](../tests/manual/)：连接真实设备的手工测试工具。
- [`../tests/modbus_tests.rs`](../tests/modbus_tests.rs)：Modbus 帧级集成测试。

## 测试与运行记录

日志文件按 `log/` 下的 BLE、硬件、Modbus、性能和压力测试目录分类。该目录受
`.gitignore` 限制，大部分现场日志不进入版本控制；交付所需的重要结果应同步写入
本目录的验证文档或 LOOP 记录。

## 发布

- [IMC v3.3.0 发布说明](releases/v3.3.0.md) · [实机验收](testing/v3.3.0-rs485-1-acceptance-2026-11-14.md)
- [v2.9.0→v3.3.0 OTA 兼容性与封板记录](testing/ota-v2.9.0-to-v3.3.0-2026-11-15.md)

- [IMC v3.1.0 生产发布说明](releases/v3.1.0.md)
- [IMC v3.0.0 发布说明](releases/v3.0.0.md)
- [IMC v2.10.0 生产发布说明](releases/v2.10.0.md)
