# 开发指南 (Development Guide)

> ESP32-S3R2 工业网关项目 — Rust + esp-idf-hal + std::thread

本文档面向新加入项目的开发者，覆盖从环境搭建、编译烧录、架构理解到典型扩展开发（新硬件版本、Modbus 寄存器、AT 命令、NVS 持久化、OTA）的全流程。

- 项目路径：工程根目录
- Rust edition：2024
- ESP-IDF：v5.5.4（通过 `esp_idf_sys` 绑定）
- esp-idf-hal 0.46 + esp-idf-svc 0.52
- 目标芯片：ESP32-S3R2（Xtensa LX7 双核 240MHz，512KB 物理 SRAM，2MB Quad PSRAM）

---

## 目录

1. [环境搭建](#1-环境搭建)
2. [编译构建](#2-编译构建)
3. [烧录调试](#3-烧录调试)
4. [项目架构](#4-项目架构)
5. [硬件版本扩展指南](#5-硬件版本扩展指南)
6. [新增 Modbus 寄存器指南](#6-新增-modbus-寄存器指南)
7. [新增 AT 命令指南](#7-新增-at-命令指南)
8. [NVS 持久化指南](#8-nvs-持久化指南)
9. [OTA 升级开发](#9-ota-升级开发)
10. [调试技巧](#10-调试技巧)

---

## 1. 环境搭建

### 1.1 系统依赖

**macOS**：
```bash
brew install ninja cmake libusb
```

**Ubuntu**：
```bash
sudo apt-get install -y gcc g++ ninja-build cmake libssl-dev pkg-config libusb-1.0-0-dev
```

### 1.2 安装 ESP-IDF v5.5.4

```bash
# 克隆 ESP-IDF（一次性，注意递归子模块）
git clone -b v5.5.4 --recursive https://github.com/espressif/esp-idf.git
cd esp-idf
./install.sh esp32s3     # 注意：esp32s3（不是 esp32c5）
. ./export.sh            # 每次打开新终端需执行
```

### 1.3 安装 Xtensa Rust 工具链

`xtensaespidf` target 不在标准 rustup 中，需通过 `espup` 安装：

```bash
# 1. 安装基础 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. 安装 espup（Xtensa 工具链管理器）
cargo install espup
espup install
. $HOME/export-esp.sh     # 每次新终端执行（激活 Xtensa 工具链）

# rust-toolchain.toml 已固定 channel = "nightly"
# .cargo/config.toml 已固定 target = "xtensaespidf"
```

### 1.4 安装链接器与烧录工具

```bash
cargo install ldproxy        # 链接器代理，配合 xtensa-esp32s3-elf-gcc
cargo install cargo-espflash # 烧录工具（可选，推荐）
cargo install espflash       # espflash CLI（runner 已配置）
```

### 1.5 验证环境

```bash
which xtensa-esp32s3-elf-gcc   # 应输出路径
cargo --version                # 应可用
espflash --version             # 应可用
```

### 1.6 项目预配置文件

项目已通过以下文件预配置，无需手动设置：

| 文件 | 作用 |
|------|------|
| `.cargo/config.toml` | `target = "xtensaespidf"`，linker = `xtensa-esp32s3-elf-gcc`，runner = `scripts/cargo-runner.sh`（拒绝测试 ELF，并强制完整分区参数） |
| `rust-toolchain.toml` | `channel = "nightly"`，`targets = ["xtensaespidf"]`，含 `rust-src`/`rustfmt`/`clippy` |
| `sdkconfig.defaults` | ESP32-S3R2 + 2MB Quad PSRAM + BLE + W5500 + Task Watchdog |
| `partitions.csv` | 8MB Flash 分区表（factory 3MB + ota_0/ota_1 各 2.25MB） |
| `build.rs` | embuild 编排 ESP-IDF，设置 `feature_*` cfg，feature 互斥校验 |
| `idf_component.yml` | 声明 W5500 外部 IDF Component 依赖 `espressif/w5500: ^1.0.0` |

> **首次编译会下载 ESP-IDF v5.5.4 并构建 Xtensa 工具链，耗时 15–40 分钟。**

---

## 2. 编译构建

### 2.1 硬件版本（编译期 feature flag，互斥）

| 版本 | 命令 | DI | DO | IO 方式 |
|------|------|----|----|---------|
| Default | `cargo build` | 8 | 8 | GPIO 直驱 |
| F3 | `cargo build --features f3` | 16 | 16 | I2C MCP23017（2 片） |
| F4 | `cargo build --features f4` | 48 | 16 | I2C MCP23017（4 片） |

F3/F4 互斥由 `build.rs::check_feature_compatibility()` 强制校验，同时启用会 panic 中断编译。启用 `f3`/`f4` 后，`io-di-do` 自动启用，I2C 总线占用 GPIO21(SDA)/GPIO33(SCL)。

### 2.2 功能 feature

| feature | 作用 | 默认 |
|---------|------|------|
| `adc-continuous` | ADC Continuous/DMA 模式（ESP32-S3 硬件加速） | ✓ |
| `ble-mesh` | BLE Mesh | ✓ |
| `ethernet-w5500` | W5500 以太网 | ✓ |
| `modbus-rtu` | Modbus RTU（主站+从站） | ✓ |
| `modbus-tcp` | Modbus TCP Server | ✓ |
| `ai-ao` | AI/AO 通道 | ✓ |
| `io-di-do` | DI/DO 任务 | ✓ |
| `wifi` | Wi-Fi（与 BLE 共存，作冗余链路） | ✗ |

`Cargo.toml` 的 `default` 字段定义了默认启用项：
```toml
default = ["adc-continuous", "ble-mesh", "ethernet-w5500",
           "modbus-rtu", "modbus-tcp", "ai-ao", "io-di-do"]
```

### 2.3 feature 互斥规则（`build.rs`）

- `modbus-rtu` 与 `modbus-tcp` **不能同时关闭**（至少一个通信通道），否则 panic。
- `ethernet-w5500` 与 `wifi` 同时关闭时仅警告（仅 BLE 可用）。
- `f3` 与 `f4` **互斥**，同时启用会 panic。

### 2.4 常用编译命令

```bash
cd /Users/ling/Workspace/idf

# Debug 构建（panic=unwind，便于 backtrace 调试）
cargo build

# Release 构建（opt-level=3 + fat LTO + panic=abort，体积最小）
cargo build --release

# Default 硬件版本（8 DI + 8 DO）
cargo build --release

# F3 硬件版本（16 DI + 16 DO，I2C 扩展）
cargo build --release --features f3

# F4 硬件版本（48 DI + 16 DO，I2C 扩展）
cargo build --release --features f4

# 自定义 feature 组合（禁用默认，显式启用）
cargo build --release --no-default-features \
    --features "ble-mesh,ethernet-w5500,modbus-rtu,modbus-tcp,ai-ao,io-di-do"

# F4 + Wi-Fi 共存（启用 COEX 软件共存）
cargo build --release --features "f4,wifi"

# 回退到 ADC OneShot 模式（不用 DMA）
cargo build --release --no-default-features \
    --features "ble-mesh,ethernet-w5500,modbus-rtu,modbus-tcp,ai-ao,io-di-do"
```

### 2.5 profile 说明

| profile | opt-level | LTO | panic | 用途 |
|---------|-----------|-----|-------|------|
| `dev` | 2 | false | unwind | 日常调试，支持 backtrace |
| `release` | 3 | fat | abort | 量产，体积最小，性能最优 |

### 2.6 清理与重建

```bash
# 修改 sdkconfig.defaults 后，需删除 build 缓存
rm -rf build sdkconfig
cargo build --release

# 完整清理（含 embuild 缓存，下次构建会很慢）
rm -rf build sdkconfig target .embuild
```

---

## 3. 烧录调试

### 3.1 串口识别

```bash
# macOS / Linux
ls /dev/cu.usbserial-* /dev/ttyUSB* 2>/dev/null
# 假设识别为 /dev/cu.usbserial-XXXX
```

UART0 默认 TX=GPIO43，RX=GPIO44，115200 8N1，与下载/日志共用。

### 3.2 烧录方式

#### 方式 1：cargo-espflash（推荐）

```bash
# 烧录 + 监视器（最常用）
cargo espflash --release /dev/cu.usbserial-XXXX --monitor

# 指定波特率（默认 460800，可提速到 921600）
cargo espflash --release /dev/cu.usbserial-XXXX --baud 921600 --monitor

# 仅烧录不监视
cargo espflash --release /dev/cu.usbserial-XXXX

# 指定芯片（通常自动识别）
cargo espflash --release --chip esp32s3 /dev/cu.usbserial-XXXX --monitor
```

#### 方式 2：cargo run（用 .cargo/config.toml 的 runner）

```bash
# runner = "scripts/cargo-runner.sh"
cargo run --release
```

#### 方式 3：esptool.py（备用）

```bash
cargo build --release
FIRMWARE=target/xtensaespidf/release/gateway
esptool.py --chip esp32s3 --port /dev/cu.usbserial-XXXX --baud 921600 \
    write_flash 0x0 bootloader/bootloader.bin \
    0x10000 $FIRMWARE \
    0x8000 partitions.csv
```

#### 方式 4：idf.py（需 ESP-IDF 环境）

```bash
. /path/to/esp-idf/export.sh
idf.py -p /dev/cu.usbserial-XXXX flash monitor
```

### 3.3 修改分区表后重新烧录

```bash
esptool.py --chip esp32s3 --port /dev/cu.usbserial-XXXX \
    write_flash 0x8000 partitions.csv
```

### 3.4 串口监视器

```bash
# macOS / Linux
python -m serial.tools.miniterm /dev/cu.usbserial-XXXX 115200

# 或用 screen（退出：Ctrl+A 然后 K 然后 Y）
screen /dev/cu.usbserial-XXXX 115200

# espflash 监视器（烧录后自动进入）
cargo espflash --release /dev/cu.usbserial-XXXX --monitor
```

### 3.5 GDB 调试

#### 启用 GDB stub

编辑 `sdkconfig.defaults`：
```
CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y     # panic 时进入 GDB stub
CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT=n
```

重新编译烧录后，panic 时可通过 JTAG/USB 连接 GDB。

#### OpenOCD + GDB（JTAG）

ESP32-S3 内置 USB-JTAG，无需外接调试器：

```bash
# 启动 OpenOCD（需 ESP-IDF 环境）
openocd -f board/esp32s3-builtin.cfg

# 另开终端启动 GDB
xtensa-esp32s3-elf-gdb target/xtensaespidf/release/gateway
(gdb) target remote :3333
(gdb) monitor reset halt
(gdb) load
(gdb) continue
```

#### 关键 GDB 命令

```gdb
bt              # backtrace
info threads    # 列出所有 FreeRTOS 任务
thread apply all bt   # 所有任务 backtrace
x/16wx 0x3F800000    # 查看 PSRAM
monitor reset run    # 复位并运行
```

---

## 4. 项目架构

### 4.1 模块依赖图

```
                          ┌─────────────┐
                          │   main.rs   │  主入口 + 主循环 + OTA 确认 + panic hook
                          └──────┬──────┘
                                 │
        ┌────────────┬───────────┼───────────┬────────────┐
        │            │           │           │            │
   ┌────▼────┐ ┌─────▼─────┐ ┌───▼───┐ ┌─────▼─────┐ ┌────▼────┐
   │ config  │ │   error   │ │ bus  │ │  health   │ │ device  │
   │ (常量)  │ │ AppError  │ │(总线)│ │ (WDT+HB) │ │ (NVS)  │
   └────┬────┘ └───────────┘ └───┬───┘ └─────┬─────┘ └────┬────┘
        │                         │           │            │
        │                         │           │            │
   ┌────▼─────────────────────────▼───────────▼────────────▼────┐
   │                        hal/                                  │
   │  pins + gpio + uart + adc + ledc + i2c_bus + mcp23017 + io_ext │
   └────┬────────────────────────────────────────────────────┬────┘
        │                                                    │
   ┌────▼──────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌▼─────────┐
   │ io/       │  │ channel/ │  │ ethernet/│  │ modbus/  │  │ blemesh/ │
   │ di + do_  │  │ ai + ao  │  │ w5500    │  │ rtu+tcp  │  │ +ble_at  │
   └───────────┘  └──────────┘  └──────────┘  └──────────┘  └──────────┘
                                                       │
                                                  ┌────▼────┐
                                                  │  ota/   │
                                                  └─────────┘
```

### 4.2 启动流程（`main.rs::main`）

```
1. init_logger()                    初始化日志 (Info 级别) + 安装 panic hook
2. confirm_new_firmware()           OTA 新固件确认（取消自动回滚）
3. Peripherals::take()               获取 ESP-IDF 外设
4. Hal::init(peripherals)            硬件抽象层初始化 (GPIO/UART/ADC/LEDC/I2C)
5. device::init()                    NVS 加载 ProtoStore + SystemConfig + 启动监听线程
6. 记录复位原因 + 持久化复位计数
7. ethernet::start()                启动 W5500 以太网 (feature_ethernet)
   wifi::start()                    启动 Wi-Fi (feature_wifi，可选)
8. blemesh::start()                  启动 BLE Mesh (feature_ble_mesh)
   ble_at::start()                   启动 BLE GATT AT 命令通道
9. io::start()                       启动 DI/DO 任务 (feature io-di-do)
10. channel::start()                 启动 AI/AO 任务 (feature ai-ao)
11. modbus::start_rtu()/start_tcp()  启动 Modbus RTU/TCP
12. main_loop()                      进入主循环 (100ms 周期)
    ├─ feed_wdt()                    喂狗 (每周期)
    ├─ 更新 uptime + reset_request   (每 1s)
    ├─ health::check_all()           任务心跳健康检查 (每 1s)
    └─ 日志状态上报
```

### 4.3 任务模型

所有任务通过 `std::thread::spawn` 创建，通过 `Arc<Hal>` 共享硬件句柄，通过 `bus::BUS`（全局 `parking_lot::Mutex`）共享数据。

| 任务名 | 周期 | 绑核 | 说明 |
|--------|------|------|------|
| `main_loop` | 100ms | Core 0 | 喂狗 + 健康检查 + uptime |
| `di-scan` | 1ms (默认) / 5ms (F3/F4) | Core 1 | DI 采样 + 去抖 |
| `do-output` | 10ms | Core 1 | DO 输出（仅变化时写硬件） |
| `ai-sample` | 100ms | Core 1 | ADC 采样 + 标定 |
| `ao-output` | 100ms | Core 1 | LEDC PWM 输出 |
| `device-store` | 50ms 轮询 | Core 0 | NVS commit/reload/apply 监听 |
| `ble-at` | 10ms 轮询 | Core 0 | AT 命令解析 + GATT notify |
| `modbus-rtu-master` | 200ms 轮询 | Core 0 | RTU 主站轮询从站 |
| `modbus-rtu-slave` | 阻塞 | Core 0 | RTU 从站响应 |
| `modbus-tcp-server` | 阻塞 | Core 0 | TCP 502 端口监听（4 连接） |

**核分配策略**（`health::pin_current_to_core`）：
- Core 0（`CORE_NET`）：网络/协议栈（eth/mb-tcp/mb-rtu/ble-at/device-store/main_loop），与 LwIP/Bluedroid/FreeRTOS 系统任务同核，减少 IPC。
- Core 1（`CORE_RT`）：实时采集（di/do/ai/ao），避开网络抖动，保证 DI 1ms 周期稳定。

### 4.4 数据总线（`bus.rs`）

`bus::BUS` 是全局单例（`once_cell::Lazy + parking_lot::Mutex`），所有模块通过它读写共享状态：

```rust
pub struct Bus {
    pub di: DiState,      // DI 输入 (u64 bits)
    pub do_: DoState,     // DO 输出 (u64 bits)
    pub ai: AiState,      // AI (raw + scaled)
    pub ao: AoState,      // AO (scaled + duty)
    pub sys: SysState,    // 系统状态 (uptime/reset_count/log_level...)
    pub proto: ProtoStore,// 协议存储区 (1500 U16)
    pub cfg: SystemConfig,// 系统配置 (SN/IP/RS485/BLE...)
}
```

**访问方式**：
```rust
// 便利函数：超时 100ms 获取锁
if let Some(mut b) = bus::lock_timeout() {
    b.do_.bits |= 1 << 3;  // 置 DO3
}

// 便利宏：自动处理锁超时（continue 跳过本次循环）
with_bus!(b, {
    b.di.bits = new_value;
});
```

设计原则：模块之间不直接耦合，只与总线交互。Modbus 寄存器映射（`bus::read_hold_reg`/`write_hold_reg`）把总线数据暴露给 Modbus 协议；BLE Mesh 模型通过总线读写 DO/AO。

---

## 5. 硬件版本扩展指南

本节以新增 **F5 版本（假设 64 DI + 16 DO，4 片 MCP23017 DI 扩展）** 为例，说明扩展步骤。

### 5.1 涉及文件

| 文件 | 修改点 |
|------|--------|
| `Cargo.toml` | 新增 `f5` feature |
| `build.rs` | 新增 `feature_f5` cfg + 互斥校验 |
| `src/config.rs` | `hw_version` 模块新增 F5 常量 + `io_ext` 新增 DI_ADDRS |
| `src/hal/mod.rs` | F5 走 I2C 扩展分支 |
| `src/hal/io_ext.rs` | `MAX_DI_CHIPS` 调整（如需） |

### 5.2 步骤 1：`Cargo.toml` 新增 feature

```toml
[features]
# ...
f3 = ["io-di-do"]
f4 = ["io-di-do"]
f5 = ["io-di-do"]    # 新增：64 DI + 16 DO
```

### 5.3 步骤 2：`build.rs` 新增 cfg + 互斥校验

```rust
// 1. 新增 cfg 输出
if std::env::var("CARGO_FEATURE_F5").is_ok() {
    println!("cargo:rustc-cfg=feature_f5");
}

// 2. 互斥校验（在 check_feature_compatibility 中）
let has_f3 = std::env::var("CARGO_FEATURE_F3").is_ok();
let has_f4 = std::env::var("CARGO_FEATURE_F4").is_ok();
let has_f5 = std::env::var("CARGO_FEATURE_F5").is_ok();
// F3/F4/F5 三者互斥
let f_count = [has_f3, has_f4, has_f5].iter().filter(|&&x| x).count();
if f_count > 1 {
    panic!("feature error: F3/F4/F5 互斥, 只能启用一个");
}
if has_f5 {
    println!("cargo:warning=info: 编译 F5 版本 (64 DI + 16 DO, I2C MCP23017 扩展)");
}
```

### 5.4 步骤 3：`src/config.rs` 修改 `hw_version`

```rust
pub mod hw_version {
    #[cfg(feature_f3)]
    pub const NAME: &str = "F3";
    #[cfg(feature_f4)]
    pub const NAME: &str = "F4";
    #[cfg(feature_f5)]
    pub const NAME: &str = "F5";                       // 新增
    #[cfg(not(any(feature_f3, feature_f4, feature_f5)))]
    pub const NAME: &str = "Default";

    #[cfg(feature_f5)]
    pub const DI_COUNT: usize = 64;                     // 新增
    // ... 其余 F5 常量同理 ...
    #[cfg(feature_f5)]
    pub const DO_COUNT: usize = 16;
    #[cfg(feature_f5)]
    pub const DI_EXT_CHIPS: usize = 4; // 64 DI / 16 per chip = 4
    #[cfg(feature_f5)]
    pub const DO_EXT_CHIPS: usize = 1;

    #[cfg(any(feature_f3, feature_f4, feature_f5))]
    pub const USE_IO_EXT: bool = true;
}
```

### 5.5 步骤 4：`src/config.rs` 修改 `io_ext`

```rust
pub mod io_ext {
    #[cfg(feature_f5)]
    pub const DI_ADDRS: &[u8] = &[0x20, 0x21, 0x22, 0x23];  // 4 片, 64 DI
    #[cfg(feature_f5)]
    pub const DO_ADDR: u8 = 0x24;                            // 注意避开 DI 地址
}
```

### 5.6 步骤 5：调整 `io_ext.rs` 的 `MAX_DI_CHIPS`

```rust
// src/hal/io_ext.rs
const MAX_DI_CHIPS: usize = 4;  // 原 3，F5 需 4
```

`IoExtender::init` 和 `read_di` 已用 `di_chip_count` 动态循环，无需改动逻辑。

### 5.7 步骤 6：`hal/mod.rs` 已自动适配

`Hal::init` 中 `#[cfg(any(feature_f3, feature_f4))]` 需扩展为 `#[cfg(any(feature_f3, feature_f4, feature_f5))]`：

```rust
#[cfg(any(feature_f3, feature_f4, feature_f5))]
pub mod i2c_bus;
#[cfg(any(feature_f3, feature_f4, feature_f5))]
pub mod mcp23017;
#[cfg(any(feature_f3, feature_f4, feature_f5))]
pub mod io_ext;

// GpioBank 初始化分支
#[cfg(not(any(feature_f3, feature_f4, feature_f5)))]
let gpio = GpioBank::init(pins.di, pins.do_, pins.eth_int, pins.eth_rst, pins.rs485_de)?;
#[cfg(any(feature_f3, feature_f4, feature_f5))]
let gpio = GpioBank::init(pins.eth_int, pins.eth_rst, pins.rs485_de)?;

// I2C 扩展初始化
#[cfg(any(feature_f3, feature_f4, feature_f5))]
let io_ext = { /* 同原 F3/F4 */ };
```

`GpioBank::init`、`HalPins::split`、`io/di.rs`、`io/do_.rs` 中的 `#[cfg(any(feature_f3, feature_f4))]` 也需同步扩展为含 `feature_f5`。

### 5.8 编译验证

```bash
cargo build --features f5
# 预期日志：info: 编译 F5 版本 (64 DI + 16 DO, I2C MCP23017 扩展)
```

---

## 6. 新增 Modbus 寄存器指南

Modbus 寄存器布局集中在 `src/config.rs::regs`，读写映射在 `src/bus.rs::Bus` 的 `read_hold_reg`/`write_hold_reg`。

### 6.1 寄存器区划分

| 区 | 类型 | 范围 | 说明 |
|----|------|------|------|
| Coil | 1-bit RW | `0x0000`~ | DO 输出（数量随硬件版本） |
| Discrete Input | 1-bit RO | `0x0000`~ | DI 输入 |
| Input Register | 16-bit RO | `0x0000`~ | AI 原始/工程量 |
| Holding Register (AO) | 16-bit RW | `0x0000`~0x0003 | AO 输出 |
| Holding Register (Sys) | 16-bit | `0x0100`~0x010F | 系统状态 + OTA |
| Holding Register (Cfg) | 16-bit RW | `0x0200`~0x025F | 系统配置 |
| Holding Register (Proto) | 16-bit RW | `0x4000`~0x45E1 | 协议存储区 |

### 6.2 步骤 1：在 `config.rs::regs` 定义寄存器地址

```rust
pub mod regs {
    // ... 既有寄存器 ...

    // ============ 新增：继电器输出区 ============
    pub const HOLD_RELAY_BASE: u16 = 0x0110;   // 继电器输出 (RW)
    pub const HOLD_RELAY_COUNT: u16 = 4;       // 4 路继电器
    pub const HOLD_RELAY_TEST: u16 = 0x0114;   // (WO) 写 0x1234 触发自检
}
```

### 6.3 步骤 2：在 `bus.rs` 扩展数据结构

```rust
/// 继电器输出
#[derive(Clone, Copy, Default)]
pub struct RelayState {
    pub bits: u16,  // bit i 对应继电器 i
}

// Bus 结构体新增字段
pub struct Bus {
    // ... 既有字段 ...
    pub relay: RelayState,
}
```

### 6.4 步骤 3：在 `bus.rs` 实现 `read_hold_reg` / `write_hold_reg`

```rust
// read_hold_reg 中新增分支
pub fn read_hold_reg(&self, addr: u16) -> Option<u16> {
    // ... 既有分支 ...

    // 继电器区
    if (regs::HOLD_RELAY_BASE..regs::HOLD_RELAY_BASE + regs::HOLD_RELAY_COUNT)
        .contains(&addr)
    {
        let idx = (addr - regs::HOLD_RELAY_BASE) as usize;
        return Some((self.relay.bits >> idx) & 1);
    }
    match addr {
        regs::HOLD_RELAY_TEST => Some(0),  // 只读返回 0
        _ => None,
    }
}

// write_hold_reg 中新增分支
pub fn write_hold_reg(&mut self, addr: u16, value: u16) -> bool {
    // ... 既有分支 ...

    // 继电器区
    if (regs::HOLD_RELAY_BASE..regs::HOLD_RELAY_BASE + regs::HOLD_RELAY_COUNT)
        .contains(&addr)
    {
        let idx = (addr - regs::HOLD_RELAY_BASE) as usize;
        if value != 0 {
            self.relay.bits |= 1 << idx;
        } else {
            self.relay.bits &= !(1 << idx);
        }
        return true;
    }
    match addr {
        regs::HOLD_RELAY_TEST => {
            if value == 0x1234 {
                log::info!("[bus] relay self-test triggered");
                // TODO: 触发自检动作
            }
            true
        }
        _ => false,
    }
}
```

### 6.5 步骤 4：硬件输出层（如需）

在 `io/do_.rs` 或新建 `io/relay.rs` 任务中读取 `bus.relay.bits` 并输出到 GPIO/IO 扩展。

### 6.6 验证

用 Modbus Poll 或 `mbpoll` 测试：
```bash
# 读 4 路继电器状态
mbpoll -m tcp -a 1 -r 0x0111 -c 4 192.168.1.100

# 写继电器 0 为 ON
mbpoll -m tcp -a 1 -r 0x0111 1 192.168.1.100
```

---

## 7. 新增 AT 命令指南

AT 命令通过 BLE GATT 通道收发（Service 0xFF01，RX 0xFF02 Write，TX 0xFF03 Notify）。

### 7.1 文件结构

```
src/ble_at/
├── mod.rs          # GATT 回调 + 处理循环 + feed_data/take_response
├── parser.rs       # 命令分派（cmd → handler）+ 响应构造助手
├── handlers.rs     # 协议区命令（READ/WRITE/BULKR/BULKW/COMMIT/RELOAD/INFO/STATUS/RESET/VERSION）
├── cfg_handlers.rs # 系统配置命令（CFGSN/CFGIP/CFG485/CFGAPPLY...）
└── ota_handlers.rs # OTA 命令（AT+OTA=BEGIN/WRITE/END/ABORT/STATUS/REBOOT）
```

### 7.2 命令格式

- 输入：`AT+<CMD>=<args>\r\n` 或 `AT+<CMD>?\r\n`（查询）或 `AT+<CMD>\r\n`（无参）
- 输出：`OK\r\n` / `OK <data>\r\n` / `ERROR <code>: <msg>\r\n`

### 7.3 步骤 1：在 `parser.rs` 新增分派

```rust
// parser.rs::process 中新增分支
} else if cmd.eq_ignore_ascii_case("RELAY") {
    handlers::handle_relay(args)
}
```

### 7.4 步骤 2：在 `handlers.rs` 实现 handler

```rust
// handlers.rs

// AT+RELAY=<idx>,<0|1>   设置继电器
// AT+RELAY=<idx>          读继电器
// AT+RELAY                读所有继电器状态
pub fn handle_relay(args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        // 读所有
        if let Some(b) = crate::bus::lock_timeout() {
            return ok_data(&format!("0x{:04X}", b.relay.bits));
        }
        return err(99, "bus lock timeout");
    }

    let mut iter = args.splitn(2, ',');
    let idx = match parse_u16(iter.next().unwrap_or("")) {
        Some(i) => i,
        None => return err(10, "invalid idx"),
    };
    if idx >= 4 {
        return err(11, "idx out of range");
    }

    match iter.next() {
        None => {
            // 读单个
            if let Some(b) = crate::bus::lock_timeout() {
                let v = (b.relay.bits >> idx) & 1;
                return ok_data(&format!("{}", v));
            }
            err(99, "bus lock timeout")
        }
        Some(val_str) => {
            let val = match parse_u16(val_str.trim()) {
                Some(v) => v,
                None => return err(10, "invalid value"),
            };
            if let Some(mut b) = crate::bus::lock_timeout() {
                if val != 0 {
                    b.relay.bits |= 1 << idx;
                } else {
                    b.relay.bits &= !(1 << idx);
                }
                ok_none()
            } else {
                err(99, "bus lock timeout")
            }
        }
    }
}
```

### 7.5 步骤 3：响应构造助手（已内置）

```rust
// parser.rs 中已提供：
pub fn ok_none() -> String           // "OK\r\n"
pub fn ok_data(data: &str) -> String // "OK <data>\r\n"
pub fn err(code: u32, msg: &str) -> String  // "ERROR <code>: <msg>\r\n"
pub fn parse_u16(s: &str) -> Option<u16>    // "1234" 或 "0x4D2"
pub fn parse_u16_list(s: &str) -> heapless::Vec<u16, 128>  // "1,2,3"
```

### 7.6 编码规范

- **避免堆分配**：用 `splitn` 迭代器而非 `split().collect()`，用 `heapless::Vec`/`heapless::String`。
- **大小写不敏感**：用 `eq_ignore_ascii_case` 而非 `to_uppercase()` 比较。
- **错误码约定**：`10`=参数错误，`11`=越界，`12`=范围错误，`20`=操作失败，`99`=系统错误。
- **handler 不直接访问 NVS**：通过 `device::request_commit()` 等异步请求，避免阻塞 GATT 线程。
- **OTA 命令**：见 `ota_handlers.rs`，hex 解码用 `heapless::Vec<u8, OTA_AT_CHUNK_MAX>`。

### 7.7 验证

用 BLE 调试工具（如 nRF Connect）连接设备，向 RX Characteristic (0xFF02) 写入：
```
AT+RELAY=0,1\r\n      # 打开继电器 0
AT+RELAY\r\n           # 读所有状态
```
TX Characteristic (0xFF03) 会 notify 响应。

---

## 8. NVS 持久化指南

NVS（Non-Volatile Storage）分区位于 `partitions.csv` 中的 `nvs` 分区（0x10000，24KB）。项目管理两类持久化数据：

| 数据 | 模块 | NVS namespace | 策略 |
|------|------|--------------|------|
| 协议存储区 (1500 U16) | `device/mod.rs` | `gateway` | **双 blob A/B 轮换** |
| 系统配置 (SN/IP/RS485/BLE) | `device/system_config.rs` | `gateway` | 单 blob + magic |
| 复位计数 | `device/mod.rs` | `gateway` | 单 U16 |
| BLE Mesh 状态 | `blemesh/` | 独立 `ble_mesh` 分区 | ESP-IDF BLE Mesh 内部管理 |

### 8.1 双 blob A/B 策略（协议存储区）

为避免写入中途掉电导致数据损坏，协议数据采用 A/B 双 blob 轮换：

**blob 布局**（共 3010 字节）：
```
[magic:2][version:2][length:2][crc:4][data:3000]
 └─ PROTO_MAGIC (0x4757)   └─ CRC32(header[0..6] + data, wrapping_add)
```

**NVS keys**：
- `proto_a` / `proto_b`：两个完整 blob
- `proto_act`：当前 active 标志（0=A，1=B）
- `proto_data` / `proto_magic` / `proto_ver` / `proto_len`：legacy 单 blob（兼容旧固件，迁移后删除）

**写入流程**（`device::commit`）：
```
1. 从 bus 拷贝 data + version + length
2. 序列化为 blob，计算 CRC32
3. 读 active 标志 → 决定写入 inactive blob
4. 写入 inactive blob（若掉电，active 仍指向旧 blob，数据不丢）
5. 切换 active 标志（set_u8，ESP-IDF NVS 保证页级原子性）
6. 清理 legacy key
7. 更新 bus.proto.dirty = false, status = 0
```

**读取流程**（`device::load_proto_from_nvs`）：
```
1. 读 active 标志 → 读对应 blob → 校验 magic + CRC
2. 失败则读另一个 blob → 校验
3. 都失败则尝试 legacy 单 blob 格式
4. 都失败则返回空默认值
```

### 8.2 SystemConfig 持久化

`SystemConfig` 是结构化配置（~110 字节），单 blob 存储：

**NVS keys**：
- `sys_cfg`：配置 blob（固定 128 字节布局）
- `sys_cfg_mag`：magic（`0x4757_4346` = "GWCF"）

**blob 布局**：
```
SN(32) + name(16) + hw_ver(2) + fw_ver(2) + cfg_ver(2)
+ eth_mac(6) + dhcp(1) + ip(4) + mask(4) + gw(4) + dns(4)
+ ble_mac(6) + ble_name(8) + ble_mesh(1)
+ rs485[0](9) + rs485[1](9)   = 110 字节，取 128 留余量
```

### 8.3 异步监听线程（`device::watch_loop`）

为避免阻塞 Modbus/AT 主线程，NVS 写入由独立线程异步执行：

```rust
fn watch_loop() {
    loop {
        WATCH_HB.tick();  // 心跳
        if COMMIT_REQUEST.swap(false, SeqCst) { commit() }
        if RELOAD_REQUEST.swap(false, SeqCst) { reload() }
        if APPLY_CONFIG_REQUEST.swap(false, SeqCst) { apply_config() }
        sleep(50ms);
    }
}
```

**请求 API**（供 Modbus/AT 调用）：
- `device::request_commit()` — 异步请求持久化协议数据
- `device::request_reload()` — 异步请求从 NVS 重载
- `device::request_apply_config()` — 异步请求应用系统配置
- `device::commit_sync()` / `reload_sync()` / `apply_config_sync()` — 同步版本（供 AT 直接调用）

### 8.4 触发方式

| 触发方式 | 协议数据 | 系统配置 |
|----------|----------|----------|
| Modbus 寄存器 | 写 `PROTO_COMMIT=0xC5C5` (0x45DC) | 写 `CFG_APPLY=0xB5B5` (0x021B) |
| Modbus 寄存器 | 写 `PROTO_RELOAD=0xA5A5` (0x45DD) | 写 `CFG_RESET=0xD5D5` (0x021C) |
| AT 命令 | `AT+COMMIT` / `AT+RELOAD` | `AT+CFGAPPLY` / `AT+CFGRESET` |

### 8.5 apply_config 的软重启设计

网络配置变更（IP/RS485/BLE）需重新初始化外设，运行时切换风险高（资源句柄所有权问题），故 `apply_config` 采用软重启：

```rust
fn apply_config() -> AppResult<()> {
    // 1. 持久化 SystemConfig 到 NVS
    cfg.save_to_nvs(&mut nvs)?;
    // 2. 500ms 后 esp_restart（给 AT 响应 + 日志 flush + NVS 提交留时间）
    std::thread::spawn(|| {
        sleep(500ms);
        esp_restart();
    });
}
```

### 8.6 CRC32 实现

项目自带 CRC32（IEEE 802.3，多项式 `0xEDB88320`），无外部依赖：

```rust
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 { crc = (crc >> 1) ^ 0xEDB8_8320; }
            else { crc >>= 1; }
        }
    }
    !crc
}
```

---

## 9. OTA 升级开发

### 9.1 分区表（`partitions.csv`）

```
factory,   app,  factory,  0x20000,  0x300000,   # 3MB 当前固件
ota_0,     app,  ota_0,    0x320000, 0x240000,   # 2.25MB 升级槽 0
ota_1,     app,  ota_1,    0x560000, 0x240000,   # 2.25MB 升级槽 1
otadata,   data, ota,      0x7A0000, 0x2000,     # 记录当前启动分区
```

`esp_ota_get_next_update_partition` 自动在 ota_0/ota_1 间轮换。

### 9.2 状态机

```
Idle ─BEGIN→ Receiving ─END→ DonePendingReboot ─REBOOT→ Idle (新固件)
                 │
                 └─ABORT→ Idle (回滚原分区)
```

| 状态 | 值 | 说明 |
|------|----|------|
| `Idle` | 0 | 空闲 |
| `Receiving` | 1 | 接收中 |
| `DonePendingReboot` | 2 | 完成待重启 |
| `VerifyFailed` | 3 | 校验失败（esp_ota_end 失败） |
| `NoSpace` | 4 | 空间不足 |
| `Aborted` | 5 | 已中止 |

### 9.3 触发方式

#### BLE AT 命令（推荐，传输数据）

```
AT+OTA=BEGIN,<total_size>     # 开始升级
AT+OTA=WRITE,<hex_chunk>      # 写入 hex 编码数据块（单帧 ≤ 512 字节 binary）
AT+OTA=END                    # 结束 + 设置启动分区
AT+OTA=REBOOT                 # 重启应用新固件
AT+OTA=STATUS                 # 查询状态：OK status=N,written=XX,total=YY
AT+OTA=ABORT                  # 中止
```

#### Modbus 寄存器（控制 + 状态查询）

| 寄存器 | 地址 | 权限 | 说明 |
|--------|------|------|------|
| `HOLD_OTA_STATUS` | 0x0107 | RO | 状态码 |
| `HOLD_OTA_TOTAL_LO` | 0x0108 | RW | 总大小低 16 位 |
| `HOLD_OTA_TOTAL_HI` | 0x0109 | RW | 总大小高 16 位 |
| `HOLD_OTA_WRITTEN_LO` | 0x010A | RO | 已写入低 16 位 |
| `HOLD_OTA_WRITTEN_HI` | 0x010B | RO | 已写入高 16 位 |
| `HOLD_OTA_BEGIN` | 0x010C | WO | 写 `0x0B0A` → 开始 |
| `HOLD_OTA_END` | 0x010D | WO | 写 `0x0E0D` → 结束 |
| `HOLD_OTA_ABORT` | 0x010E | WO | 写 `0x0AB0` → 中止 |
| `HOLD_OTA_REBOOT` | 0x010F | WO | 写 `0x0F0E` → 重启 |

### 9.4 OTA API（`src/ota/mod.rs`）

```rust
pub fn set_pending_total(size: u32)         // 设置总大小（供 Modbus 写 TOTAL_LO/HI）
pub fn begin(total_size: u32) -> AppResult  // esp_ota_begin
pub fn write_chunk(data: &[u8]) -> AppResult<usize>  // esp_ota_write（单次 ≤ 4KB）
pub fn end() -> AppResult                   // esp_ota_end + set_boot_partition
pub fn abort() -> AppResult                 // esp_ota_abort
pub fn status() -> OtaStatus                // 查询状态
pub fn written_bytes() -> u32               // 已写入字节
```

### 9.5 固件确认机制（`main.rs::confirm_new_firmware`）

ESP-IDF 启用 `CONFIG_APP_ROLLBACK_ENABLE` 后，OTA 升级后的新固件首次启动状态为 `ESP_OTA_IMG_PENDING_VERIFY`。若 app 不主动确认，下次重启会自动回滚到旧固件。

```rust
fn confirm_new_firmware() {
    let partition = unsafe { esp_ota_get_running_partition() };
    let mut state = 0;
    unsafe { esp_ota_get_state_partition(partition, &mut state) };
    if state == ESP_OTA_IMG_PENDING_VERIFY {
        unsafe { esp_ota_mark_app_valid_cancel_rollback() };
        log::info!("[main] OTA: new firmware confirmed valid");
    }
}
```

**确认时机**：当前在 `main()` 入口即确认（表示"启动到此处即认为新固件可运行"）。如需更严格确认（如启动所有任务 + 网络连通后才确认），可改为延后到主循环中调用。

### 9.6 关键 sdkconfig 配置

```
CONFIG_APP_ROLLBACK_ENABLE=y                    # 允许 OTA 回滚
CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y        # bootloader 支持回滚
CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT=y          # panic 时打印 + 重启
```

### 9.7 升级示例脚本

```bash
# 假设固件镜像为 firmware.bin，通过 BLE 工具发送 AT 命令
# 1. 查询状态
AT+OTA=STATUS
# OK status=0,written=0,total=0

# 2. 开始（需先知道固件大小）
AT+OTA=BEGIN,150000

# 3. 分块写入（hex 编码，每块 ≤ 512 字节）
AT+OTA=WRITE,0xE9,0x00,0x00,0x00,...   # 实际为 hex 字符串

# 4. 结束
AT+OTA=END
# OK ended, send AT+OTA=REBOOT to apply

# 5. 重启
AT+OTA=REBOOT
```

---

## 10. 调试技巧

### 10.1 日志级别调节

#### 编译期默认

`main.rs::init_logger` 设置默认 Info：
```rust
log::set_max_level(log::LevelFilter::Info);
```

#### 运行时调节（无需重新编译）

**方式 1：Modbus 寄存器** — 写 `HOLD_SYS_LOG_LEVEL` (0x0106)：
```
值 0 = Error
值 1 = Warn
值 2 = Info（默认）
值 3 = Debug
值 4 = Trace
```

`bus.rs::write_hold_reg` 中立即应用：
```rust
regs::HOLD_SYS_LOG_LEVEL => {
    if value <= 4 {
        self.sys.log_level = value as u8;
        let level = match value {
            0 => log::LevelFilter::Error,
            // ...
            4 => log::LevelFilter::Trace,
            _ => log::LevelFilter::Info,
        };
        log::set_max_level(level);
    }
}
```

**方式 2：sdkconfig 调整最大级别**
```
CONFIG_LOG_MAXIMUM_LEVEL_VERBOSE=y   # 允许 Trace 级别输出
```

### 10.2 Panic Backtrace

#### Rust panic hook（`main.rs::install_panic_hook`）

已安装 panic hook，打印 panic 位置 + Rust backtrace：

```rust
std::panic::set_hook(Box::new(move |info| {
    log::error!("========== RUST PANIC ==========");
    log::error!("panic: {}", info);
    if let Some(loc) = info.location() {
        log::error!("  at {}:{}:{}", loc.file(), loc.line(), loc.column());
    }
    let bt = std::backtrace::Backtrace::force_capture();
    log::error!("backtrace:\n{}", bt);
    default_hook(info);  // 触发 ESP-IDF panic handler → 复位
}));
```

#### ESP-IDF panic handler

`sdkconfig.defaults` 配置：
```
CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT=y   # 打印寄存器 + backtrace + 重启
CONFIG_ESP_SYSTEM_USE_EH_FRAME=y         # 启用 backtrace decoder
```

ESP-IDF 自身会打印 CPU 寄存器 + Xtensa backtrace（地址），Rust hook 补充 Rust 层 backtrace（含符号）。

#### 解析 backtrace 地址

panic 日志中的地址需用 `addr2line` 解析：
```bash
xtensa-esp32s3-elf-addr2line -e target/xtensaespidf/release/gateway 0x42012345
# 或用 xtensa-esp32s3-elf-nm + 反汇编
xtensa-esp32s3-elf-objdump -d target/xtensaespidf/release/gateway | grep -A 20 42012345
```

#### profile 选择

- **调试 panic**：用 `dev` profile（`panic=unwind`，backtrace 完整）
- **量产**：用 `release` profile（`panic=abort`，体积小，hook 仍会被调用）

### 10.3 任务健康检查

#### 任务看门狗（ESP-IDF Task Watchdog）

`sdkconfig`：`CONFIG_ESP_TASK_WDT_INIT=y`，`TIMEOUT_S=10`。

```rust
// 在任务函数开头订阅看门狗
health::subscribe_wdt();
loop {
    health::feed_wdt();   // 每 100ms 喂狗
    sleep(100ms);
}
// 任务退出前
health::unsubscribe_wdt();
```

超时未喂狗 → 看门狗触发系统复位。`main_loop` 已订阅并每 100ms 喂狗。

#### 任务心跳（软件心跳，`health.rs`）

每个任务用 `static TASK_HB: TaskHb = TaskHb::new("name")` 注册，loop 中调用 `TASK_HB.tick()` 递增：

```rust
static TASK_HB: TaskHb = TaskHb::new("di-scan");

pub fn start_scan_task(hal: Arc<Hal>) -> AppResult<()> {
    health::register(&TASK_HB);
    std::thread::spawn(move || {
        loop {
            TASK_HB.tick();  // 每 N 次循环调一次（HB_DIV 节流）
            // ... 任务工作 ...
        }
    });
}
```

`main_loop` 每 1s 调用 `health::check_all()`，返回停滞任务名列表（心跳连续未变化次数超过 `max_stall`）：

```rust
let stalled = health::check_all();
if !stalled.is_empty() {
    log::warn!("[main] stalled tasks: {:?}", stalled.as_slice());
}
```

**调整停滞阈值**（用于阻塞等待型任务，如 TCP 监听）：
```rust
static TASK_HB: TaskHb = TaskHb::new_with_stall("tcp-server", 30);  // 允许 30s 无活动
```

#### 复位原因持久化

`main.rs` 启动时记录 `esp_reset_reason()` 并持久化到 NVS：

```rust
let reset_reason = unsafe { esp_idf_sys::esp_reset_reason() } as u8;
let mut reset_count = device::load_reset_count();
reset_count = reset_count.wrapping_add(1);
device::save_reset_count(reset_count)?;
```

复位原因值（`esp_reset_reason_t`）：
```
1=POWERON  2=EXT  3=SW  4=PANIC  5=INT_WDT  6=TASK_WDT  7=WDT  15=BROWNOUT  16=DEEPSLEEP
```

通过 Modbus 读取：
- `HOLD_SYS_RESET_CNT` (0x0102)：复位计数
- `HOLD_SYS_RESET_REASON` (0x0104)：上次复位原因

### 10.4 内存分析

#### PSRAM 分配策略

`sdkconfig.defaults`：
```
CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=4096       # <4KB 走内部 SRAM
CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=16384    # 保留 16KB 内部 SRAM 给 DMA/中断栈
```

#### 运行时查看堆

```rust
log::info!("free heap: {} bytes", unsafe { esp_idf_sys::esp_get_free_heap_size() });
log::info!("free internal: {} bytes", unsafe { esp_idf_sys::esp_get_free_internal_heap_size() });
log::info!("min free: {} bytes", unsafe { esp_idf_sys::esp_get_minimum_free_heap_size() });
```

### 10.5 常见问题排查

| 现象 | 可能原因 | 排查 |
|------|----------|------|
| 启动后立即重启循环 | OTA 新固件 panic | 看 panic backtrace，可能固件损坏 |
| 任务停滞日志反复 | 某任务死锁或阻塞 | 看 `stalled tasks` 名，加 Debug 日志 |
| Modbus 无响应 | RS485 DE 引脚方向错误 / 波特率不匹配 | 用示波器看 DE 信号，检查 `config::modbus` |
| BLE 连接不上 | Bluedroid 未初始化 / COEX 冲突 | 检查 `blemesh::start` 日志，`CONFIG_ESP_COEX_SW_COEXIST_ENABLE` |
| NVS 写入失败 | NVS 分区满 / 加密配置冲突 | `CONFIG_NVS_ENCRYPTION`，擦除 nvs 分区 |
| OTA 写入失败 | 分区大小不足 / esp_ota_begin 错误码 | 看 `0x{err:08X}`，查 `esp_err_to_name` |

### 10.6 调试用日志规范

```rust
log::error!("...");  // 系统级错误，必须关注
log::warn!("...");   // 可恢复异常，潜在问题
log::info!("...");   // 启动/状态变更，默认级别
log::debug!("...");  // 调试细节，DI 边沿/Modbus 帧
log::trace!("...");  // 极详细，每个 ADC 采样点
```

- 模块前缀：`[di]`、`[do]`、`[bus]`、`[main]`、`[device]`、`[ota]` 等，便于 grep 过滤。
- 高频任务（DI 1ms）的日志用 `HB_DIV` 节流，避免刷屏。
- 启动关键步骤用 `info!`，故障用 `error!` + 完整错误链。

---

## 附录：常用命令速查

```bash
# === 编译 ===
cargo build                                    # Debug, Default 硬件
cargo build --release --features f4            # Release, F4 硬件
cargo build --release --no-default-features --features "f4,ble-mesh,ethernet-w5500,modbus-rtu,modbus-tcp,ai-ao"

# === 烧录 ===
cargo espflash --release /dev/cu.usbserial-XXXX --monitor
cargo run --release                            # 用 .cargo/config.toml 的 runner

# === 监视 ===
screen /dev/cu.usbserial-XXXX 115200           # Ctrl+A K Y 退出

# === 清理重建 ===
rm -rf build sdkconfig && cargo build --release

# === 烧录分区表（修改 partitions.csv 后） ===
esptool.py --chip esp32s3 --port /dev/cu.usbserial-XXXX write_flash 0x8000 partitions.csv

# === GDB ===
openocd -f board/esp32s3-builtin.cfg
xtensa-esp32s3-elf-gdb target/xtensaespidf/release/gateway
(gdb) target remote :3333

# === 地址解析 ===
xtensa-esp32s3-elf-addr2line -e target/xtensaespidf/release/gateway 0x42012345
```
