# 文档索引

## 当前项目文档

- [项目概览与功能](README.md)
- [架构与任务资源边界](architecture.md)
- [构建指南](build.md) · [Release 交付指南](DELIVERY_BUILD.md)
- [烧录与 OTA](FLASH.md)
- [用户手册](user-manual.md)
- [引脚映射](pinmap.md) · [内存布局](MEMORY_LAYOUT.md)
- [验证说明](verification.md) · [持续集成记录](LOOP.md)
- [当前待办](todo.md)

## 按主题分类

- [`ble/`](ble/)：BLE Android 协议、修复和兼容流程。
- [`testing/`](testing/)：手持机和网络配置验证计划。
- [`comm/`](comm/)：2026 年 7 月通信、架构及阶段审计记录。
- [`system/`](system/)：系统设计专题和历史变更说明。
- [`archive/`](archive/)：已被当前源码/LOOP 记录取代的旧报告。
- [`../tests/manual/`](../tests/manual/)：连接真实设备的手工测试工具。
- [`../tests/modbus_tests.rs`](../tests/modbus_tests.rs)：Modbus 帧级集成测试。

## 测试与运行记录

日志文件按 `log/` 下的 BLE、硬件、Modbus、性能和压力测试目录分类。该目录受
`.gitignore` 限制，大部分现场日志不进入版本控制；交付所需的重要结果应同步写入
本目录的验证文档或 LOOP 记录。
