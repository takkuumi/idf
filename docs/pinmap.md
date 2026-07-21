# GPIO 引脚分配表 (本系统最终版本)

> 硬件平台：**ESP32-S3R2** (Xtensa LX7 双核 240MHz, 512KB SRAM, **2MB Quad SPI PSRAM**)
>
> **本表与 `src/config.rs::pins` 100% 一致**。修改引脚时只改 `config.rs` 一处, 本表同步更新。
>
> 最后核对：2026-07-21 (esp-idf 启动日志确认 PSRAM=2MB)

## ESP32-S3 GPIO 总览

ESP32-S3 共 45 个 GPIO (GPIO0~GPIO48)，其中：
- **GPIO26~32**：被 Quad SPI Flash/PSRAM 占用，**不可用**
- **GPIO0**：strapping（boot mode，需外部上拉）
- **GPIO3**：strapping（JTAG source）
- **GPIO45/46**：strapping（VDD_SPI / system freq）
- **GPIO43/44**：UART0 默认 TX/RX（下载/日志）
- **GPIO19/20**：USB Serial/JTAG（可作普通 GPIO）

## 实际引脚分配（与 config.rs::pins 1:1 对应）

| 模块 | 引脚号 | 功能 | 备注 |
|------|--------|------|------|
| **W5500 以太网 (SPI3)** | GPIO12 | MOSI | 主出从入 |
| | GPIO11 | MISO | 主入从出 |
| | GPIO10 | SCLK | 时钟 |
| | GPIO9 | CS | 片选 |
| | GPIO13 | INT | 中断，低有效 |
| | GPIO14 | RST | 复位，低有效 |
| **UART0 (下载/日志)** | GPIO43 | TX | 115200-N-8-1 |
| | GPIO44 | RX | 与下载串口共用 |
| **UART1 (RS485 #0)** | GPIO45 | TX | RTU Master |
| | GPIO46 | RX | |
| | GPIO7 | DE/RE | 高=发送 |
| **UART2 (RS485 #1)** | GPIO42 | TX | RTU Slave |
| | GPIO41 | RX | |
| | GPIO8 | DE/RE | 高=发送 |
| **电源使能** | GPIO21 | LED_EN | 灯板电源 HIGH=上电 |
| | GPIO33 | RELAY_EN | 继电器板 JDQ_24V_EN |
| **拨码开关** | GPIO19 | AD0 | RS485 地址码 |
| | GPIO20 | AD1 | |
| | GPIO48 | AD2 | |
| | GPIO47 | AD3 | |
| | GPIO34 | ESP_STOP | 与 NCA9555 INT 共用 |
| **PCA9555 (NCA9555) IO 扩展** | GPIO35 | I2C_SDA | IO 总线数据线 |
| | GPIO36 | I2C_SCL | IO 总线时钟线 |
| | GPIO38 | LED_SDA | LED 板 I2C SDA |
| | GPIO37 | LED_SCL | LED 板 I2C SCL |
| **光纤检测** | GPIO39 | FIB1 | 光纤输入 1 |
| | GPIO40 | FIB2 | 光纤输入 2 |
| **AI (6 路, ADC1)** | GPIO1~6 | AI0~AI5 | 12-bit SAR ADC |
| **AO (4 路, LEDC PWM)** | GPIO15/16/17/18 | AO0~AO3 | 5kHz, 12-bit |

### DI/DO 通道 (通过 PCA9555 I2C 扩展)

⚠️ **本系统 DI/DO 全部通过 PCA9555 (NCA9555) I2C 扩展，不使用 ESP32 GPIO 直驱**。
这是因为 ESP32-S3R2 上没有足够空闲 GPIO 给 16+ 路 DI/DO 直驱。

- **PCA9555 #1 (I2C 总线 1)**：SDA=GPIO35, SCL=GPIO36
- **PCA9555 #2 (I2C 总线 2, LED 板)**：SDA=GPIO38, SCL=GPIO37

DI/DO 实际通道数取决于硬件版本：
| 版本 | feature | DI | DO | 实现 |
|------|---------|----|----|------|
| F16 (默认) | (无) | 16 | 16 | 1x PCA9555 (主 IO 板) |
| F3 | `f3` | 16 | 16 | 2x MCP23017 |
| F4 | `f4` | 48 | 16 | 4x MCP23017 |

## 与 MCA_F16V2_1_F48_BLE 引脚对齐参考

| 功能 | MCA (C++) | 本系统 (Rust) | 差异 |
|------|-----------|---------------|------|
| RS485 主 TX | 32 (Arduino GPIO) | 45 (ESP-IDF GPIO) | 物理引脚相同 (Arduino 32 = ESP-IDF 45 是同一 pad) |
| RS485 主 RX | 33 | 46 | 同上 |
| PCA9555 SDA | 35 | 35 | ✅ 一致 |
| PCA9555 SCL | 36 | 36 | ✅ 一致 |
| LED PCA9555 SDA | 38 | 38 | ✅ 一致 |
| LED PCA9555 SCL | 37 | 37 | ✅ 一致 |
| RS485 UART | 2 (HardwareSerial) | UART1 | ⚠️ 不同实例 |

> 注：Arduino 风格 GPIO 编号与 ESP-IDF 编号对 ESP32-S3 **完全相同**（不像 ESP32 有 GPIO6~11 不可用的差异）。

## ADC1 通道映射 (ESP32-S3)

ESP32-S3 ADC1 有 10 个通道 (CH0~CH9)，本系统使用前 6 个：

| ADC 通道 | 对应 GPIO | 用途 |
|----------|-----------|------|
| ADC1_CH0 | GPIO1 | AI0 |
| ADC1_CH1 | GPIO2 | AI1 |
| ADC1_CH2 | GPIO3 | AI2 (strapping pin，但 ADC 高阻不影响) |
| ADC1_CH3 | GPIO4 | AI3 |
| ADC1_CH4 | GPIO5 | AI4 |
| ADC1_CH5 | GPIO6 | AI5 |

## 修改方法

修改 `src/config.rs::pins` 模块中的常量：

```rust
pub mod pins {
    // ---- 以太网 W5500 (SPI3_HOST) ----
    pub const ETH_SPI_HOST: u8 = 2;
    pub const ETH_SPI_MOSI: u8 = 12;
    pub const ETH_SPI_MISO: u8 = 11;
    pub const ETH_SPI_SCLK: u8 = 10;
    pub const ETH_SPI_CS: u8 = 9;
    pub const ETH_INT: u8 = 13;
    pub const ETH_RST: u8 = 14;

    // ---- RS485 #0 (UART1, 主站) ----
    pub const RS485_0_UART: u8 = 1;
    pub const RS485_0_TX: u8 = 45;
    pub const RS485_0_RX: u8 = 46;
    pub const RS485_0_DE: u8 = 7;

    // ---- RS485 #1 (UART2, 从站) ----
    pub const RS485_1_UART: u8 = 2;
    pub const RS485_1_TX: u8 = 42;
    pub const RS485_1_RX: u8 = 41;
    pub const RS485_1_DE: u8 = 8;

    // ---- 电源使能 ----
    pub const POWER_LED_EN: u8 = 21;
    pub const POWER_RELAY_EN: u8 = 33;

    // ---- 拨码开关 (启动时读 RS485 地址) ----
    pub const RS485_ADDR_PINS: [u8; 4] = [19, 20, 48, 47];
    pub const ESP_STOP_PIN: u8 = 34;

    // ---- PCA9555 (NCA9555) IO 扩展 ----
    pub const NCA9555_IIC_SCL: u8 = 36;
    pub const NCA9555_IIC_SDA: u8 = 35;
    pub const NCA9555_LED_SCL: u8 = 37;
    pub const NCA9555_LED_SDA: u8 = 38;
    pub const NCA9555_INT: u8 = 34;

    // ---- 光纤检测 ----
    pub const FIB1_PIN: u8 = 39;
    pub const FIB2_PIN: u8 = 40;

    // ---- AI 6 路 (ADC1) ----
    pub const AI_ADC_UNIT: u8 = 1;
    pub const AI_CHANNELS: [u8; 6] = [0, 1, 2, 3, 4, 5];

    // ---- AO 4 路 (LEDC) ----
    pub const AO_CHANNELS: [(u8, u8); 4] = [(0, 15), (1, 16), (2, 17), (3, 18)];
    pub const AO_FREQ_HZ: u32 = 5000;
    pub const AO_RESOLUTION_BITS: u8 = 12;
}
```

## GPIO 占用总览图

```
GPIO0  (strapping)   GPIO26 (Flash, 不可用)
GPIO1  AI0           GPIO27 (Flash, 不可用)
GPIO2  AI1           GPIO28 (Flash, 不可用)
GPIO3  AI2           GPIO29 (Flash, 不可用)
GPIO4  AI3           GPIO30 (Flash, 不可用)
GPIO5  AI4           GPIO31 (Flash, 不可用)
GPIO6  AI5           GPIO32 (Flash, 不可用)
GPIO7  RS485_0 DE    GPIO33 RELAY_EN
GPIO8  RS485_1 DE    GPIO34 ESP_STOP/NCA9555_INT
GPIO9  W5500 CS      GPIO35 NCA9555 I2C_SDA
GPIO10 W5500 SCLK    GPIO36 NCA9555 I2C_SCL
GPIO11 W5500 MISO    GPIO37 NCA9555 LED_SCL
GPIO12 W5500 MOSI    GPIO38 NCA9555 LED_SDA
GPIO13 W5500 INT     GPIO39 FIB1
GPIO14 W5500 RST     GPIO40 FIB2
GPIO15 AO0           GPIO41 RS485_1 RX
GPIO16 AO1           GPIO42 RS485_1 TX
GPIO17 AO2           GPIO43 UART0 TX (下载/日志)
GPIO18 AO3           GPIO44 UART0 RX
GPIO19 AD0           GPIO45 RS485_0 TX
GPIO20 AD1           GPIO46 RS485_0 RX
GPIO21 LED_EN        GPIO47 AD3
GPIO22 (未分配)      GPIO48 AD2
GPIO23 (未分配)      GPIO25 (未分配)
```

## 硬件版本 F3 / F4 (MCP23017 I2C 扩展)

通过编译期 feature flag 切换：

| 版本 | feature | DI | DO | MCP23017 |
|------|---------|----|----|----------|
| F16 (默认) | (无) | 16 | 16 | 0 (用 PCA9555) |
| F3 | `f3` | 16 | 16 | 2 (DI=0x20, DO=0x21) |
| F4 | `f4` | 48 | 16 | 4 |

### F3/F4 I2C 总线 (MCP23017)

- **SDA**: GPIO21 (注意：与默认版本的 POWER_LED_EN 共用，需软件时分复用)
- **SCL**: GPIO33 (与默认版本的 POWER_RELAY_EN 共用)
- **频率**: 400 kHz Fast Mode

> F3/F4 版本启动时，软件会先拉高 GPIO21/GPIO33（继电器上电），然后切到 I2C 模式复用同一引脚。

## 引脚分配设计原则

1. **避开 Flash/PSRAM 引脚**：GPIO26~32 不可用
2. **strapping 引脚谨慎使用**：GPIO0/3/45/46（启动后可作普通 GPIO）
3. **下载串口独占**：UART0 (GPIO43/44) 不可占用
4. **W5500 SPI 用高位 GPIO**：避开 ADC1 和 strapping
5. **PCA9555 软件 I2C**：不依赖硬件 I2C 外设，避免与 ESP-IDF HAL 冲突
6. **AI 用 ADC1**：GPIO1-6 不与 UART/SPI 冲突
