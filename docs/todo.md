# 已知 TODO 清单

> 待对照实际硬件、ESP-IDF v5.5.4 头文件、esp-idf-hal 0.45 实际 API 校准

## ★ 存储区域划分与 ESP32-S3R8 潜力利用修复（2026-07-08）

对照 ESP-IDF v5.5.4 源码核对分区表和 sdkconfig.defaults，修复存储分区与硬件潜力利用问题。

### 分区表修复 (partitions.csv)

| 修复项 | 旧 | 新 | 说明 |
|--------|-----|-----|------|
| factory/ota 等大 | factory 3MB / ota 0/1 2.25MB | 全部 2.25MB (0x240000) | 保证 factory 固件可 OTA 升级 |
| nvs_keys 分区 | 缺失 | 新增 0x17000, 4KB, encrypted | NVS 加密必需 (CONFIG_NVS_ENCRYPTION=y) |
| coredump 分区 | 缺失 | 新增 0x6E0000, 64KB | 崩溃转储到 Flash, 工业故障分析 |
| ble_mesh 分区 | 已定义但代码未使用 | 通过 CONFIG_BT_BLE_MESH_SPECIFIC_PARTITION 启用 | BLE Mesh 栈自动调用 nvs_flash_init_partition("ble_mesh") |
| storage 分区 | 304KB | 1MB (0x100000) | FAT 大文件存储扩展空间 |

### sdkconfig.defaults 修复

| 修复项 | 旧 | 新 | 说明 |
|--------|-----|-----|------|
| Flash 速度 | 未显式配置 (默认 40MHz) | QIO + 80MHz + 8MB | 性能提升一倍 |
| Core Dump | 未启用 | TO_FLASH + ELF + 16 tasks | 工业可靠性 |
| Heap Poisoning | 未启用 | COMPREHENSIVE | 越界写入检测 |
| 日志级别 | VERBOSE | DEBUG (编译时) + INFO (运行时) | 减少代码体积 |
| NVS 加密密钥 | 未配置 | ENCRYPTION_KEYS_FLASH=y | 配合 nvs_keys 分区 |
| BLE Mesh NVS | 默认 "nvs" 分区 | SPECIFIC_PARTITION + "ble_mesh" | 与系统 NVS 隔离 |

### 已验证的 ESP32-S3R8 潜力利用 ✅

- 双核 240MHz + vTaskCoreAffinitySet 11 任务绑定 (Core 0 网络 / Core 1 实时采集)
- 8MB Octal PSRAM 80MHz + XIP + 4KB 内部分配阈值 + 16KB DMA 保留
- ICache 32KB + DCache 32KB + 64B line
- 硬件加密 AES/SHA/RSA/GCM/ECC 全启用
- FreeRTOS SMP HZ=1000 + TASK_SNAPSHOT + RUN_TIME_STATS
- 3 路 UART + SPI2 (W5500) + I2C0 (MCP23017) + ADC1 DMA 6 通道 + LEDC 4 通道
- WiFi/BLE 软件共存 + OTA 回滚 + Task/Int WDT 双层看门狗

## ★ BLE Mesh API 对照 ESP-IDF v5.5.4 源码查漏补缺（2026-07-08）

对照 `/Users/ling/workspace/esp-idf/components/bt/esp_ble_mesh/` 真实源码，逐项核对 BLE Mesh C API 绑定与结构体布局，修复 3 个关键错误：

| 错误项 | 错误 | 正确 | 源码位置 |
|--------|------|------|----------|
| Proxy enable 函数名 | `esp_ble_mesh_proxy_proxy_enable` | `esp_ble_mesh_proxy_gatt_enable` | `esp_ble_mesh_proxy_api.h:40` |
| OnOff GET opcode | `0x8200` | `0x8201` (`OP_2(0x82, 0x01)`) | `esp_ble_mesh_defs.h:2161` |
| 回调函数共用 | `custom_model_cb` 与 `generic_client_cb` 共用同一函数, 事件值空间冲突 (event 0 在前者=MODEL_OPERATION, 在后者=GET_STATE) | 拆分为 `custom_model_event_cb` + `generic_client_event_cb` 两个独立回调 | `esp_ble_mesh_defs.h:2620` / `esp_ble_mesh_generic_model_api.h:460` |

**附加修正**:
- `_enh_reserved` 从 `[u8; 32]` 缩减到 `[u8; 12]` (匹配 `esp_ble_mesh_msg_enh_params_t` 在未启用 EXT_ADV/LONG_PACKET 时的实际大小, defs.h L710-750)
- `EspBleMeshMsgCtx` 总大小从 64 bytes 修正为 44 bytes
- 添加 Generic Client 事件常量 `EVT_GENERIC_CLIENT_GET_STATE/SET_STATE/PUBLISH/TIMEOUT`

**已验证正确项**:
- `esp_ble_mesh_init(prov, comp)` 双参数 ✓
- `esp_ble_mesh_model_publish` 5 参数 (无 ctx) ✓
- `esp_ble_mesh_server_model_send_msg` 5 参数 (含 ctx) ✓
- `esp_ble_mesh_client_model_send_msg` 8 参数 (含 ctx+timeout+need_rsp) ✓
- `EspBleMeshProv` 128 bytes 布局 (NODE+PROVISIONER) ✓
- `ModelOpParam` 字段 (opcode+model+ctx+length+msg, 无 errcode) ✓
- `NodeProvCompleteParam` / `ProvisionerProvCompleteParam` padding ✓
- `CONFIG_BLE_MESH_MODEL_KEY_COUNT=3` / `GROUP_COUNT=3` (Kconfig 默认值) ✓
- 事件值 `EVT_NODE_PROV_COMPLETE=10` / `EVT_PROVISIONER_PROV_COMPLETE=31` ✓
- OOB 常量 `PROV_OOB_INFO_OTHER=0x0001` (enum=u32) ✓
- 模型 ID `0x1000/0x1001` ✓

### 遗留 TODO (编译验证项)

| 验证项 | 说明 |
|--------|------|
| Xtensa 工具链编译验证 | espup install 完成后运行 cargo build 验证所有修正 |
| `MeshCb` 类型兼容性 | 三类回调使用统一 `extern "C" fn(c_int, *mut c_void)`, ABI 兼容但类型不同 |
| `*const` cast `*mut` 安全性 | `SIG_MODELS.as_ptr() as *mut` 用于 publish/send_msg, C API 不修改 model 结构 |

## ★ 阶段七已完成：设备抽象层 + 通信协议插件化（2026-07-08）

实施架构改进方案 G (DigitalIo trait) 和 H (Protocol trait + ProtocolRegistry)，详见 [architecture.md - 设备抽象层](architecture.md#设备抽象层-阶段七---g) 和 [architecture.md - 通信协议插件化](architecture.md#通信协议插件化-阶段七---h)。

### G. 设备抽象层 (DigitalIo trait)

统一 DI/DO 访问接口，屏蔽 GPIO 直驱与 I2C MCP23017 扩展的差异。

| 项目 | 内容 |
|------|------|
| `DigitalIo` trait | 8 个方法: `di_count` / `do_count` / `read_di_all` / `read_di` / `write_do_all` / `write_do` / `read_do_cached` / `read_do_actual` |
| `GpioBank` impl | 默认版本 (8 DI + 8 DO, GPIO 直驱) + `do_cache: Mutex<u64>` 缓存 |
| `IoExtender` impl | F3/F4 版本 (委托现有方法) |
| `Hal::dio()` | 返回 `&dyn DigitalIo`，编译期自动选择实现 |
| io/di.rs | 移除 `#[cfg]` 分支，统一 `hal.dio().read_di_all()` |
| io/do_.rs | 移除 `#[cfg]` 分支，统一 `hal.dio().write_do_all()` |

**关键决策**: `PinDriver<Output>` 不支持 `is_high()` 读回，GpioBank 新增 `do_cache` 字段记录已写入电平。trait object vtable 开销 ~1-2ns，相对 I2C 600μs 可忽略。

### H. 通信协议插件化 (Protocol trait + ProtocolRegistry)

统一 Modbus RTU/TCP、BLE Mesh 的启动/停止/状态查询接口。

| 项目 | 内容 |
|------|------|
| `Protocol` trait | `name` / `start` / `stop` / `is_running` / `stats` |
| `ProtocolRegistry` | `register` / `start_all` / `stop_all` / `find` / `list` / `stats` |
| `ModbusRtuProtocol` | 封装 `modbus::start_rtu(hal)` (feature_modbus_rtu) |
| `ModbusTcpProtocol` | 封装 `modbus::start_tcp()` (feature_modbus_tcp) |
| `BleMeshProtocol` | 封装 `blemesh::start(hal, nvs)` (feature_ble_mesh) |
| `ProtocolState` | `AtomicBool` 运行标志 + `Mutex<Option<Instant>>` 启动时间 + `AtomicU32` 错误计数 |

**关键决策**: `start_all()` 采用尽力而为策略 (单个协议失败不影响其它)。BLE Mesh 单独先启动 (ble_at 依赖 BLE 协议栈)。扩展新协议只需实现 trait + 注册。

### 阶段七遗留 TODO (编译验证项)

| 验证项 | 说明 |
|--------|------|
| `PinDriver<Output>` 不支持 `is_high()` | 已用 `do_cache` 替代，编译验证 |
| `EspNvsPartition::clone()` 可用性 | `device::nvs_partition()` 已在 main.rs 使用，适配器复用 |
| `Instant` 在 ESP-IDF std 可用 | `build-std = ["std"]` 已启用 |
| `Protocol::start(&self)` 不可变引用 | 用 `AtomicBool` / `Mutex` 实现内部可变性 |
| `protocols` 变量生命周期 | main() 栈上存活至 main_loop()，线程已 detach |

## ★ 阶段六已完成：硬件版本 F3/F4 支持 (I2C MCP23017 扩展)（2026-07-08）

通过编译期 feature flag 支持多种硬件版本，F3/F4 用 I2C MCP23017 扩展芯片扩展 DI/DO 通道数。详见 [architecture.md - 硬件版本 F3/F4 支持](architecture.md#硬件版本-f3f4-支持-阶段六)。

### 版本对比

| 版本 | feature | DI | DO | MCP23017 数量 | 启用命令 |
|------|---------|----|----|--------------|---------|
| Default | (无) | 8 (GPIO) | 8 (GPIO) | 0 | `cargo build` |
| F3 | `f3` | 16 (I2C) | 16 (I2C) | 2 | `cargo build --features f3` |
| F4 | `f4` | 48 (I2C) | 16 (I2C) | 4 | `cargo build --features f4` |

> F3 与 F4 互斥（build.rs 编译期校验）

### 新增模块

| 路径 | 说明 |
|------|------|
| [src/hal/i2c_bus.rs](../src/hal/i2c_bus.rs) | I2C 总线封装 (I2cDriver + 寄存器读写), 400kHz Fast Mode |
| [src/hal/mcp23017.rs](../src/hal/mcp23017.rs) | MCP23017 单芯片驱动 (init_as_input/init_as_output/read_inputs/write_outputs/probe) |
| [src/hal/io_ext.rs](../src/hal/io_ext.rs) | IoExtender 聚合器: 多片 MCP23017 管理 + DI 读取 + DO 写入 + do_cache |

### 修改文件

| 路径 | 改动 |
|------|------|
| [Cargo.toml](../Cargo.toml) | 新增 `f3` / `f4` feature (互斥), 启用后自动启用 `io-di-do` |
| [build.rs](../build.rs) | `CARGO_FEATURE_F3/F4` → `feature_f3/feature_f4` cfg 桥接 + F3/F4 互斥校验 |
| [src/config.rs](../src/config.rs) | 新增 `hw_version` 模块 (NAME/DI_COUNT/DO_COUNT/USE_IO_EXT/DI_EXT_CHIPS/DO_EXT_CHIPS) + `io_ext` 模块 (DI_ADDRS/DO_ADDR/MCP23017 寄存器) + `pins::I2C_PORT/SDA/SCL/FREQ_HZ`; `regs::COIL_DO_COUNT/DISC_DI_COUNT` 改为引用 `hw_version` |
| [src/hal/mod.rs](../src/hal/mod.rs) | `Hal` 新增 `io_ext: IoExtender` 字段 (条件) + I2C 初始化 (条件) |
| [src/hal/gpio.rs](../src/hal/gpio.rs) | `GpioBank` 条件化 DI/DO 字段 + init 签名分两个版本 (默认含 DI/DO, F3/F4 不含) + 提取 `init_eth_and_rs485()` 公共函数 |
| [src/hal/pins.rs](../src/hal/pins.rs) | `HalPins` 条件化 `di/do_` 字段 |
| [src/bus.rs](../src/bus.rs) | `DoState.bits` / `DiState.bits` 改为 u64; `read_coil/write_coil/read_disc` 用 `1u64 << addr` |
| [src/io/di.rs](../src/io/di.rs) | 采样逻辑条件编译: 默认 GPIO / F3-F4 `hal.io_ext.read_di()`; 状态变量改为 u64; 日志用 `0x{:016X}` |
| [src/io/do_.rs](../src/io/do_.rs) | 输出逻辑条件编译: 默认逐通道 GPIO / F3-F4 `hal.io_ext.write_do(bits)`; `last` 改为 u64 |
| [src/ble_at/handlers.rs](../src/ble_at/handlers.rs) | `AT+STATUS` 加 `ver={}` 字段 + DI/DO 格式改 `0x{:X}`; `AT+VERSION` 加 `(HW: {})` |
| [src/blemesh/models.rs](../src/blemesh/models.rs) | `apply_onoff` / `send_status` 中 `0x01` 改为 `0x01u64` 显式后缀 |
| [src/blemesh/bindings.rs](../src/blemesh/bindings.rs) | `heartbeat_loop` 中 `0x01` 改为 `0x01u64` 显式后缀 |

### 关键设计决策

1. **编译期 feature flag** (非运行时切换): 硬件版本固定不变, 编译期确定省去运行时分支开销
2. **DI/DO 位宽统一 u64**: 默认 8 bit / F3 16 bit / F4 48 bit, 共用同一 `DoState.bits: u64` / `DiState.bits: u64`
3. **GpioBank 条件化 DI/DO 字段**: F3/F4 版本下 GpioBank 不含 DI/DO, 释放 GPIO21/33 给 I2C
4. **IoExtender 聚合多片 MCP23017**: 内部 `di_chips: [Mcp23017; 3]` (F4 最多), `read_di() -> u64` 按顺序拼接
5. **Modbus 寄存器通道数版本相关**: `COIL_DO_COUNT` / `DISC_DI_COUNT` 引用 `hw_version`, 自动适配版本

### 阶段六遗留 TODO (编译验证项)

#### [src/hal/i2c_bus.rs](../src/hal/i2c_bus.rs) - I2C 总线

- [ ] **`AnyPin::new(num as i32)` 可用性**: esp_idf_hal 中 AnyPin 是否有此 unsafe 构造器 (AnyInputPin/AnyOutputPin 已验证, AnyPin 待确认)
- [ ] **`I2cDriver::new(i2c, sda, scl, &cfg)` 签名**: esp_idf_hal 0.45 中第 1 参数类型 `impl Peripheral<P = I2CVendor>`
- [ ] **`I2cDriver::write(addr, bytes, STOP)` 第 3 参数**: 是否为 `I2cConfig::new()` 或其他类型
- [ ] **`I2cDriver::write_read(addr, bytes_in, bytes_out, STOP)`**: 是否存在此方法
- [ ] **`I2CVendor` 类型路径**: 是否为 `esp_idf_hal::i2c::I2CVendor`
- [ ] **`I2cConfig::new().baudrate(u32.into())`**: baudrate 参数类型 (Hertz / u32)

#### [src/hal/mcp23017.rs](../src/hal/mcp23017.rs) - MCP23017 驱动

- [ ] **寄存器地址正确性**: IODIRA=0x00, IODIRB=0x01, GPPUA=0x0C, GPPUB=0x0D, GPIOA=0x12, GPIOB=0x13, OLATA=0x14, OLATB=0x15 (BANK=0 模式)
- [ ] **`init_as_input` 上拉配置**: GPPU=0xFF 启用内部上拉, 输入引脚默认高电平 (光耦隔离输入应正确反映外部电平)
- [ ] **`write_outputs` 双字节写入**: `write_reg(addr, REG_OLATA, &[port_a, port_b])` 是否连续写入 OLATA + OLATB (MCP23017 地址自增)

#### [src/hal/io_ext.rs](../src/hal/io_ext.rs) - IoExtender

- [ ] **`[Mcp23017; 3]` 初始化**: 数组固定大小 3 (F4 最大), F3 用 1 片, 默认 0 片, 数组空位如何处理
- [ ] **`Mutex<I2cBus>` 锁粒度**: read_di 期间持锁, write_do 期间也持锁, 是否需要分锁
- [ ] **probe 探测失败处理**: 当前 init 时探测失败返回 Err, 是否应继续启动 (硬件缺失某片芯片)

#### [src/hal/mod.rs](../src/hal/mod.rs) - Hal 初始化

- [ ] **`peripherals.i2c0` 字段**: esp_idf_hal::peripherals::Peripherals 是否有 `i2c0` 字段
- [ ] **`I2cBus::init` 返回值所有权**: I2cBus 持有 `I2cDriver<'static>`, 移动到 IoExtender 是否需要 `'static` 约束

#### [src/io/di.rs](../src/io/di.rs) / [src/io/do_.rs](../src/io/do_.rs) - IO 任务

- [ ] **`hal.io_ext` 字段访问**: `Arc<Hal>` 共享, `io_ext` 内部 `Mutex<I2cBus>` 线程安全
- [ ] **F3/F4 I2C 延迟**: 1ms DI 周期可能因 I2C 400kHz 读取 (16-48 通道 = 1-3 片芯片) 延迟超过 1ms, 需实测

#### [src/bus.rs](../src/bus.rs) - u64 位操作

- [ ] **`1u64 << addr` 中 addr 类型**: addr 是 u16, 位移量超过 63 会 panic (F4 最大 47, OK)
- [ ] **`COIL_DO_COUNT`/`DISC_DI_COUNT` 引用 `hw_version`**: const 上下文中 `as u16` 转换是否在编译期完成

#### 端到端验证

- [ ] **默认版本编译**: `cargo build` 通过, 行为与之前一致
- [ ] **F3 编译**: `cargo build --features f3` 通过
- [ ] **F4 编译**: `cargo build --features f4` 通过
- [ ] **互斥校验**: `cargo build --features f3,f4` 应 panic
- [ ] **F3 硬件**: DI 0-15 读写正确, DO 0-15 输出正确
- [ ] **F4 硬件**: DI 0-47 读写正确, DO 0-15 输出正确
- [ ] **Modbus**: F3 下 read_coil(15) / write_coil(15) 可读写; F4 下 read_disc(47) 可读
- [ ] **AT+STATUS**: F3 显示 `ver=F3,di=0xXXXX,do=0xXXXX`; F4 显示 `ver=F4,di=0xXXXXXXXXXXXX,do=0xXXXX`
- [ ] **AT+VERSION**: 显示 `(HW: F3)` / `(HW: F4)`

---

## ★ 阶段五已完成：Rust + ESP32-S3R8 最佳实践（2026-07-08）

针对 Rust 语言特性 + ESP32-S3R8 硬件协同优化，详见 [architecture.md - Rust + ESP32-S3R8 最佳实践 (阶段五)](architecture.md#rust--esp32-s3r8-最佳实践-阶段五)。

### 已实施项

| 优化项 | 实施内容 | 文件 |
|-------|---------|------|
| OTA 升级 | `src/ota/mod.rs` 提供 `begin/write/end/abort/status` API; BLE AT 命令 + Modbus 寄存器 0x0107-0x010F 双触发; heapless hex 解码; AppError::Ota 新增变体 | [src/ota/mod.rs](../src/ota/mod.rs), [src/ble_at/ota_handlers.rs](../src/ble_at/ota_handlers.rs), [src/bus.rs](../src/bus.rs), [src/error.rs](../src/error.rs) |
| Rust panic hook | `std::panic::set_hook` 打印 panic 位置 + `Backtrace::force_capture` backtrace, 调用 default_hook 触发 ESP-IDF 复位 | [src/main.rs](../src/main.rs) `install_panic_hook` |
| 关键路径 #[inline] | `modbus_crc16` / `AdcHandle::sample_all` / `sample` / `parse_u16` 加 `#[inline]` 让编译器内联 | [src/modbus/shared.rs](../src/modbus/shared.rs), [src/hal/adc.rs](../src/hal/adc.rs), [src/ble_at/parser.rs](../src/ble_at/parser.rs) |
| build.rs feature 校验 | `check_feature_compatibility()` 编译期校验: modbus-rtu/tcp 至少启用一个 (panic); eth+wifi 全关 (warning); wifi+ble-mesh 共存提示 | [build.rs](../build.rs) |
| 栈溢出检测 | `CONFIG_FREERTOS_CHECK_STACKOVERFLOW_CANARY=y` + `TASK_PRE_DELETE_CALLBACK=y` | [sdkconfig.defaults](../sdkconfig.defaults) |
| CPU 240MHz 锁定 | `CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ_240=y` + `CONFIG_PM_ENABLE=n` (关 DFS) | [sdkconfig.defaults](../sdkconfig.defaults) |
| Panic backtrace 配置 | `CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT=y` + `CONFIG_ESP_SYSTEM_USE_EH_FRAME=y` | [sdkconfig.defaults](../sdkconfig.defaults) |
| OTA 回滚支持 | `CONFIG_APP_ROLLBACK_ENABLE=y` + `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` | [sdkconfig.defaults](../sdkconfig.defaults) |

### 评估后不实施的 Rust 优化

| Rust 特性 | 评估结论 |
|----------|---------|
| async/await | std::thread + Mutex 在 ESP-IDF 上稳定, async runtime 复杂度收益不明显 |
| embedded-hal trait 抽象 | 硬件固定不变, trait 抽象需大改, 性价比低 |
| defmt 替代 log | esp_idf_svc::log::EspLogger 已够用, defmt 主要价值在 no_std |
| cargo workspace 拆分 | 单 crate 编译时间可接受, 拆分增加维护成本 |
| RTIC 框架 | 与 std::thread + ESP-IDF 任务模型冲突, 不适用 |
| 静态内存池 | heapless 已覆盖热点, 全静态池收益有限 |

### 阶段五遗留 TODO (编译验证项)

#### [src/ota/mod.rs](../src/ota/mod.rs) - OTA API

- [ ] **`esp_ota_handle_t` 类型**：esp_idf_sys 0.35 是否暴露 (i32 vs *mut)
- [ ] **`esp_ota_get_next_update_partition(NULL)`**：参数 `*const esp_partition_t` 是否可用 NULL
- [ ] **`esp_ota_begin(partition, size, &mut handle)`**：第 2 参数 `size_t` 类型, 0xFFFFFFFF 是否可作 SIZE_WITH_FLASH 常量
- [ ] **`esp_ota_write(handle, data, len)`**：第 2 参数 `const void *` 是否接受 `*const u8`
- [ ] **`(*partition).size`**：partition struct 字段是否为 `size`, 类型 `size_t`
- [ ] **`esp_ota_abort`**：返回 void, 调用方式是否正确
- [ ] **`AppError::Ota` 已新增**, 检查 fmt::Display 等是否完整 (已添加)

#### [src/ble_at/ota_handlers.rs](../src/ble_at/ota_handlers.rs) - AT 命令

- [ ] **`heapless::Vec<u8, 512>`**：容量 512 是否足够 AT+OTA=WRITE 单帧 (hex 1024 字符 / 2 = 512 byte)
- [ ] **hex 解析**：`char::to_digit(16)` 大小写不敏感 (已用)

#### [src/bus.rs](../src/bus.rs) - Modbus 寄存器

- [ ] **OTA_TOTAL_HI 位移**：`(value as u32) << 16` 是否正确 (u16 → u32 高 16 位)
- [ ] **OTA reboot 异步**：`std::thread::spawn + esp_restart` 是否能正确响应 Modbus (响应发送后才重启)

#### [src/main.rs](../src/main.rs) - panic hook

- [ ] **`std::backtrace::Backtrace::force_capture()`**：在 ESP-IDF std 环境下是否可用 (需要 symbols)
- [ ] **`std::panic::take_hook`**：是否需要 nightly feature
- [ ] **release `strip = true`**：backtrace 显示地址, 需 host 用 `xtensa-esp32s3-elf-addr2line` 解析

#### [build.rs](../build.rs) - feature 校验

- [ ] **`std::env::var` 顺序**：`check_feature_compatibility()` 在 embuild::output() 之后调用, env 是否已设置
- [ ] **panic 消息**：build.rs panic 是否能正确中断 cargo build

#### [sdkconfig.defaults](../sdkconfig.defaults) - 验证项

- [ ] **`CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ_240`**：实际选项名 (可能为 `CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ_240` 或 `CONFIG_ESP32S3_DEFAULT_CPU_FREQ_MHZ_240`)
- [ ] **`CONFIG_PM_ENABLE=n`**：禁用 PM 是否影响其他子系统 (BLE Mesh 可能依赖)
- [ ] **`CONFIG_FREERTOS_CHECK_STACKOVERFLOW_CANARY`**：实际选项名 (可能为 `CONFIG_FREERTOS_CHECK_STACKOVERFLOW_CANARY` 或 `_PATROL`)
- [ ] **`CONFIG_FREERTOS_TASK_PRE_DELETE_CALLBACK`**：实际选项名
- [ ] **`CONFIG_ESP_SYSTEM_USE_EH_FRAME`**：实际选项名 (可能仅 C++ 用, Rust 不需)
- [ ] **`CONFIG_APP_ROLLBACK_ENABLE`**：实际选项名 (可能为 `CONFIG_BOOTLOADER_APP_ANTI_ROLLBACK`)
- [ ] **`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`**：实际选项名

---

## ★ 阶段四已完成：ESP32-S3R8 硬件潜力实施（2026-07-08）

针对 ESP32-S3R8 的硬件特性全面评估并实施到系统中。详见 [architecture.md - ESP32-S3R8 硬件潜力实施](architecture.md#esp32-s3r8-硬件潜力实施-阶段四)。

### 已实施项

| 硬件特性 | 实施内容 | 文件 |
|---------|---------|------|
| 双核 SMP 任务绑定 | `vTaskCoreAffinitySet` 把 11 个任务固定到 Core 0 (网络) / Core 1 (实时采集) | [src/health.rs](../src/health.rs) `pin_current_to_core` + 11 个任务文件 |
| ADC Continuous + DMA | 6 通道后台 DMA 连续采样 (10kHz/ch), CPU 仅读环形缓冲区; feature flag 切换 OneShot fallback | [src/hal/adc.rs](../src/hal/adc.rs) |
| ICache 32KB / DCache 32KB | sdkconfig 启用 32KB ICache + 32KB DCache + 64B cache line | [sdkconfig.defaults](../sdkconfig.defaults) |
| 硬件加密加速器 | sdkconfig 启用 AES/SHA/MPI/GCM/ECC 硬件加速 (TLS/BLE Mesh/NVS 加密卸载) | [sdkconfig.defaults](../sdkconfig.defaults) |
| PSRAM 8MB 分配策略 | `MALLOC_ALWAYSINTERNAL=4096` (<4KB 走 SRAM), `MALLOC_RESERVE_INTERNAL=16384` (保留 16KB 给 DMA) | [sdkconfig.defaults](../sdkconfig.defaults) |
| Wi-Fi (ESP32-S3 内置) | Station 模式骨架 (feature flag `wifi`, 默认不启用); 与 BLE 共存 (COEX) | [src/wifi/mod.rs](../src/wifi/mod.rs) |
| FreeRTOS SMP 优化 | `NO_AFFINITY_HIGHEST_BOUND` + `TLSP_DELETION_CALLBACKS` (Rust JoinHandle 需要) | [sdkconfig.defaults](../sdkconfig.defaults) |

### 配套改动

| 文件 | 改动 |
|------|------|
| [Cargo.toml](../Cargo.toml) | `edition = "2024"`, release `opt-level = 3` (从 "s"), dev `opt-level = 2` + `panic = "unwind"`, 新增 features `adc-continuous` / `wifi` |
| [build.rs](../build.rs) | 新增 `feature_adc_continuous` / `feature_wifi` cfg 桥接 |
| [src/main.rs](../src/main.rs) | `#![warn(unsafe_op_in_unsafe_fn)]` (edition 2024), main_loop 开头 `pin_current_to_core(CORE_NET)`, 新增 Wi-Fi 条件启动块 |
| [src/config.rs](../src/config.rs) | 新增 `pub mod wifi` (SSID/PASSWORD/HEARTBEAT_PERIOD_S/CONNECT_TIMEOUT_S) |
| [src/blemesh/bindings.rs](../src/blemesh/bindings.rs) | `extern "C" {` → `unsafe extern "C" {` (edition 2024 强制) |
| [src/ble_at/handlers.rs](../src/ble_at/handlers.rs) | `handle_bulkr` 改用 `String::with_capacity` 一次 alloc (替代 Vec<String> 1500 次); `handle_write` 改用迭代器; `handle_bulkw` 改用 `split_first` |
| [src/ble_at/parser.rs](../src/ble_at/parser.rs) | `parse_u16_list` 返回 `heapless::Vec<u16, 128>` |
| [src/modbus/rtu_master.rs](../src/modbus/rtu_master.rs) | `regs: Vec` 改为 `heapless::Vec<u16, 128>`; 新增核绑定 |
| [src/channel/ai.rs](../src/channel/ai.rs) | 用 `hal.adc.sample_all()` 替代 6 次 `sample(ch)` 批量采样 |
| [docs/architecture.md](architecture.md) | 新增 "ESP32-S3R8 硬件潜力实施 (阶段四)" 章节, 含已实施 / 评估后不实施两类 |

### 评估后不实施的硬件特性

| 硬件特性 | 评估结论 |
|---------|---------|
| USB OTG | GPIO19/20 (USB D+/D-) 被 DI1/DI2 占用, 硬件不可用; 工业网关无需 USB |
| ULP 协处理器 | 工业网关常供电, 主 CPU 始终运行, 无深睡需求 |
| RMT / I2S / PCNT / TWAI | 无对应业务需求 (AO 用 LEDC, DI 用 GPIO 中断, 通信走 Modbus/BLE Mesh) |

### 阶段四遗留 TODO (编译验证项)

> 以下 API binding 因环境限制未编译验证, 需首次编译时校准

#### [src/hal/adc.rs](../src/hal/adc.rs) - ADC Continuous/DMA

- [ ] **`esp_adc_continuous_handle_t` 类型**：esp_idf_sys 0.35 是否暴露
- [ ] **`esp_adc_continuous_handle_cfg_t` 字段**：`max_store_buf_size` / `conv_frame_size` 名称是否正确
- [ ] **`adc_digi_pattern_config_t` 字段**：`atten` / `channel` / `unit` / `bit_width` 名称
- [ ] **`esp_adc_continuous_config_t` 字段**：`conv_mode` / `format` / `sample_freq_hz` / `adc_pattern` / `pattern_num`
- [ ] **enum 变体名**：`adc_atten_t_ADC_ATTEN_DB_11`、`adc_digi_convert_mode_t_ADC_CONV_SINGLE_UNIT_1`、`adc_digi_output_format_t_ADC_DIGI_FORMAT_12BIT` 是否为 esp_idf_sys 0.35 实际命名
- [ ] **`SOC_ADC_DIGI_MAX_BITWIDTH` 常量**：是否在 esp_idf_sys 暴露
- [ ] **`esp_adc_continuous_read` 签名**：第 5 参数 `timeout` 类型 (u32 vs i32 vs tick type)
- [ ] **样本格式**：ESP32-S3 12-bit mode 输出格式确认为 `[data:12 | channel:4]`, 还是 ADC1 通道号映射需查 SOC_ADC_CHANNEL 映射

#### [src/wifi/mod.rs](../src/wifi/mod.rs) - Wi-Fi Station 骨架

- [ ] **`esp_idf_svc::wifi::BlockingWifi::wrap` 签名**：第 2 参数是否为 `Duration`
- [ ] **`esp_idf_svc::netif::{EspNetif, NetifStack}`**：`NetifStack::Wifi` 是否存在 (esp_idf_svc 0.50)
- [ ] **`Configuration::Client` 字段**：`ssid`/`password` 类型 (`heapless::String` vs `String`)
- [ ] **`AuthMethod::None` / `WPA2Personal`**：esp_idf_svc 0.50 实际变体名
- [ ] **`wifi.sta_netif().get_ip_info().ip`**：返回类型 (`Ipv4Addr` 路径)
- [ ] **`ClientConfiguration::default()`**：Default trait 是否实现
- [ ] TODO: SSID/password 从 SystemConfig 加载 (运行时配置)
- [ ] TODO: AP 模式实现 (提供手机 APP 配置入口)
- [ ] TODO: 链路故障切换 (eth down → 启用 Wi-Fi; eth up → 关闭 Wi-Fi)

#### [src/health.rs](../src/health.rs) - 双核绑定

- [ ] **`vTaskCoreAffinitySet` 在 esp_idf_sys 0.35**：SMP 版本可用性 (若不可用需移除调用)
- [ ] **`xTaskGetCurrentTaskHandle` 返回类型**：`*mut tskTaskControlBlock` 是否可 `.is_null()`

#### [sdkconfig.defaults](../sdkconfig.defaults) - 验证项

- [ ] **`CONFIG_ESP32S3_INSTRUCTION_CACHE_32KB`**：实际 Kconfig 选项名 (可能为 `CONFIG_ESP32S3_INSTRUCTION_CACHE_SIZE` 或 `_32KB`)
- [ ] **`CONFIG_ESP32S3_DATA_CACHE_32KB`**：同上
- [ ] **`CONFIG_ESP32S3_DATA_CACHE_LINE_64B`**：实际选项名 (可能为 `CONFIG_ESP32S3_DATA_CACHE_LINE_64B` 或 `_64BYTE`)
- [ ] **`CONFIG_FREERTOS_NO_AFFINITY_HIGHEST_BOUND`**：实际选项名 (可能为 `CONFIG_FREERTOS_NO_AFFINITY_HIGHEST_BOUND` 或 `_NUM`)
- [ ] **`CONFIG_FREERTOS_TLSP_DELETION_CALLBACKS`**：实际选项名 (TLS deletion callbacks)
- [ ] **`CONFIG_MBEDTLS_HARDWARE_GCM` / `CONFIG_MBEDTLS_HARDWARE_ECC`**：ESP-IDF v5.5 是否提供 (AES/SHA/MPI 是经典项, GCM/ECC 可能新增)
- [ ] **`CONFIG_SPIRAM_USE_AHB_DBUS3`**：实际选项名
- [ ] **`CONFIG_SPIRAM_XIP_FROM_DATA`**：实际选项名
- [ ] **`CONFIG_ESP_WIFI_MGMT_SBUF_NUM` / `ESP_WIFI_TX_BA_WIN` / `ESP_WIFI_RX_BA_WIN`**：实际选项名
- [ ] **`CONFIG_ESP_COEX_SW_COEXIST_ENABLE`**：实际选项名 (ESP-IDF v5.x 可能改名)

#### [src/main.rs](../src/main.rs) - edition 2024

- [ ] **`#![warn(unsafe_op_in_unsafe_fn)]` 是否生效**：edition 2024 默认行为
- [ ] **其他 `extern "C" { }` 块**：除了 blemesh/bindings.rs, 是否还有其他模块需改 `unsafe extern "C" { }`

---

## ★ 阶段三已完成：硬件平台迁移（2026-07-08）

将整个项目从 ESP32-C5 (RISC-V, 29 GPIO, 2 UART, 无内置 PSRAM) 迁移到 **ESP32-S3R8** (Xtensa LX7 双核 240MHz, 512KB SRAM, **8MB Octal SPI PSRAM**, 3 UART, 内置 BLE 5.0 + Mesh)，以太网芯片从 DM9051 替换为 **W5500**。

### 工具链与构建配置

| 路径 | 改动 |
|------|------|
| `rust-toolchain.toml` | targets: `riscv32espidf` → `xtensaespidf` |
| `.cargo/config.toml` | target = `xtensaespidf`, linker = `xtensa-esp32s3-elf-gcc`, MCU = `esp32s3` |
| `Cargo.toml` | 包名 `esp32s3-iot-gateway`, features 新增 `ethernet-w5500` |
| `build.rs` | `CARGO_FEATURE_ETHERNET_W5500` → `feature_ethernet` cfg |
| `sdkconfig.defaults` | ESP32-S3 + 8MB Octal PSRAM + BLE Mesh + W5500 + Task Watchdog |
| `partitions.csv` | 8MB Flash 分区表 (factory 3MB + ota_0/ota_1 2.25MB + ble_mesh 64KB + storage) |
| `idf_component.yml` | 新增 `espressif/w5500: "^1.0.0"` 外部 IDF Component 依赖 |

### 硬件引脚重分配 (`src/config.rs`)

| 模块 | ESP32-C5 (旧) | ESP32-S3R8 (新) |
|------|---------------|-----------------|
| 以太网 SPI | DM9051 | W5500 (MOSI=11, MISO=13, SCLK=12, CS=10, INT=14, RST=15) |
| RS485 #0 (UART1) | GPIO4/5/6 | GPIO40/41/42 (避开 ADC1) |
| RS485 #1 (UART2) | UART0 复用 GPIO2/3 | GPIO17/18/7 (UART2 独立, 不再与下载串口复用) |
| DI 8 路 | GPIO17-24 | GPIO19/20/21/33-37 |
| DO 8 路 | GPIO8/9/10/25-28/0 | GPIO8/9/16/38/39/45/46/48 |
| AI 6 路 (ADC1) | CH0-5 | CH0-5 = GPIO1-6 (不与 UART 冲突) |
| AO 4 路 (LEDC) | GPIO8/9/10/25 | GPIO8/9/16/38 (与 DO 复用) |

### 驱动替换

| 文件 | 改动 |
|------|------|
| [src/ethernet/w5500.rs](../src/ethernet/w5500.rs) | 新建 W5500 驱动 (`esp_eth_mac_new_w5500` / `esp_eth_phy_new_w5500`), 删除 `dm9051.rs` |
| [src/hal/uart.rs](../src/hal/uart.rs) | 2 路 → 3 路 UART (UART0/1/2) 支持 |
| [src/hal/pins.rs](../src/hal/pins.rs) | 新增 `uart2_tx`/`uart2_rx` 字段, UART0 固定 GPIO43/44 |
| [src/hal/spi_bus.rs](../src/hal/spi_bus.rs) | **已删除** — SPI2_HOST 由 ethernet 模块独占管理, 避免与 `Hal::init` 重复 `spi_bus_initialize` |
| [src/hal/mod.rs](../src/hal/mod.rs) | 移除 `SpiBus` 字段, `Hal` 仅持有 GPIO/UART/ADC/LEDC |
| [src/ethernet/w5500.rs](../src/ethernet/w5500.rs) | `reset_w5500` 删除, 复用 `hal.gpio.eth_reset()` 避免重复 `gpio_config` |
| [src/device/system_config.rs](../src/device/system_config.rs) | `ESP32C5-UNKNOWN-0001` → `ESP32S3-UNKNOWN-0001`, `GW-ESP32C5` → `GW-ESP32S3` |
| [src/main.rs](../src/main.rs) | 启动日志 `ESP32-S3R8 IoT Gateway starting...` |

### W5500 vs DM9051 关键差异

- **SPI 帧格式**：W5500 用 2 字节地址段 (bit15=R/W)，DM9051 用 1 bit R/W + 7 bit reg
- **ESP-IDF 驱动**：W5500 需作为外部 IDF Component 添加 (`espressif/w5500`)，DM9051 是内置组件
- **PHY 地址**：W5500 内部 PHY 固定地址 0，DM9051 PHY 地址 1
- **SPI 配置**：W5500 `command_bits=0, address_bits=0`，DM9051 `command_bits=1, address_bits=7`

### 阶段三遗留 TODO

#### [ethernet/w5500.rs](../src/ethernet/w5500.rs) - W5500 驱动

- [ ] **`esp_eth_mac_new_w5500` / `esp_eth_phy_new_w5500` / `eth_w5500_config_t` binding 可用性**：需验证在 esp-idf-sys 0.35 + `idf_component.yml` 声明 `espressif/w5500` 后是否能正常生成 binding
- [ ] **`SPI_DMA_CH_AUTO` 常量**：暂用 `1`，需确认 esp_idf_sys 0.35 是否暴露该常量
- [ ] **`IP_EVENT` binding 形态**：假设为 `&[u8]`，通过 `.as_ptr()` 转 `esp_event_base_t`；若为 extern static 需调整
- [ ] **`IP_EVENT_ETH_GOT_IP` 转 `i32` 比较方式**：需校验
- [ ] **`ip_event_got_ip_t` 字段布局**：`ip_info.ip/gw/netmask.addr` 以 binding 实际为准
- [ ] **`esp_eth_new_netif_glue` 返回类型**：v5.x 返回 `esp_eth_netif_glue_t*`，`esp_netif_attach` 第二参数为 `void*`
- [ ] **`heartbeat_once()` 真实实现**：当前返回 `true` 占位，需实现 ICMP ping 网关或 TCP connect 检测

---



## ★ 阶段二已完成：系统配置区（2026-07-07）

将分散的设备配置（SN/MAC/IP/网关/RS485/BLE/SN 等）整合为统一 SystemConfig 结构，提供 Modbus 寄存器映射 + BLE AT 命令 + NVS 持久化 + 应用生效入口。

### 新增模块

| 路径 | 行数 | 说明 |
|------|-----|------|
| [src/device/system_config.rs](../src/device/system_config.rs) | ~550 | SystemConfig 结构 + 编解码 + NVS 持久化 + Modbus 寄存器读写映射 |
| [src/ble_at/cfg_handlers.rs](../src/ble_at/cfg_handlers.rs) | ~370 | 15 个 AT+CFG* 命令处理函数 |

### 修改文件

| 路径 | 改动 |
|------|------|
| [src/config.rs](../src/config.rs) | regs 模块新增 CFG_BASE/END/SN/NAME/HW/FW/CFG_VER/APPLY/RESET/MAC/DHCP/IP/MASK/GW/DNS/BLE_MAC/BLE_NAME/MESH/RS485_* 常量 |
| [src/bus.rs](../src/bus.rs) | Bus 新增 `cfg: SystemConfig` 字段；`read_hold_reg/write_hold_reg` 扩展 CFG_BASE..CFG_END 分支，处理 Ok/Apply/Reset/NotFound |
| [src/device/mod.rs](../src/device/mod.rs) | 新增 `APPLY_CONFIG_REQUEST` 标志 + `request_apply_config()` / `apply_config_sync()` API；`init()` 同时加载 SystemConfig；`watch_loop` 处理 APPLY_CONFIG_REQUEST；新增 `apply_config()` 实现（持久化 + 运行时应用 TODO） |
| [src/ble_at/parser.rs](../src/ble_at/parser.rs) | 新增 14 个 CFG* 命令路由分发 |
| [src/ble_at/mod.rs](../src/ble_at/mod.rs) | 新增 `pub mod cfg_handlers;` 声明 |

### Modbus 寄存器布局新增（0x0200-0x025F）

```
0x0200-0x020F  SN 号 (16 字, ASCII 32 字符, big-endian)
0x0210-0x0217  设备名称 (8 字, ASCII 16 字符)
0x0218         硬件版本 (BCD, RW)
0x0219         固件版本 (BCD, RW)
0x021A         配置版本 (每次 APPLY 自增, RW)
0x021B         APPLY (WO, 写 0xB5B5 → 持久化 + 触发应用)
0x021C         RESET_DEFAULT (WO, 写 0xD5D5 → 恢复默认)
0x0220-0x0222  以太网 MAC (3 字 = 6 字节, big-endian)
0x0223         DHCP (0=静态, 1=DHCP)
0x0224-0x0225  IP 地址 (2 字 = 4 字节, big-endian)
0x0226-0x0227  子网掩码
0x0228-0x0229  网关
0x022A-0x022B  DNS
0x0230-0x0232  BLE MAC (3 字)
0x0233-0x0236  BLE 名称 (4 字 = 8 字符)
0x0237         BLE Mesh 启用 (0/1)
0x0240-0x024F  RS485 #0 (16 字, 实际用 6 字)
0x0250-0x025F  RS485 #1 (16 字)
               通道内偏移:
                 +0 波特率 (÷100, 9600→96, 115200→1152)
                 +1 数据位 (7/8)
                 +2 停止位 (1/2)
                 +3 校验 (0=None 1=Odd 2=Even)
                 +4 从站地址 (0=主站)
                 +5 模式 (0=Master 1=Slave 2=Gateway)
                 +6..+15 保留
```

### BLE AT 命令集新增（15 个 CFG* 命令）

```
AT+CFGSN[=<sn>]                       设置/读 SN (32 字符)
AT+CFGNAME[=<name>]                   设置/读设备名称 (16 字符)
AT+CFGIP[=<ip>,<mask>,<gw>[,<dns>]]   设置/读网络配置
AT+CFGDHCP=<0|1>                      启用/禁用 DHCP
AT+CFGMAC                             读以太网 MAC
AT+CFGBTMAC                           读蓝牙 MAC
AT+CFGBTNAME[=<name>]                 设置/读 BLE 名称 (8 字符)
AT+CFGMESH=<0|1>                      启用/禁用 BLE Mesh
AT+CFG485=<idx>,<baud>,<data>,<stop>,<parity>,<slave>,<mode>   配置 RS485
AT+CFG485=<idx>                       读 RS485 配置
AT+CFGAPPLY                           应用配置 (持久化 + 触发应用)
AT+CFGRESET                           恢复默认配置
AT+CFGINFO                            列出所有配置
AT+CFGREAD=<addr>                     按 Modbus 地址读 U16 (0x0200-0x025F)
AT+CFGWRITE=<addr>,<value>            按 Modbus 地址写 U16
```

### 编码约定

- **IPv4**：4 字节 → 2 U16，big-endian（`192.168.1.100` → `[0xC0A8, 0x0164]`）
- **MAC**：6 字节 → 3 U16，big-endian
- **ASCII**：2 字符/U16，big-endian（SN/Name/BLE Name）
- **波特率**：÷100 存储（9600→96, 115200→1152）
- **NVS blob**：固定 128 字节布局，magic=0x4757_4346（"GWCF"），namespace=`gateway`，key=`sys_cfg`

### 阶段二遗留 TODO

#### [device/system_config.rs](../src/device/system_config.rs) - 系统配置

- [x] **运行时应用已实现**：`apply_config()` 持久化后 200ms 软重启（`esp_restart`），让新配置在启动时完整生效。设计说明：网络/RS485/BLE 配置运行时切换风险高（资源句柄所有权问题），软重启是最稳妥方案
- [x] **eth_mac/ble_mac 默认值**：新增 `fill_hw_macs()` 在 `init()` 中从 `esp_read_mac()` 读取 ESP_MAC_ETH/ESP_MAC_BT 自动填充
- [x] **hw_version/fw_version 同步**：新增 `fw_version_from_cargo()` 从 `env!("CARGO_PKG_VERSION")` 解析 `(major << 8) | minor`，`init()` 中强制同步。`hw_version` 仍需硬件识别（GPIO 或烧录时配置）
- [ ] **配置版本回写**：Modbus `write_hold_reg` 在 Apply 时 `cfg_version.wrapping_add(1)`，需校验 NVS 持久化的 cfg_version 与 RAM 一致
- [ ] **blob 大小限制**：当前 128 字节，NVS blob 单 key 上限约 4000 字节，OK；但 `CFG_BLOB_SIZE` 与字段偏移强耦合，新增字段需同步更新 OFF_* 常量

#### [ble_at/cfg_handlers.rs](../src/ble_at/cfg_handlers.rs) - AT 命令

- [ ] **`with_cfg` / `with_cfg_mut` 容错**：bus 锁超时返回 defaults()，可能掩盖真实故障。建议返回 ERROR 而非静默回退
- [ ] **`AT+CFG485` 参数解析**：`parse_u16` 解析 `parts[1]` 后又调用 `parse_u16(parts[2])` 等重复解析，可简化
- [ ] **`AT+CFGRESET` 直接恢复默认**：当前立即覆盖 `*c = defaults()`，未持久化。已调用 `apply_config_sync()` 持久化，OK
- [ ] **`AT+CFGWRITE` 写 APPLY 字段**：返回 `apply requested` 后由监听线程异步应用，AT 响应已返回但应用未完成。建议 `AT+CFGAPPLY` 改为同步等待

#### 端到端验证

- [ ] Modbus 写 0x0200=0x4142 → Modbus 读 0x0200 应返回 0x4142（SN 写入）
- [ ] Modbus 写 0x0224=0xC0A8 + 0x0225=0x0164 → `AT+CFGINFO` 应显示 `ip=192.168.1.100`
- [ ] Modbus 写 0x021B=0xB5B5 → 等待 100ms → 重启 → `AT+CFGINFO` 应保留所有修改
- [ ] Modbus 写 0x021C=0xD5D5 → 重启 → `AT+CFGINFO` 应显示默认配置
- [ ] BLE AT `AT+CFGSN=ABC123` → `AT+CFGSN` 应返回 `OK ABC123`
- [ ] BLE AT `AT+CFGIP=10.0.0.50,255.255.255.0,10.0.0.1` → `AT+CFGIP` 应返回新配置
- [ ] BLE AT `AT+CFG485=0,115200,8,1,0,1,1` → `AT+CFG485=0` 应返回新配置
- [ ] BLE AT `AT+CFGAPPLY` → 重启 → `AT+CFGINFO` 应保留所有修改

---

## ★ 阶段一已完成：协议存储闭环（2026-07-07）

### 新增模块

| 路径 | 行数 | 说明 |
|------|-----|------|
| [src/device/mod.rs](../src/device/mod.rs) | ~310 | NVS 全局 + ProtoStore + 监听线程 + AT/Modbus 读写 API |
| [src/ble_at/mod.rs](../src/ble_at/mod.rs) | ~150 | GATT 服务接口 + 输入输出缓冲区 + 处理线程 |
| [src/ble_at/parser.rs](../src/ble_at/parser.rs) | ~110 | AT 命令字符串解析 |
| [src/ble_at/handlers.rs](../src/ble_at/handlers.rs) | ~170 | 10 个 AT 命令处理函数 |

### 修改文件

| 路径 | 改动 |
|------|------|
| [src/config.rs](../src/config.rs) | regs 模块新增 PROTO_BASE/END/COMMIT/RELOAD/VERSION/LENGTH/STATUS/MAGIC 常量 |
| [src/bus.rs](../src/bus.rs) | 新增 `ProtoStore` 结构 + `Bus.proto` 字段 + `read_hold_reg/write_hold_reg` 扩展协议区 |
| [src/main.rs](../src/main.rs) | 添加 `mod device` + `mod ble_at` + 启动顺序加入 `device::init()` 和 `ble_at::start()` |

### Modbus 寄存器布局新增

```
0x4000-0x45DB  协议数据区 (1500 个 U16, RW)
0x45DC         COMMIT (WO, 写 0xC5C5 → 异步持久化到 NVS)
0x45DD         RELOAD (WO, 写 0xA5A5 → 异步从 NVS 重载)
0x45DE         VERSION (RW, 用户自定义协议版本)
0x45DF         LENGTH  (RW, 有效协议长度, U16 数)
0x45E0         STATUS  (RO, 0=空闲 1=写入中 2=加载中 3=校验失败)
0x45E1         MAGIC   (RO, 0x4757 = 'GW')
```

### BLE AT 命令集

```
AT+READ=<addr>                 → OK <value>,0xXXXX
AT+WRITE=<addr>,<value>         → OK
AT+BULKR=<start>,<len>          → OK 0xXXXX,0xXXXX,...
AT+BULKW=<start>,<v1>,<v2>,...  → OK <N> words
AT+COMMIT                       → OK committed
AT+RELOAD                       → OK reloaded
AT+INFO                         → OK cap=1500,ver=...,len=...
AT+STATUS                       → OK uptime=...,fw=...,di=...
AT+RESET                        → OK resetting in 100ms
AT+VERSION                      → OK <name> v<x.x>
```

### 阶段一遗留 TODO

#### [device/mod.rs](../src/device/mod.rs) - 协议存储

- [x] **`EspNvsPartition::clone()` 已验证**：esp_idf_svc 0.50 中 `EspNvsPartition` 内部 `Arc`，clone 合法
- [x] **`EspNvs::set_blob` 签名已验证**：`set_blob(&mut self, key, &[u8]) -> EspResult<()>`，需要 `&mut self`
- [x] **`EspNvs::get_blob` 签名已验证并修复**：返回 `Result<Option<&[u8]>, EspError>`（不是 `usize`），已改为 match Some/None 分支
- [x] **NVS blob 单 key 大小限制已验证**：上限 `min(508000, 0.976 × partition_size - 4000)` 字节，3000 字节 OK
- [x] **`EspNvsPartition::take()` 单次限制已验证**：二次调用返回 `Err(ESP_ERR_INVALID_STATE)`，不 panic。blemesh 通过 `device::nvs_partition()` clone 借用
- [x] **`AppError::Config` vs `AppError::Other` 已统一**：device/mod.rs 中所有 `AppError::Other` 改为 `AppError::Config`
- [ ] **ProtoStore 占用 Bus 大小**：3000 字节常驻 SRAM，对 384KB SRAM 无压力，但 `Bus::clone()` 会复制整个数组（如果有调用方）
- [x] **commit 期间总线锁占用已加去抖**：`watch_loop` 在处理 COMMIT 前检查 `bus.proto.status == 1`，RELOAD 前检查 `status == 2`，已在写入中/加载中则跳过本次请求

#### [ble_at/mod.rs](../src/ble_at/mod.rs) - GATT 服务

- [ ] **GATT 服务注册未实现**：当前仅 AT 命令处理线程可用，无实际 BLE GATT 服务。需用 `esp_idf_svc::ble::gatt::Server` 注册：
  - Service UUID `0xFF01`
  - RX Char UUID `0xFF02` (Write)
  - TX Char UUID `0xFF03` (Notify)
- [ ] **`feed_data()` / `take_response()` 调用方**：需在 GATT write/notify 回调中调用，当前无调用方
- [ ] **BLE Mesh + GATT 共存**：sdkconfig 已开 `BT_GATTS_ENABLE`，但需实测验证
- [ ] **MTU 协商**：单条 AT 命令可能超 23 字节（默认 MTU），需主动请求 MTU=250
- [ ] **AT 命令分片**：BULKW 50 个 U16 约 200 字节，超过 MTU 需分片重组，当前 `RX_BUFFER` 已支持 `\n` 终结符
- [x] **`std::sync::Mutex` 与 `parking_lot::Mutex` 已统一**：ble_at/mod.rs 改用 `parking_lot::Mutex`，`lock()` 不返回 Result
- [ ] **响应缓冲区溢出**：`TX_BUFFER: String<512>`，BULKR 返回 50 个 U16 约 250 字节，够用；但 200 字节需校验

#### [ble_at/parser.rs](../src/ble_at/parser.rs) - AT 解析

- [x] **大小写处理已优化**：`process()` 改用 `eq_ignore_ascii_case` 链，避免 `to_uppercase()` 堆分配
- [ ] **数值解析**：`parse_u16` 不支持负数，不支持二进制，需补充？
- [ ] **参数分隔**：当前用 `splitn(2, ',')`，BULKW 多参数需 split(',')，已正确

#### [ble_at/handlers.rs](../src/ble_at/handlers.rs) - AT 处理

- [x] **`AT+RESET` 异步复位延时已改**：从 100ms 改为 200ms，给 AT 响应发送留足时间
- [x] **`AT+STATUS` 字段已补全**：从只返回 ai0/ao0 扩展为返回全部 6 AI + 4 AO（`ai=0x..,0x..,0x..,0x..,0x..,0x..,ao=0x..,0x..,0x..,0x..`）
- [x] **`AT+INFO` 响应格式已优化**：新增 `proto_base=0x{:04X}` 字段

#### [bus.rs](../src/bus.rs) - 总线扩展

- [ ] **`ProtoStore` derive(Clone) 但 Bus derive(Default)**：Bus 没有 Clone，无法整体克隆，OK
- [ ] **`read_hold_reg` 中 `regs::PROTO_MAGIC` 调用 `crate::device::PROTO_MAGIC`**：跨模块常量引用，OK；但要注意如果 device 模块未编译（feature 关闭）会报错
- [ ] **`write_hold_reg` 调用 `crate::device::request_commit()`**：跨模块调用，OK；但 device 模块必须启用，否则编译失败

#### [main.rs](../src/main.rs) - 启动顺序

- [ ] **device::init() 调用位置**：当前在 HAL 初始化之后、ethernet 之前。NVS 必须在调用任何使用 nvs 的模块（blemesh）之前初始化，OK
- [ ] **ble_at::start()** 调用位置：当前在 blemesh::start() 之后。GATT 服务可能与 Mesh 控制器初始化顺序冲突，需实测
- [x] **错误处理已加回退**：`device::init()` 失败时不再退出 main，改为日志告警 + 使用空 ProtoStore（bus 已有默认值）继续启动

#### 端到端验证

- [ ] Modbus 写 0x4000=0x1234 → Modbus 读 0x4000 应返回 0x1234
- [ ] Modbus 写 0x45DC=0xC5C5 → 等待 100ms → Modbus 读 0x45E0 应返回 0（写入完成）
- [ ] 重启设备 → Modbus 读 0x4000 应返回 0x1234（已持久化）
- [ ] Modbus 写 0x45DD=0xA5A5 → 等待 100ms → Modbus 读 0x4000 应返回最近一次 commit 的值
- [ ] BLE AT `AT+WRITE=100,0xABCD` → `AT+READ=100` 应返回 `OK 43981,0xABCD`
- [ ] BLE AT `AT+COMMIT` → 重启 → `AT+READ=100` 应返回 0xABCD
- [ ] BLE AT `AT+INFO` → 应返回 `cap=1500,ver=0,len=0,dirty=false,status=0,magic=0x4757`

---

## HAL 模块

### [hal/adc.rs](../src/hal/adc.rs) - ADC 采样
- [ ] ESP32-S3 ADC channel ↔ GPIO 映射在 esp-idf-hal 0.45 中可能用 const generic 绑定 (`AdcChannelDriver<'_, _, N>`)，当前用 `AdcChannelDriver<'static, AnyInputPin>` 需验证
- [ ] ADC1 默认分辨率/衰减是否需在 `AdcConfig` 中显式指定
- [ ] ADC calibration API 未启用，实际采样值偏差需校准

### [hal/ledc.rs](../src/hal/ledc.rs) - LEDC PWM 输出
- [ ] `LedcDriver::new` / `LedcChannelDriver::new` 可能需要 `LSTimer<N>` / `LSChannel<N>` 标记类型而非运行时 u8
- [ ] 若需要 const generic，应改为 4 个独立字段（每个 channel 一个 LSChannel 类型）
- [ ] `set_duty` + `update_duty` 调用顺序需验证

### [hal/gpio.rs](../src/hal/gpio.rs) - GPIO 控制
- [ ] `PinDriver` 的 mode 参数（`Pull::Down`、`Output::default()`）实际签名需验证
- [x] `eth_reset()` 拉低 50ms + 等待 50ms 已被 ethernet::w5500 复用，W5500 数据手册要求 RSTBW > 500us + PLL 锁定 1ms，50ms 足够

### [hal/pins.rs](../src/hal/pins.rs) - 引脚分配
- [x] ESP32-S3 有 45 个 GPIO (GPIO0~48)，[config.rs](../src/config.rs) 中引脚分配已重新规划，所有引脚在可用范围内 (见 [pinmap.md](pinmap.md))
- [ ] `Pins` 字段在 esp-idf-hal 0.45 中是否完整支持 ESP32-S3

## 以太网模块

### [ethernet/w5500.rs](../src/ethernet/w5500.rs) - W5500 驱动 (替代 dm9051.rs)
- [x] **SPI 资源冲突已修复**：删除 `hal/spi_bus.rs`，SPI2_HOST 由 ethernet 模块独占初始化和管理
- [x] **ETH_RST 重复 gpio_config 已修复**：删除本模块 `reset_w5500()`，复用 `hal.gpio.eth_reset()`
- [ ] `SPI_DMA_CH_AUTO` 常量是否在 esp_idf_sys 0.35 暴露（暂用 `1`）
- [ ] `IP_EVENT` binding 形态：假设为 `&[u8]`，通过 `.as_ptr()` 转 `esp_event_base_t`；若为 extern static 需调整
- [ ] `IP_EVENT_ETH_GOT_IP` 转 `i32` 比较方式
- [ ] `ip_event_got_ip_t` 字段布局（`ip_info.ip/gw/netmask.addr`）以 binding 实际为准
- [ ] `esp_eth_new_netif_glue` 返回类型（v5.x 返回 `esp_eth_netif_glue_t*`，`esp_netif_attach` 第二参数为 `void*`）
- [ ] `heartbeat_once()` 真实实现：列出 3 种方案（`esp_netif_get_ip_info` 取 gw + `esp_ping` ICMP / TCP connect 网关端口），当前返回 `true` 占位
- [ ] 替代方案：可用 `EspSystemEventLoop::subscribe::<IpEvent,_>` 替代 C API 注册，但 IpEvent 在 esp_idf_svc 0.50 是否覆盖 ETH_GOT_IP 待确认

## RS485 模块

### [rs485/port.rs](../src/rs485/port.rs) - RS485 端口
- [ ] `uart_config_t` 的 `source_clk` 字段在 ESP32-S3 上应使用 `UART_SCLK_DEFAULT` 还是 `UART_SCLK_APB`
- [x] `uart_set_pin` 的 RTS 引脚参数：硬件把 RTS 接到 DE 引脚 (GPIO42 / GPIO7)
- [ ] `uart_driver_install` 第 4 个参数 (queue size) 当前为 0，是否需要事件队列
- [x] `read()` 函数的帧结束判断已改进：两阶段读取 + `uart_get_buffered_data_len` + 3.5 字符静默 + `uart_set_rx_timeout(port, 3)` (见工业可靠性改进)
- [x] ESP32-S3 有 3 个 UART，RS485 #1 改用 UART2 (GPIO17/18)，不再与 UART0 下载串口复用

## Modbus 模块

### [modbus/rtu_master.rs](../src/modbus/rtu_master.rs) - RTU 主站
- [ ] 轮询表硬编码 1 条示例 (`slave=1, FC=03, addr=0, count=8`)，**应从 NVS 加载用户配置**
- [x] 收到的寄存器数据已写回 `bus.proto.data`：`PollItem` 新增 `dest_reg` 字段，`poll_once()` 收到数据后按 dest_reg 写回 bus 并置 dirty
- [x] 失败重试机制已实现：新增 `poll_with_retry()`，最多重试 3 次，间隔 100ms

### [modbus/rtu_slave.rs](../src/modbus/rtu_slave.rs) - RTU 从站
- [ ] `read()` 超时 1000ms，可能导致线程长时间阻塞
- [ ] 异常响应中 `slave` 参数已包含在 `out` 中，但 `exc_response` 返回 `[func|0x80, code]` 只有 2 字节，需确认外层拼装逻辑

### [modbus/tcp_server.rs](../src/modbus/tcp_server.rs) - TCP Server
- [x] keepalive 已实现：通过 `set_read_timeout` + `WouldBlock` 检测，空闲超 RX_TIMEOUT_MS 的连接主动关闭，释放 MAX_CONNECTIONS 名额
- [x] `read_exact` 失败已区分：`WouldBlock` = keepalive 超时，`UnexpectedEof` = 对端正常关闭，其他 = 读错误，均记日志后关闭
- [ ] 无 MBAP transaction_id 一致性校验

## BLE Mesh 模块

### [blemesh/bindings.rs](../src/blemesh/bindings.rs) - C API 绑定
- [ ] **`esp_ble_mesh_init` 签名**：v5.5 可能改为单参数 `esp_ble_mesh_init_cfg_t *init_cfg`，需核对调整
- [ ] **`esp_bt_controller_config_t` 字段**：用 `..Default::default()` 构造，需确认 bindgen 是否 derive Default，且需补齐 `BT_CONTROLLER_INIT_CONFIG_DEFAULT()` 等价字段
- [ ] **`ModelOpParam` 结构偏移**：联合体 `model_operation` 与 `esp_ble_mesh_msg_ctx_t` 字段顺序为近似值，`ctx_recv_op`/`length`/`msg` 偏移需按实际头文件修正
- [ ] **事件枚举值**：`EVT_MODEL_OPERATION=0x00` 为占位，需对齐 `esp_ble_mesh_cb_event_t`
- [ ] **`esp_ble_mesh_model_publish` 调用**：`send_status`/`send_onoff_set` 仅占位未实际调用
- [ ] **Provisioner**：`add_app_key` / `model_bind_app_key` / 添加未配网设备未实现
- [ ] **OOB / dev_key / device UUID**：均为占位，应从 nvs 读出或生成
- [ ] **generic client 回调签名**：`esp_ble_mesh_generic_client_cb_t` 与 mesh_cb 不同，当前复用 `MeshCb` 类型需修正

### [blemesh/models.rs](../src/blemesh/models.rs) - 模型定义
- [ ] GPIO 物理输出由 `io` 任务消费总线状态刷新，未在回调中直接驱动（回调为 C fn 无法捕获 `hal`）
- [ ] 多个 OnOff Server 实例（当前只支持 1 个 DO）

## 配置文件

### [config.rs](../src/config.rs) - 引脚分配
- [x] **引脚分配已重新规划为 ESP32-S3R8**：`DO_PINS=[8,9,16,38,39,45,46,48]`，`AO_CHANNELS=[(0,8),(1,9),(2,16),(3,38)]`，均在 ESP32-S3 GPIO 0-48 范围内（参考 [pinmap.md](pinmap.md)）
- [x] RS485 #1 已改用 UART2 (GPIO17/18)，UART0 专用于下载/日志
- [x] 4-20mA 标定参数已参数化：新增 `config::ai_calib` 模块（`ADC_MAX/MA_MIN/MA_MAX`），`channel/ai.rs` 改用 `ai_calib::*` 替代硬编码

### [sdkconfig.defaults](../sdkconfig.defaults)
- [x] 已迁移到 ESP32-S3R8 + 8MB Octal PSRAM (`CONFIG_SPIRAM_SIZE=8388608`, `CONFIG_SPIRAM_MODE_OCT=y`, `CONFIG_SPIRAM_SPEED_80M=y`)
- [x] BLE 5.0 + BLE Mesh: `CONFIG_BT_BLUEDROID=y` (BLE Mesh 需要 Bluedroid, NimBLE 不支持 Mesh)
- [x] Watchdog: `CONFIG_ESP_TASK_WDT_INIT=y`, `CONFIG_ESP_TASK_WDT_TIMEOUT_S=10`, `CONFIG_ESP_INT_WDT=y`
- [x] UART0: `CONFIG_ESP_CONSOLE_UART_NUM=0`, `CONFIG_ESP_CONSOLE_UART_BAUDRATE=115200`
- [ ] `CONFIG_BT_BLE_MESH_MAX_PROV_NODES=10` 节点数是否够用（按实际网络规模调整）
- [ ] PSRAM 8MB Octal: `CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=16384` 内部 SRAM 保留阈值是否合适

## 后续优化建议

- [ ] **OTA 升级**：分区表已预留 ota_0/ota_1，需实现 esp_ota_* API 封装
- [ ] **NVS 配置**：把 Modbus 轮询表、设备地址、波特率等存入 NVS，支持运行时修改
- [x] **看门狗**：`CONFIG_ESP_TASK_WDT_INIT=y` + main_loop 100ms 喂狗，关键任务均通过 `health::subscribe_wdt()` 订阅
- [x] **日志分级**：运行时通过 Modbus 寄存器 0x0106 调节 (0=Err 1=Warn 2=Info 3=Debug 4=Trace)
- [x] **健康检查**：`TaskHb` 心跳 + `check_all()` 停滞检测 + 0x0105 任务健康位图 (部分实现)
- [ ] **Modbus 网关模式**：实现 RTU ↔ TCP 透传（RTU 收到 → TCP 转发；TCP 收到 → RTU 转发）

## 已完成的工业可靠性改进（阶段一遗留）

- [x] **看门狗机制**：ESP-IDF Task Watchdog (10s 超时, 100ms 喂狗, main_loop + 关键任务订阅)
- [x] **复位原因持久化**：`esp_reset_reason()` + NVS `rst_cnt` 启动时记录
- [x] **任务心跳**：`TaskHb` AtomicU32 无锁计数 + `check_all()` 停滞阈值累积
- [x] **Modbus RTU 帧间静默**：3.5 字符时间 (9600bps≈4ms) + `uart_set_rx_timeout(port, 3)`
- [x] **Rs485Port::read bug 修复**：两阶段读取 + `uart_get_buffered_data_len` 替代不存在的 API
- [x] **AI 滑动平均位移优化**：AVG_SHIFT=3 用位移替代除法
- [x] **CfgApply 软重启延时**：200ms → 500ms (给 AT 响应留足时间)
- [x] **运行时日志级别调节**：Modbus 寄存器 0x0106 + `log::set_max_level` 即时生效
