# ESP32-S3 工业网关固件

Rust + ESP-IDF v5.5.4，面向 W5500、BLE 手持机配置、Modbus RTU/TCP、IO 和模拟量。
默认构建使用 MCP23017；F3/F4 构建使用 PCA9555（F4 为多芯片 48 点组），必须与实物核对。

## 开发与验证

```bash
cargo check --locked
python3 tests/host/run_production_core.py
cargo test --locked --bin gateway --no-run
cargo build --locked --release --bin gateway
```

`--no-run` 仅编译设备测试。烧录必须显式指定 bootloader、分区表、factory，使用
`--no-skip`，不可只传 ELF。完整命令见 [烧录指南](docs/FLASH.md)。

## 文档与测试

- [文档索引](docs/index.md) · [项目与硬件概览](docs/README.md)
- [架构](docs/architecture.md) · [寄存器/引脚](docs/pinmap.md)
- [当前生产核查](docs/testing/production-audit-2026-10-30.md) · [验收清单](docs/verification.md)
- [当前待办与功能边界](docs/todo.md) · [正式构建与交付](docs/DELIVERY_BUILD.md)
- [台架测试工具](tests/manual/README.md) · [主机核心测试](tests/host/run_production_core.py)

`log/` 存现场记录；多数原始日志被 Git 忽略，重要结论必须同步到 `docs/`。
`release/candidates/` 为有 SHA 的候选产物，不代表已经通过全部物理硬件或 72 小时放行门槛。
正式发布脚本要求干净提交和固定版本；不要将旧 release 文件或候选包误作最新生产物。

## 交付范围

当前默认路径是 BLE GATT、W5500、Modbus RTU 主/从站及 TCP、IO、AI/AO、配置保存和 OTA。
LoRa、透明转发、Wi-Fi 自动故障切换、BLE Mesh 业务和双光口冗余不作为已完成能力。
所有硬件功能和 7×24 运行结论以对应镜像、硬件、负载和实测记录为准。
