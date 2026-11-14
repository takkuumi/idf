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
- **GPIO43/44**：UART0 TX/RX（ROM 下载 UART，板载 LoRa RXD/TXD 共用）
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
| **UART0 (ROM 下载 / F3 LoRa profile)** | GPIO43 | TX | 板载 E22 RXD；F3 `lora` profile 中作为第三路 RTU 逻辑端口 |
| | GPIO44 | RX | 板载 E22 TXD；标准调试 profile 才将其用于运行期控制台 |
| **UART1 (RS485 #0)** | GPIO45 | TX | RTU Master |
| | GPIO46 | RX | |
| | GPIO7 | SDMMC0_DET_L | F3 接线表用途；不是 RS485 DE/RE |
| **UART2 (RS485 #1)** | GPIO42 | TX | RTU Slave |
| | GPIO41 | RX | |
| | GPIO8 | F3/F4 AI3 | ADC1_CH7；UART2 不使用 RTS/DE |
| **板级控制** | GPIO21 | `LORA_RST` (F3 表网名) | E22-400T22D D 型接口没有独立 RESET 脚；实际网路用途/极性未确认，固件保留旧行为并启动拉高，不发送复位脉冲 |
| **LoRa 模式控制** | GPIO26 | LORA_M0 | F3 表有标注，但当前 Quad Flash/PSRAM 占用，固件不驱动 |
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
| **环网 PHY** | GPIO39 | PHY_INT1 | PHY 中断输入，不是链路状态输入 |
| | GPIO40 | PHY_INT2 | PHY 中断输入，不是链路状态输入 |
| **AI (Default/F3)** | GPIO1/2/3/8 | AI0~AI3 | ADC1_CH0/1/2/7，12-bit SAR ADC |
| **AI (F4)** | GPIO1/2/3/8/15/16/17/18 | AI0~AI7 | ADC1_CH0/1/2/7 + ADC2_CH4/5/6/7 |
| **AO (Default/F3)** | GPIO15/16/17/18 | AO0~AO3 | 5kHz, 12-bit LEDC；F4 禁用以释放 AI4-AI7 |

GPIO39/40 在 F3 接线表中是环网 PHY 中断脚，不能直接作为 FIB1/FIB2 链路状态。
当前固件没有对应 PHY/环网驱动，因此不应将这两个脚的电平显示成光纤链路状态；
FIB 状态需待 PHY 驱动读取真实链路寄存器后再提供。

### DI/DO 通道（所有变体均使用 PCA9555）

DEFAULT/F3 使用一组 16DI/16DO PCA9555，F4 使用三组 48DI/48DO。IO 总线和指示灯总线都由软件 I2C 驱动；未插扩展板时系统以降级模式启动，DI/DO 操作报告硬件不可用。

- **IO 总线**：SDA=GPIO35，SCL=GPIO36
- **LED 总线**：SDA=GPIO38，SCL=GPIO37

| 版本 | feature | DI | DO | IO 总线地址 |
|------|---------|----|----|-------------|
| Default/F16、F3 | Default 无；F3 为 `f3` | 16 | 16 | DI `0x40`，DO `0x42` |
| F4 | `f4` | 48 | 48 | DI `0x40/0x44/0x42`，DO `0x48/0x4C/0x4E` |

F4 的 DI 逻辑顺序为 `0x40`→I1–I16、`0x44`→I17–I32、`0x42`→I33–I48；DO 顺序为 `0x48`→Q1–Q16、`0x4C`→Q17–Q32、`0x4E`→Q33–Q48。依旧固件 F48 实际读写代码，每片 PCA9555 的 P0 按位序直接映射，P1 才做 bit reverse；输入为低有效。Default/F3 的 DO 变换不同，继续保持各自旧版映射。

LED 总线地址与 IO 总线分开：16 点板使用 DI 镜像 `0x40`、状态 LED `0x44`、DO 镜像 `0x48`；F4 使用对应多芯片镜像和状态器件 `0x46`。

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

## ADC 通道映射 (ESP32-S3)

Default/F3 使用 ADC1 的四个物理通道；F4 额外使用 ADC2 的四个物理通道：

| ADC 通道 | 对应 GPIO | 用途 |
|----------|-----------|------|
| ADC1_CH0 | GPIO1 | AI0 |
| ADC1_CH1 | GPIO2 | AI1 |
| ADC1_CH2 | GPIO3 | AI2 |
| ADC1_CH7 | GPIO8 | F3/F4 AI3 |
| ADC2_CH4 | GPIO15 | F4 AI4 |
| ADC2_CH5 | GPIO16 | F4 AI5 |
| ADC2_CH6 | GPIO17 | F4 AI6 |
| ADC2_CH7 | GPIO18 | F4 AI7 |

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
    pub const RS485_0_DE: u8 = 255; // F3 接线表未给独立 DE/RE；GPIO7 是 SDMMC0_DET_L

    // ---- RS485 #1 (UART2, 从站) ----
    pub const RS485_1_UART: u8 = 2;
    pub const RS485_1_TX: u8 = 42;
    pub const RS485_1_RX: u8 = 41;
    pub const RS485_1_DE: u8 = 255; // 参考固件未使用 RTS/DE；GPIO8 是 AI3

    // ---- 板级控制 ----
    pub const LORA_BOARD_CTRL_PIN: u8 = 21;
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

    // ---- 环网 PHY 中断；不是 FIB 链路状态输入 ----
    pub const PHY_INT1_PIN: u8 = 39;
    pub const PHY_INT2_PIN: u8 = 40;
    pub const FIB1_PIN: u8 = 255;
    pub const FIB2_PIN: u8 = 255;

    // ---- AI 6 路 (ADC1) ----
    pub const AI_ADC_UNIT: u8 = 1;
    // F3/F4 ADC1 前四路；F4 AI4..AI7 由 ADC2 OneShot 读取 GPIO15..18。
    pub const AI_CHANNELS: [u8; 6] = [0, 1, 2, 7, 0, 0];

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
GPIO4  (未分配)      GPIO30 (Flash, 不可用)
GPIO5  (未分配)      GPIO31 (Flash, 不可用)
GPIO6  (未分配)      GPIO32 (Flash, 不可用)
GPIO7  SDMMC_DET_L   GPIO33 RELAY_EN
GPIO8  F3/F4 AI3     GPIO34 ESP_STOP/NCA9555_INT
GPIO9  W5500 CS      GPIO35 NCA9555 I2C_SDA
GPIO10 W5500 SCLK    GPIO36 NCA9555 I2C_SCL
GPIO11 W5500 MISO    GPIO37 NCA9555 LED_SCL
GPIO12 W5500 MOSI    GPIO38 NCA9555 LED_SDA
GPIO13 W5500 INT     GPIO39 PHY_INT1
GPIO14 W5500 RST     GPIO40 PHY_INT2
GPIO15 AO0           GPIO41 RS485_1 RX
GPIO16 AO1           GPIO42 RS485_1 TX
GPIO17 AO2           GPIO43 UART0 TX / LoRa_RXD
GPIO18 AO3           GPIO44 UART0 RX / LoRa_TXD
GPIO19 AD0           GPIO45 RS485_0 TX
GPIO20 AD1           GPIO46 RS485_0 RX
GPIO21 LORA_RST      GPIO47 AD3
GPIO22 (未分配)      GPIO48 AD2
GPIO23 (未分配)      GPIO25 (未分配)
```

## 硬件版本 Default / F3 / F4

通过编译期 feature flag 切换通道数量；三种版本的 DI/DO 全部使用 PCA9555：

| 版本 | feature | DI | DO | PCA9555 组 |
|------|---------|----|----|------------|
| Default/F16 | 无 | 16 | 16 | 单组 |
| F3 | `f3` | 16 | 16 | 单组 |
| F4 | `f4` | 48 | 48 | 多组 |

DEFAULT/F3/F4 的 IO SDA/SCL 均为 GPIO35/36，LED SDA/SCL 均为 GPIO38/37。F3 的 GPIO21 网名为 `LORA_RST`，但 E22 D 型接口无独立 RESET；板级实际网路用途待图纸确认，固件维持旧启动拉高行为。GPIO33 为继电器板电源使能。

扩展板可选：未插入或芯片暂时无应答时，系统保留网络、BLE、Web 和 RS485 启动；DI/DO 返回硬件不可用状态。

## 引脚分配设计原则

1. **避开 Flash/PSRAM 引脚**：GPIO26~32 不可用
2. **strapping 引脚谨慎使用**：GPIO0/3/45/46（启动后可作普通 GPIO）
3. **UART0 复用须匹配构建 profile**：F3 `lora` profile 关闭 UART0 控制台并由 RTU runtime 独占；标准调试 profile 使用控制台，不能用于 LoRa 透传验收。ROM 下载发生在应用启动前
4. **LoRa M0 暂不驱动**：F3 表中的 GPIO26 与当前 Quad Flash/PSRAM 保留区冲突，必须先完成模组/PCB 复用确认
4. **W5500 SPI 用高位 GPIO**：避开 ADC1 和 strapping
5. **PCA9555 软件 I2C**：不依赖硬件 I2C 外设，避免与 ESP-IDF HAL 冲突
6. **AI 按芯片 ADC 映射**：F3 使用 ADC1 GPIO1/2/3/8；F4 额外使用 ADC2 GPIO15/16/17/18；UART1/UART2 仅使用 GPIO45/46/42/41
