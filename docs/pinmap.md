# GPIO 引脚分配表

> 硬件平台：ESP32-S3R8 (Xtensa LX7 双核 240MHz, 512KB SRAM, **8MB Octal SPI PSRAM**)
>
> ESP32-S3 共 45 个 GPIO (GPIO0~GPIO48)，其中：
> - **GPIO26~32**：被 Octal SPI Flash/PSRAM 占用，**不可用**
> - **GPIO0**：strapping（boot mode，需外部上拉）
> - **GPIO3**：strapping（JTAG source）
> - **GPIO45/46**：strapping（VDD_SPI / system freq）
> - **GPIO43/44**：UART0 默认 TX/RX（下载/日志）
> - **GPIO19/20**：USB Serial/JTAG（可作普通 GPIO，建议避开）
>
> 当前 `config.rs` 中所有引脚均在可用范围内。

## 当前分配（与 `src/config.rs` 一致）

| 模块 | 引脚号 | 功能 | 备注 |
|------|--------|------|------|
| **SPI2 (W5500)** | GPIO11 | MOSI | W5500 SPI 主出从入 |
| | GPIO13 | MISO | W5500 SPI 主入从出 |
| | GPIO12 | SCLK | W5500 SPI 时钟 |
| | GPIO10 | CS | W5500 SPI 片选 |
| | GPIO14 | INT | W5500 中断，低有效 |
| | GPIO15 | RST | W5500 复位，低有效 |
| **UART0 (下载/日志)** | GPIO43 | TX | 115200-N-8-1 |
| | GPIO44 | RX | 与下载串口共用 |
| **UART1 (RS485 #0)** | GPIO40 | TX | RTU Master |
| | GPIO41 | RX | |
| | GPIO42 | DE/RE | RTS 共控（高=发送，低=接收） |
| **UART2 (RS485 #1)** | GPIO17 | TX | RTU Slave |
| | GPIO18 | RX | |
| | GPIO7 | DE/RE | RTS 共控 |
| **DI (8 路)** | GPIO19/20/21/33/34/35/36/37 | DI0~DI7 | 光耦隔离，Pull Down |
| **DO (8 路)** | GPIO8/9/16/38/39/45/46/48 | DO0~DO7 | OC 输出 |
| **AI (6 路, ADC1)** | ADC1_CH0~5 | AI0~AI5 | 12-bit SAR ADC，对应 GPIO1~GPIO6 |
| **AO (4 路, LEDC)** | GPIO8/9/16/38 | AO0~AO3 | 与 DO 复用（硬件互斥） |

## ADC1 通道映射 (ESP32-S3)

ESP32-S3 ADC1 有 10 个通道 (CH0~CH9)，本系统使用前 6 个：

| ADC 通道 | 对应 GPIO | 用途 |
|----------|-----------|------|
| ADC1_CH0 | GPIO1 | AI0 |
| ADC1_CH1 | GPIO2 | AI1 |
| ADC1_CH2 | GPIO3 | AI2 |
| ADC1_CH3 | GPIO4 | AI3 |
| ADC1_CH4 | GPIO5 | AI4 |
| ADC1_CH5 | GPIO6 | AI5 |

> **引脚冲突说明**：GPIO3 (ADC1_CH2) 同时是 strapping pin (JTAG source)，但 ADC 为高阻抗输入，上电后不影响 strapping 值，硬件可放心接入模拟信号。
>
> `config.rs::AI_CHANNELS = [0, 1, 2, 3, 4, 5]` 表示 ADC 通道号 (非 GPIO 号)，由 `AdcChannelDriver` 内部映射到 GPIO1-6。

## 引脚分配设计原则

1. **避开 Flash/PSRAM 引脚**：GPIO26~32 不可用
2. **strapping 引脚谨慎使用**：GPIO0/3/45/46 上电瞬间被读取，启动后可作普通 GPIO（ADC 输入、DO 输出）；硬件设计需确保上电电平不冲突
3. **避开 USB Serial/JTAG**：GPIO19/20（如需 USB 调试）；本系统已用作 DI0/DI1
4. **UART0 独占下载/日志**：ESP32-S3 有 3 个 UART，不再需要 RS485 与下载串口复用
5. **ADC1 引脚独占**：GPIO1-6 不与 UART/SPI 冲突
6. **RS485 主站用高位 GPIO**：UART1 TX/RX/DE = GPIO40/41/42，避开 ADC1 和 strapping

## 修改方法

修改 `/Users/ling/Workspace/idf/src/config.rs` 中的 `pins` 模块：

```rust
pub mod pins {
    // ---- 以太网 W5500 (SPI2_HOST) ----
    pub const ETH_SPI_HOST: u8 = 2;
    pub const ETH_SPI_MOSI: u8 = 11;
    pub const ETH_SPI_MISO: u8 = 13;
    pub const ETH_SPI_SCLK: u8 = 12;
    pub const ETH_SPI_CS: u8 = 10;
    pub const ETH_INT: u8 = 14;
    pub const ETH_RST: u8 = 15;

    // ---- RS485 #0 (UART1, 主站) ----
    pub const RS485_0_UART: u8 = 1;
    pub const RS485_0_TX: u8 = 40;
    pub const RS485_0_RX: u8 = 41;
    pub const RS485_0_DE: u8 = 42;

    // ---- RS485 #1 (UART2, 从站) ----
    pub const RS485_1_UART: u8 = 2;
    pub const RS485_1_TX: u8 = 17;
    pub const RS485_1_RX: u8 = 18;
    pub const RS485_1_DE: u8 = 7;

    // ---- DI 8 路 ----
    pub const DI_PINS: [u8; 8] = [19, 20, 21, 33, 34, 35, 36, 37];

    // ---- DO 8 路 ----
    pub const DO_PINS: [u8; 8] = [8, 9, 16, 38, 39, 45, 46, 48];

    // ---- AI 6 路 (ADC1, 通道号非 GPIO 号) ----
    pub const AI_ADC_UNIT: u8 = 1;
    pub const AI_CHANNELS: [u8; 6] = [0, 1, 2, 3, 4, 5];

    // ---- AO 4 路 (LEDC PWM) ----
    pub const AO_CHANNELS: [(u8, u8); 4] = [
        (0, 8), (1, 9), (2, 16), (3, 38),
    ];
    pub const AO_FREQ_HZ: u32 = 5000;
    pub const AO_RESOLUTION_BITS: u8 = 12;
}
```

## GPIO 占用总览图

```
GPIO0  (strapping)   GPIO26~32 (Flash/PSRAM, 不可用)
GPIO1  AI0 (ADC1_0)  GPIO33 DI3
GPIO2  AI1 (ADC1_1)  GPIO34 DI4
GPIO3  AI2 (ADC1_2)  GPIO35 DI5   (strapping)
GPIO4  AI3 (ADC1_3)  GPIO36 DI6
GPIO5  AI4 (ADC1_4)  GPIO37 DI7
GPIO6  AI5 (ADC1_5)  GPIO38 DO3
GPIO7  RS485_1 DE    GPIO39 DO4
GPIO8  DO0/AO0       GPIO40 RS485_0 TX (UART1)
GPIO9  DO1/AO1       GPIO41 RS485_0 RX
GPIO10 W5500 CS      GPIO42 RS485_0 DE
GPIO11 W5500 MOSI    GPIO43 UART0 TX (下载/日志)
GPIO12 W5500 SCLK    GPIO44 UART0 RX
GPIO13 W5500 MISO    GPIO45 DO5 (strapping)
GPIO14 W5500 INT     GPIO46 DO6 (strapping)
GPIO15 W5500 RST     GPIO47 (未分配)
GPIO16 DO2/AO2       GPIO48 DO7
GPIO17 RS485_1 TX    (UART2)
GPIO18 RS485_1 RX
GPIO19 DI0           (USB Serial/JTAG, 可作 GPIO)
GPIO20 DI1           (USB Serial/JTAG, 可作 GPIO)
GPIO21 DI2
```

## 硬件版本 F3 / F4 (I2C MCP23017 扩展)

通过编译期 feature flag 切换硬件版本，F3/F4 用 I2C MCP23017 扩展芯片扩展 DI/DO 通道数。

### 版本对比

| 版本 | feature | DI 通道 | DO 通道 | MCP23017 数量 | 启用命令 |
|------|---------|---------|---------|---------------|---------|
| Default | (无) | 8 (GPIO 直驱) | 8 (GPIO 直驱) | 0 | `cargo build` |
| F3 | `f3` | 16 | 16 | 2 (DI=0x20, DO=0x21) | `cargo build --features f3` |
| F4 | `f4` | 48 | 16 | 4 (DI=0x20/0x21/0x22, DO=0x23) | `cargo build --features f4` |

> F3 与 F4 互斥，不能同时启用（build.rs 编译期校验）。

### F3/F4 版本 GPIO 占用差异

F3/F4 版本下，原 DI 占用的 GPIO 释放，其中 GPIO21/GPIO33 给 I2C 总线使用：

| GPIO | 默认版本 | F3/F4 版本 |
|------|---------|-----------|
| GPIO19 | DI0 | 释放（可作他用，本系统不占用） |
| GPIO20 | DI1 | 释放 |
| GPIO21 | DI2 | **I2C SDA** (MCP23017 数据线) |
| GPIO33 | DI3 | **I2C SCL** (MCP23017 时钟线) |
| GPIO34 | DI4 | 释放 |
| GPIO35 | DI5 | 释放 |
| GPIO36 | DI6 | 释放 |
| GPIO37 | DI7 | 释放 |

DO 在所有版本下均由原 GPIO 直驱（F3/F4 也不变），仅 DI 全部走 I2C 扩展。

> 注：实际硬件中 DO 通道数从 8 扩展到 16，由 MCP23017 输出。原 8 路 DO GPIO 引脚在 F3/F4 版本下释放（GPIO8/9/16/38/39/45/46/48）。

### MCP23017 I2C 地址分配

MCP23017 地址范围 0x20-0x27（A0/A1/A2 引脚组合），本系统分配：

**F3 版本**（2 片）：
| 芯片 | I2C 地址 | A2/A1/A0 | 用途 |
|------|---------|----------|------|
| U1 | 0x20 | 0/0/0 | DI 0-15 (PORTA=DI0-7, PORTB=DI8-15) |
| U2 | 0x21 | 0/0/1 | DO 0-15 (PORTA=DO0-7, PORTB=DO8-15) |

**F4 版本**（4 片）：
| 芯片 | I2C 地址 | A2/A1/A0 | 用途 |
|------|---------|----------|------|
| U1 | 0x20 | 0/0/0 | DI 0-15 |
| U2 | 0x21 | 0/0/1 | DI 16-31 |
| U3 | 0x22 | 0/1/0 | DI 32-47 |
| U4 | 0x23 | 0/1/1 | DO 0-15 |

### I2C 总线参数

- **I2C 端口**：I2C0 (`config::pins::I2C_PORT`)
- **SDA**：GPIO21 (`config::pins::I2C_SDA`)
- **SCL**：GPIO33 (`config::pins::I2C_SCL`)
- **频率**：400kHz Fast Mode (`config::pins::I2C_FREQ_HZ`)，MCP23017 支持到 1.7MHz
- **上拉**：MCP23017 内部上拉使能（GPPU 寄存器），外部可不接上拉电阻
