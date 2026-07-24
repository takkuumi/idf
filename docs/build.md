# 构建与烧录

## 硬件平台

- **主控**：ESP32-S3R8 (Xtensa LX7 双核 240MHz, 512KB SRAM, **8MB Octal SPI PSRAM**)
- **以太网**：WIZnet W5500 (SPI 接口)
- **Flash**：8MB (分区表见 `partitions.csv`)

## 环境准备

### 1. ESP-IDF v5.5.4 + Xtensa 工具链

```bash
# 克隆 ESP-IDF (一次性, 含子模块)
git clone -b v5.5.4 --recursive https://github.com/espressif/esp-idf.git
cd esp-idf
./install.sh esp32s3     # 注意: esp32s3 (不是 esp32c5)

# 每次打开新终端, 激活环境变量
. ./export.sh
```

ESP-IDF 安装后, Xtensa GCC 工具链位于：
```
~/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20260121/xtensa-esp-elf/bin/
```

主要工具：
- `xtensa-esp32s3-elf-gcc` (链接器, 由 ldproxy 调用)
- `xtensa-esp-elf-gcc-ar` (归档器)
- `xtensa-esp-elf-objcopy` / `xtensa-esp-elf-size` 等

### 2. Rust 工具链 (Xtensa, 项目级 nightly)

```bash
# 安装 Rust (如已安装可跳过)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 espup (管理 Xtensa Rust 工具链)
cargo install espup
espup install --targets esp32s3 --toolchain-version 1.90.0.0
# 生成 ~/export-esp.sh, 每次新终端执行: . $HOME/export-esp.sh
```

**⚠️ Intel Mac (x86_64) 注意**：Xtensa Rust v1.91.1.0+ 仅发布 `aarch64-apple-darwin`（Apple Silicon）构建，不再提供 `x86_64-apple-darwin`。Intel Mac 必须使用 `--toolchain-version 1.90.0.0`（最后一个支持 Intel Mac 的版本）。

**espup install 失败排查**：

1. **`File exists (os error 17)`**：之前失败的安装残留了文件。清理后重试：
   ```bash
   rustup toolchain uninstall esp
   rm -rf ~/.rustup/toolchains/esp ~/.espup
   # 删除 stable 工具链中残留的 RISC-V target（espup 手动安装，rustup 不追踪）
   rm -rf ~/.rustup/toolchains/stable-x86_64-apple-darwin/lib/rustlib/riscv32imc-unknown-none-elf
   rm -rf ~/.rustup/toolchains/stable-x86_64-apple-darwin/lib/rustlib/riscv32imac-unknown-none-elf
   rm -rf ~/.rustup/toolchains/stable-x86_64-apple-darwin/lib/rustlib/riscv32imafc-unknown-none-elf
   rm -f ~/.rustup/toolchains/stable-x86_64-apple-darwin/lib/rustlib/manifest-rust-std-riscv32im*
   espup install --targets esp32s3 --toolchain-version 1.90.0.0
   ```

2. **`Operation not permitted (os error 1)` on `.espup/esp-clang`**：espup 内部 `symlink()` 系统调用被沙箱拦截。手动创建符号链接指向 Homebrew LLVM 即可：
   ```bash
   brew install llvm  # 如已安装可跳过
   mkdir -p ~/.espup
   ln -sfn /usr/local/opt/llvm/lib ~/.espup/esp-clang   # Intel Mac 路径
   # Apple Silicon: ln -sfn /opt/homebrew/opt/llvm/lib ~/.espup/esp-clang
   ```
   然后手动创建 `~/export-esp.sh`：
   ```bash
   echo 'export LIBCLANG_PATH="$HOME/.espup/esp-clang"' > ~/export-esp.sh
   ```

**项目级 nightly 配置**：`rust-toolchain.toml` 仅对本项目生效，不影响其他 Rust 项目。

### 3. 链接器与烧录工具（关键）

```bash
# ldproxy: embuild 提供的链接器代理
# esp-idf-sys 0.37 强制要求 linker = "ldproxy", 不能直接用 xtensa-esp32s3-elf-gcc
cargo install ldproxy

# cargo-espflash / espflash: 烧录工具
cargo install cargo-espflash
cargo install espflash
```

**⚠️ 重要**：`ldproxy` 必须在 PATH 中（默认安装到 `~/.cargo/bin/`）。如果没有安装 ldproxy 而直接把 `linker` 设为 gcc，会导致 `--ldproxy-linker` / `--ldproxy-cwd` 等私有参数被传给 gcc，报 `unrecognized command-line option` 错误。

### 4. 系统依赖

**macOS**:
```bash
brew install ninja cmake libusb
```

**Ubuntu**:
```bash
sudo apt-get install -y gcc g++ ninja-build cmake libssl-dev pkg-config libusb-1.0-0-dev
```

## 项目配置文件

### `.cargo/config.toml`（构建核心配置）

```toml
[build]
target = "xtensa-esp32s3-espidf"

[target.xtensa-esp32s3-espidf]
linker = "ldproxy"
runner = "espflash flash --monitor"
rustflags = ["--cfg", "espidf_time64"]

[unstable]
build-std = ["std", "core", "alloc", "panic_abort"]
build-std-features = ["panic_immediate_abort"]

[env]
ESP_IDF_VERSION = "v5.5.4"
ESP_IDF_SDKCONFIG_DEFAULTS = { value = "sdkconfig.defaults", relative = true }
MCU = "esp32s3"
ESP_IDF_TARGET = "esp32s3"
```

**关键说明**：
- `linker = "ldproxy"`：必须用 ldproxy，不能直接用 gcc（esp-idf-sys 0.37 强制要求）
- `rustflags = ["--cfg", "espidf_time64"]`：ESP-IDF 5.x time_t 为 64 位
- `build-std`：从源码编译 std/core/alloc（因为 xtensa-esp32s3-espidf 不是官方 target）
- **不要**手动加 `-C link-arg=--gc-sections` / `--build-id=none` 等链接器原生选项（ldproxy 会自动处理）

### `rust-toolchain.toml`

```toml
[toolchain]
channel = "esp"
targets = ["xtensa-esp32s3-espidf"]
```

`channel = "esp"` 是 espup 安装的 Xtensa Rust 工具链别名。

### `Cargo.toml`（依赖版本）

```toml
[dependencies]
esp-idf-sys = { version = "0.37", features = ["binstart"] }
esp-idf-hal = { version = "0.46", default-features = false, features = ["std"] }
esp-idf-svc = { version = "0.52", features = ["alloc"] }
embuild = { version = "0.33", features = ["espidf"] }  # build-dependencies
```

### `sdkconfig.defaults`（ESP-IDF 配置）

关键配置项（详见文件本身）：
- Flash: QIO 80MHz 8MB
- PSRAM: Octal 80MHz 8MB
- BLE: `CONFIG_BT_BLUEDROID_ENABLED=y`（v5.5.4 重命名，旧名 `CONFIG_BT_BLUEDROID` 已废弃）
- BLE Mesh: `CONFIG_BLE_MESH=y`（v5.5.4 重命名，旧名 `CONFIG_BT_BLE_MESH` 已废弃）
- W5500: `CONFIG_ETH_SPI_ETHERNET_W5500=y`
- Core Dump: `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH=y`
- Heap: `CONFIG_HEAP_POISONING_COMPREHENSIVE=y`

### `partitions.csv`（8MB Flash 分区表）

| 分区 | 类型 | 偏移 | 大小 | 用途 |
|------|------|------|------|------|
| nvs | data nvs | 0x9000 | 24 KB | 系统 NVS |
| nvs_keys | data nvs_keys | 0xF000 | 4 KB | NVS 加密密钥 |
| phy_init | data phy | 0x10000 | 4 KB | PHY 校准数据 |
| factory | app factory | 0x10000 | 2.25 MB | 出厂固件 |
| ota_0 | app ota_0 | 0x260000 | 2.25 MB | OTA 升级槽 0 |
| ota_1 | app ota_1 | 0x4A0000 | 2.25 MB | OTA 升级槽 1 |
| otadata | data ota | 0x6E0000 | 8 KB | OTA 选择分区 |
| ble_mesh | data nvs | 0x6E2000 | 32 KB | BLE Mesh 独立 NVS |
| coredump | data coredump | 0x6EA000 | 64 KB | 崩溃转储 |
| storage | data fat | 0x6FA000 | 1 MB | FAT 文件系统 |

## 构建环境变量

### 必须设置的环境变量（每次新终端）

```bash
# 1. Xtensa GCC 工具链 (ESP-IDF 安装后)
export PATH="$HOME/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20260121/xtensa-esp-elf/bin:$PATH"

# 2. ESP-IDF 环境 (可选, 仅当需要 idf.py 时)
. /path/to/esp-idf/export.sh

# 3. Xtensa Rust 工具链 (espup 安装后)
. $HOME/export-esp.sh
```

### 检查环境

```bash
which ldproxy                    # 应输出 ~/.cargo/bin/ldproxy
which xtensa-esp32s3-elf-gcc    # 应输出 ~/.espressif/.../bin/xtensa-esp32s3-elf-gcc
rustc --version                  # 应显示 esp 工具链版本
```

## 编译

### Debug 构建

```bash
cd /Users/takumi/Workspace/idf
. $HOME/export-esp.sh
export PATH="$HOME/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20260121/xtensa-esp-elf/bin:$PATH"
cargo build
```

产物路径：`target/xtensa-esp32s3-espidf/debug/gateway`（~23 MB，含调试符号）

### Release 构建（生产级）

```bash
cd /Users/takumi/Workspace/idf
. $HOME/export-esp.sh
export PATH="$HOME/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20260121/xtensa-esp-elf/bin:$PATH"
cargo build --release
```

产物路径：`target/xtensa-esp32s3-espidf/release/gateway`（~1.6 MB，已 strip）

Release profile 配置（`Cargo.toml`）：
```toml
[profile.release]
opt-level = 3          # 性能优先 (Modbus CRC/ADC 滤波延迟)
lto = "fat"            # 跨 crate 全链接时优化
codegen-units = 1      # 单一 codegen unit, 最优优化
strip = true           # 移除调试符号
panic = "abort"        # panic 直接 abort (省 unwind 表)
```

### Feature 编译

```bash
# 默认全部启用
cargo build --release

# 仅启用部分功能
cargo build --release --no-default-features \
    --features "ble-mesh,ethernet-w5500,modbus-rtu,modbus-tcp,ai-ao,io-di-do"

# F3 硬件版本 (16 DI + 16 DO, I2C MCP23017 扩展)
cargo build --release --features f3

# F4 硬件版本 (48 DI + 16 DO, I2C MCP23017 扩展)
cargo build --release --features f4
```

可用 feature（`Cargo.toml`）：
- `adc-continuous`（默认）- ADC 连续采样模式
- `ble-mesh`（默认）- BLE Mesh 协议栈
- `ethernet-w5500`（默认）- W5500 以太网
- `modbus-rtu`（默认）- Modbus RTU 主从
- `modbus-tcp`（默认）- Modbus TCP Server
- `ai-ao`（默认）- AI 采样 + AO PWM 输出
- `io-di-do`（默认）- DI/DO 数字 IO
- `wifi` - Wi-Fi（与 BLE 共存，默认关）
- `f3` / `f4` - 硬件版本选择

## 构建产物

每次构建生成 3 个核心产物（对应 ESP32 启动链的 3 个阶段）：

| 产物 | 大小 (Release) | 烧录位置 | 作用 |
|------|---------------|----------|------|
| `bootloader.bin` | ~22 KB | `0x0` | 引导加载器（芯片复位后最先运行） |
| `partition-table.bin` | ~3 KB | `0x8000` | 分区表（描述 Flash 布局） |
| `gateway` | ~1.6 MB | factory/ota 分区 | 应用程序（ELF 格式，可直接烧录） |

**启动流程**：
```
上电 → bootloader.bin (0x0)
         ↓ 读取 partition-table.bin (0x8000)
         ↓ 找到 factory/ota 分区
         ↓ 跳转执行 gateway
       应用程序运行
```

**首次编译**会编译 ESP-IDF C 库（~1381 个文件）+ Rust 依赖，耗时 15-40 分钟。后续增量编译仅需 10-30 秒。

## 烧录

### 方式 1: espflash（推荐）

```bash
# 列出串口
ls /dev/cu.usbserial-* /dev/ttyUSB* 2>/dev/null

# 烧录 + 监控（自动烧录 bootloader/partition-table/app 三个产物）
espflash flash --monitor target/xtensa-esp32s3-espidf/release/gateway

# 指定串口
espflash flash --monitor --port /dev/cu.usbserial-XXXX target/xtensa-esp32s3-espidf/release/gateway
```

### 方式 2: cargo run（用 .cargo/config.toml 配置的 runner）

```bash
cargo run --release
```

### 方式 3: esptool.py（备用）

```bash
esptool.py --chip esp32s3 --port /dev/cu.usbserial-XXXX --baud 921600 \
    write_flash 0x0   target/xtensa-esp32s3-espidf/release/bootloader.bin \
                    0x8000 target/xtensa-esp32s3-espidf/release/partition-table.bin \
                    0x10000 target/xtensa-esp32s3-espidf/release/gateway
```

## 监控

烧录后用串口工具连接 (UART0: GPIO43=TX, GPIO44=RX, 115200 8N1)：

```bash
# espflash monitor (推荐, 支持地址解析)
espflash monitor target/xtensa-esp32s3-espidf/release/gateway

# 或用 screen
screen /dev/cu.usbserial-XXXX 115200
# 退出 screen: Ctrl+A 然后 K 然后 Y
```

## 常见问题

### 1. `unrecognized command-line option '--ldproxy-linker'`

**原因**：`.cargo/config.toml` 把 `linker` 直接设为 `xtensa-esp32s3-elf-gcc`，导致 ldproxy 私有参数被传给 gcc。

**解决**：
```bash
# 安装 ldproxy
cargo install ldproxy

# 修改 .cargo/config.toml
[target.xtensa-esp32s3-espidf]
linker = "ldproxy"    # 不要用 xtensa-esp32s3-elf-gcc
rustflags = ["--cfg", "espidf_time64"]  # 不要加 -C link-arg=--gc-sections 等
```

### 2. `undefined reference to esp_ble_mesh_*`

**原因**：ESP-IDF v5.5.4 重命名了 BLE Mesh 配置项，旧名 `CONFIG_BT_BLE_MESH_*` 已废弃。

**解决**：修改 `sdkconfig.defaults`，把所有 `CONFIG_BT_BLE_MESH_*` 改为 `CONFIG_BLE_MESH_*`：
```diff
- CONFIG_BT_BLUEDROID=y
+ CONFIG_BT_BLUEDROID_ENABLED=y
- CONFIG_BT_BLE_MESH=y
+ CONFIG_BLE_MESH=y
- CONFIG_BT_BLE_MESH_PROVISIONER=y
+ CONFIG_BLE_MESH_PROVISIONER=y
- CONFIG_BT_BLE_MESH_PROXY=y
+ CONFIG_BLE_MESH_PROXY=y
```

然后删除已生成的 sdkconfig 强制重新配置：
```bash
rm -f target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-*/out/sdkconfig
cargo build
```

### 3. `undefined reference to esp_ble_gatts_app_create`

**原因**：ESP-IDF API 函数名写错。正确名是 `esp_ble_gatts_app_register`（不是 create）。

**解决**：检查 `src/ble_at/mod.rs` 中的 extern 声明和调用，把 `esp_ble_gatts_app_create` 改为 `esp_ble_gatts_app_register`。

### 4. `call to unsafe function esp_restart is unsafe`

**原因**：Rust Edition 2024 + `#![warn(unsafe_op_in_unsafe_fn)]` 要求 extern "C" 函数调用必须显式 `unsafe {}` 块。

**解决**：把 `esp_idf_sys::esp_restart()` 包裹 `unsafe {}`：
```rust
unsafe { esp_idf_sys::esp_restart() };
```

### 5. `error[E0107]: missing lifetime specifier` for `EspNvs` / `EspNvsPartition` / `EspTimerService`

**原因**：esp-idf-svc 0.52 引入泛型参数，需要类型别名。

**解决**：
```rust
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
// EspDefaultNvs = EspNvs<NvsDefault>
// EspDefaultNvsPartition = EspNvsPartition<NvsDefault>

use esp_idf_svc::timer::EspTaskTimerService;
// EspTaskTimerService = EspTimerService<Task>
```

### 6. `error: failed to run custom build command for esp-idf-sys`

**原因**：ESP-IDF 下载/构建失败，常见于网络问题。

**解决**：
- 检查网络连接
- 设置代理：`export HTTPS_PROXY=http://your-proxy:port`
- 手动指定 ESP-IDF 路径：`export IDF_PATH=/path/to/esp-idf`

### 7. 修改 sdkconfig.defaults 后未生效

**原因**：ESP-IDF 的 sdkconfig 是缓存文件，修改 defaults 后需删除缓存强制重新生成。

**解决**：
```bash
# 删除所有 esp-idf-sys 构建目录下的 sdkconfig
rm -f target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-*/out/sdkconfig
rm -f target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*/out/sdkconfig

# 重新构建
cargo build --release
```

### 8. `GPIO_NUM_xxx is not a valid GPIO for ESP32-S3`

**原因**：`config.rs` 中某些引脚号超出 ESP32-S3 范围 (0-48)，或使用了 GPIO26~32 (被 Flash/PSRAM 占用)。

**解决**：参考 [pinmap.md](pinmap.md) 中的引脚分配表，按实际硬件修改 `config::pins`。

## 重新构建配置

修改 `sdkconfig.defaults` 后，需删除 build 缓存强制重新生成：

```bash
rm -f target/xtensa-esp32s3-espidf/*/build/esp-idf-sys-*/out/sdkconfig
cargo build --release
```

修改 `partitions.csv` 后，需重新烧录分区表：

```bash
esptool.py --chip esp32s3 --port /dev/cu.usbserial-XXXX \
    write_flash 0x8000 target/xtensa-esp32s3-espidf/release/partition-table.bin
```

## 工具链架构对照

| 项目 | ESP32-C5 (旧) | ESP32-S3R8 (新) |
|------|---------------|-----------------|
| CPU 架构 | RISC-V 32-bit | Xtensa LX7 32-bit 双核 |
| Rust target | `riscv32imc-esp-espidf` | `xtensa-esp32s3-espidf` |
| Linker | `riscv32-esp-elf-gcc` | `ldproxy` → `xtensa-esp32s3-elf-gcc` |
| Rust 工具链 | `rustup target add riscv32imc-esp-espidf` | `espup install` |
| ESP-IDF install | `./install.sh esp32c5` | `./install.sh esp32s3` |
| ESP-IDF target | `CONFIG_IDF_TARGET="esp32c5"` | `CONFIG_IDF_TARGET="esp32s3"` |
| BLE Mesh 配置项 | `CONFIG_BT_BLE_MESH_*` | `CONFIG_BLE_MESH_*` |
| BT Host 配置项 | `CONFIG_BT_BLUEDROID` | `CONFIG_BT_BLUEDROID_ENABLED` |

## 验证过的环境组合

本次构建验证通过的环境（2026-07-08）：

| 组件 | 版本 |
|------|------|
| ESP-IDF | v5.5.4 |
| esp-idf-sys | 0.37.2 |
| esp-idf-hal | 0.46.2 |
| esp-idf-svc | 0.52 |
| embuild | 0.33.1 |
| ldproxy | 0.3.4 |
| Xtensa GCC | esp-14.2.0_20260121 |
| Rust toolchain | esp (Xtensa nightly 1.90.0.0) |
| LLVM (libclang) | Homebrew llvm 22.1.1 (`/usr/local/opt/llvm`) |
| Rust edition | 2024 |

构建命令：
```bash
. $HOME/export-esp.sh
export PATH="$HOME/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20260121/xtensa-esp-elf/bin:$PATH"
cargo build --release
```

构建结果：
- Debug: 23 MB (含调试符号, 126 warnings, 0 errors)
- Release: 1.6 MB (已 strip, fat LTO)
- 编译时间: 首次 ~30 分钟, 增量 ~25 秒


## LOOP1-7 累计修复完成

| LOOP | 修复内容 | 提交 |
|------|----------|------|
| LOOP1 | 路径覆盖 (Metuory 11 读路径 length-prefix 格式) | `a10a9ef` |
| LOOP2 | TCP 写 panic 修复 (线程栈 12KB→20KB) | `25e83f4` |
| LOOP3 | BLE 名字 MBAP.length 修复 + I2C 旧驱动警告抑制 | `22a6e0a`, `eac88db` |
| LOOP4 | SN/LOCATION 默认值调整, 长度适配 Modbus 容量 | `88c7cc9` |
| LOOP5 | TCP 连接线程栈 20KB (12KB 栈溢出修复) | `25e83f4` |
| LOOP6 | I2C_SKIP_LEGACY_CONFLICT_CHECK 配置 | `eac88db` |
| LOOP7 | BLE 名字 GAP 同步 (ble_name_str UTF-16 BE 解码) | `6c51359` |

## 完整构建命令 (LOOP7 后)

```bash
# 1. 准备环境 (一次性)
. $HOME/export-esp.sh  # 或: . $HOME/esp/esp-idf/export.sh
export PATH="$HOME/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20260121/xtensa-esp-elf/bin:$PATH"

# 2. 完整构建 (Debug 版, 含完整调试信息)
cargo build

# 3. 完整构建 (Release 版, 优化 + strip)
cargo build --release

# 4. 仅 cargo check (不链接, 快速验证编译)
cargo check

# 5. 单元测试 (编译测试 binary, ESP-IDF 平台需 qemu 跑)
cargo test --bin gateway --no-run
```

### 关键 cargo 命令解释

| 命令 | 作用 | 时间 |
|------|------|------|
| `cargo build` | Debug 构建 (不优化, 1.4MB binary) | ~25s 增量 |
| `cargo build --release` | Release 构建 (LTO + strip, 1.6MB) | ~3min 全量 |
| `cargo check` | 编译检查, 不链接生成 binary | ~10s |
| `cargo test --no-run` | 编译测试 binary (Xtensa) | ~25s |
| `cargo clean` | 清理 target/ 目录 | 0s |
| `touch build.rs && rm -rf target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-*` | 强制重生成 esp-idf-sys (App version 等) | 3min |

### 常见问题 (LOOP7 经验)

1. **App version 不更新**: cargo build 不会重链接 esp-idf-sys, 烧录后 App version 还是旧的
   - 解决: `touch build.rs && rm -rf target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-* && cargo build`

2. **DTR/RTS 软复位失灵**: CH340 在反复擦除后软复位不响应, 必须冷启动
   - 解决: `ser.dtr = True; time.sleep(0.5); ser.dtr = False` (拉低 DTR 500ms 让 EN 断电)
   - 或完全断 USB 重插

3. **BLE 名字写入搜不到设备**: 见 LOOP7 修复
   - 修复前 `ble_name_str()` 返回空字符串, GAP 设备名不更新
   - 修复后按 BE 字节序解码, 正确返回名字
