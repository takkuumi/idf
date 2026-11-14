# 工业测控器 IMC 固件

Rust + ESP-IDF v5.5.4，面向 W5500、BLE 手持机配置、Modbus RTU/TCP、IO 和模拟量。
当前发布版本为 **3.2.0**，二进制目标为 `imc`，产品标识为 `imc-controller`。

> 初版 3.2.0 实机定位 RTU 收帧微秒/毫秒混用（9600bps 等待约4秒），初版归档已移到 candidates 并暂停交付；修复及完整验收进行中。3.2.0 同时合入组态落盘/BLE 请求修复，并修复 Web 会话失效和 AI 单端标定问题。旧 3.1.0 初版归档暂停交付。验证范围以 [3.2.0 验收记录](docs/testing/v3.2.0-real-device-acceptance-2026-11-14.md) 为准。
出厂网络为 `192.168.1.200 / 255.255.255.0`，网关与 DNS 为 `192.168.1.1`；
2.7.0 是首个生产数据基线；v3.2.0 沿用该版存储格式，可从 v2.7.0 保留配置升级。开发版本首次切换仍须全擦 Flash。
DEFAULT/F3 使用 PCA9555（16DI/16DO），F4 使用多片 PCA9555（48DI/48DO）。三者共用软件 I2C IO/LED 总线引脚。

## 开发与验证

```bash
cargo check --locked
python3 tests/host/run_production_core.py
cargo test --locked --bin imc --no-run
cargo build --locked --release --bin imc
```

`--no-run` 仅编译设备测试。烧录必须显式指定 bootloader、分区表、factory，使用
`--no-skip`，不可只传 ELF。完整命令见 [烧录指南](docs/FLASH.md)。

## 文档与测试

- [文档索引](docs/index.md) · [项目与硬件概览](docs/README.md)
- [架构](docs/architecture.md) · [寄存器/引脚](docs/pinmap.md)
- [当前生产核查](docs/testing/production-audit-2026-10-30.md) · [验收清单](docs/verification.md)
- [v3.2.0 发布说明](docs/releases/v3.2.0.md) · [当前待办与功能边界](docs/todo.md) · [正式构建与交付](docs/DELIVERY_BUILD.md)
- [台架测试工具](tests/manual/README.md) · [主机核心测试](tests/host/run_production_core.py)

`log/` 存现场记录；多数原始日志被 Git 忽略，重要结论必须同步到 `docs/`。
`release/candidates/` 为有 SHA 的候选产物，不代表已经通过全部物理硬件或 72 小时放行门槛。
正式发布脚本要求干净提交和固定版本；不要将旧 release 文件或候选包误作最新生产物。

## 交付范围

当前默认路径是 BLE GATT、W5500、Modbus RTU 主/从站及 TCP、IO、AI/AO、配置保存和 OTA。
LoRa 透明转发已纳入 F3 生产构建；Wi-Fi 自动故障切换、BLE Mesh 业务和双光口冗余不作为默认生产能力。
所有硬件功能和 7×24 运行结论以对应镜像、硬件、负载和实测记录为准。


