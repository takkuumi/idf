# Repository Guidelines

ESP32-S3 工业网关固件（Rust + ESP-IDF v5.5）。所有贡献者必须遵循本文档。

## Project Structure & Module Organization

- **入口与配置**：`src/main.rs`（主循环）、`src/config.rs`（寄存器映射、引脚、常量）、`src/health.rs`（任务心跳）。
- **硬件驱动**：`src/ethernet/`（W5500）、`src/hal/`（GPIO/ADC/I2C/PCA9555）、`src/rs485/`（Modbus RTU UART 端口）。
- **协议**：`src/ble_at/`（BLE GATT + AT 命令）、`src/modbus/`（RTU/TCP server）、`src/protocol/`（可插拔注册表）。
- **业务与状态**：`src/bus/`（无锁 RCU 总线 + AtomicBits64）、`src/channel/`（AI/AO 采样）、`src/io/`（DI/DO 扫描）、`src/device/`（NVS 持久化）。
- **系统层**：`src/actor/`（设备状态机）、`src/error/`（recovery + ringlog）、`src/ota/`、`src/sync.rs`。
- **集成测试**：`tests/`。
- **文档**：`docs/`（架构、烧录、引脚、API）；**测试日志**：`log/`（按 sessions/hardware/ble/modbus/unit 分类）。

## Build, Test, and Development Commands

```bash
# 编译（默认 features）
cargo build

# 编译 + 烧录（必须带 --no-skip 强制重写所有块）
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    --bootloader target/xtensa-esp32s3-espidf/debug/bootloader.bin \
    --partition-table partitions.csv --partition-table-offset 0x8000 \
    --target-app-partition factory --erase-parts otadata \
    --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
    target/xtensa-esp32s3-espidf/debug/gateway

# 串口监控（按 Ctrl+R 重置芯片）
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200

# 单元测试（受 esp-idf 工具链限制，--no-run 仅做编译检查）
cargo test --bin gateway --no-run

# 配置修改后必须清构建缓存
rm -rf target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-*
```

**特性开关**（`Cargo.toml`）：`ble-at` / `ethernet-w5500` / `modbus-rtu` / `modbus-tcp` / `ai-ao` / `io-di-do` / `f3` / `f4`。

## Coding Style & Naming Conventions

- **缩进**：4 空格（Rust 标准），禁止 tab。
- **命名**：`snake_case`（变量、函数、模块）；`SCREAMING_SNAKE_CASE`（常量，如 `INREG_QI_COUNT`）；`CamelCase`（结构体、枚举）。
- **寄存器常量**：集中在 `src/config.rs::regs`，地址用 `u16`，命名遵循 `HOLD_*` / `INREG_*` / `COIL_*` / `DISC_*` 前缀。
- **日志**：中文 + ASCII 标识，例 `log::info!("[ble_at] service startup queued")`。
- **无堆分配原则**：pthread 任务中禁止 `Vec::with_capacity()`；用 `heapless::Vec<u8, N>` 或栈数组。
- **警告**：保持 0 警告；非必要 `#[allow(...)]` 必须注释理由。
- **格式化**：`cargo fmt`（隐含）；提交前运行 `cargo check 2>&1 | grep -E '^warning:' | wc -l` 应为 0。

## Testing Guidelines

- **单元测试**：写在源文件内 `#[cfg(test)] mod tests`，命名 `test_<行为>`。覆盖 127+ 测试（`src/device/system_config.rs:31`、`src/ble_at/mod.rs:21`、`src/modbus/shared.rs:12`）。
- **集成测试**：`tests/modbus_tests.rs`（CRC、PDU 解析）。
- **硬件测试**：`log/` 目录记录真实设备运行结果（启动日志、Modbus TCP 命令响应、BLE 协议响应）。
- **测试命名**：断言可读、错误信息含输入值，例 `assert_eq!(regs::HOLD_HW_VER, 0x08A5, "HW_VER must be 0x08A5 (MCA 2213)")`。
- **新功能必须**：（1）单测覆盖正常+边界路径；（2）`log/` 增加测试用例与结果文档。

## Commit & Pull Request Guidelines

提交信息格式（参考最近 10 条 commit）：

```
<类型>: <作用域> 简明描述 (<=72 字符)

<可选详细说明: 改动动机、根因、验证结果>
```

**类型**：`fix` / `feat` / `docs` / `refactor` / `test` / `perf` / `P0/P1/P2`（优先级）。

**作用域**：`ble` / `eth` / `modbus` / `io` / `channel` / `docs` / `build`。

**示例**：
```
fix(ble): 修复 Android 手持机连接后 IP/MAC/BLE_ID 不显示
docs: 完整烧录指南 (espflash --no-skip + 故障排查)
P0: 根本修复 pthread Stack canary + ENOMEM
```

**PR 要求**：（1）描述动机 + 根因 + 验证数据；（2）引用关联 issue；（3）涉及硬件改动附 `log/` 测试日志；（4）`docs/LOOP.md` 任务状态更新。

## Architecture Overview

**4 个用户 pthread + main_loop 调度 7 个模块**（见 `docs/ARCHITECTURE.md`）：
- **pthread**：DeviceActor（8KB）、mb-rtu-master/slave、mb-tcp-listen。
- **main_loop 100ms tick**：ai-sample（100ms）、ao-output（100ms）、di-scan（20ms）、do-output（100ms + notify）、eth-heartbeat（5s）。
- **状态共享**：所有 IO 状态走 `std::sync::Mutex<Option<State>>` + `try_lock`（死锁安全）。
- **禁止**：在 pthread 中 `Vec::with_capacity()`、递归超过 5 层、自定义 `.stack_size()` 超过 16KB（破坏栈总量平衡）。

## Security & Configuration Tips

- **NVS namespace**：`gateway`（`src/device/mod.rs`），修改后需 `AT+CFGAPPLY` 触发 NVS 写入。
- **设备 ID 默认值**：`src/device/system_config.rs::defaults()`（SN、IP、MAC）。
- **引脚分配**：必须改 `src/config.rs::pins` 而非硬件层；改动后同步 `docs/pinmap.md`。
- **禁止修改**：`/Users/takumi/Workspace/esp-idf`、`/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE`、`/Users/takumi/Workspace/metuory-wireless-management-app-1.0.78`。
- **烧录前**：`cargo build` + 上述完整 `espflash flash --no-skip` 命令；禁止只传 ELF，
  否则 espflash 会生成默认单 factory 分区表并破坏 OTA 布局。**监控用 Ctrl+R 重置**，不进入下载模式。
