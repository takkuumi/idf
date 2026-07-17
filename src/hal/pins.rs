//! 引脚分组容器
//!
//! `HalPins::split` 消费 `peripherals.pins` 以独占引脚资源，
//! 实际引脚号从 `crate::config::pins` 读取，供各 HAL 子模块 init 使用。
//!
//! # ESP32-S3 说明
//!
//! ESP32-S3 有 45 个 GPIO (GPIO0~GPIO48), GPIO26~31 被 Quad SPI Flash/PSRAM 占用。
//! 本模块不依赖 `Pins` 的具体字段，而是直接使用 GPIO 编号 (u8)。
//! 各子模块通过 `esp_idf_hal::gpio::AnyInputPin::new(num)` /
//! `AnyOutputPin::new(num)` 等 unsafe 构造器创建类型擦除引脚。

use esp_idf_hal::gpio::Pins;

use crate::config::pins as cfg;
use crate::error::AppResult;

/// ADC1 配置 (单元号 + 通道号列表)
#[derive(Clone, Copy)]
pub struct Adc1Cfg {
    pub unit: u8,
    pub channels: [u8; 6],
}

/// LEDC 定时器配置
#[derive(Clone, Copy)]
pub struct LedcTimerCfg {
    pub timer: u8,
    pub freq_hz: u32,
    pub resolution_bits: u8,
}

/// 所有引脚的分组容器。
///
/// 字段均为 GPIO 编号或简单配置结构，由各 HAL 子模块的 `init` 函数
/// 接收后创建实际的 `PinDriver` / `SpiDriver` 等驱动对象。
pub struct HalPins {
    // ---- SPI2 (W5500) ----
    pub spi_mosi: u8,
    pub spi_miso: u8,
    pub spi_sclk: u8,
    pub spi_cs: u8,

    // ---- UART (3 路) ----
    /// UART0 TX (下载/日志, 通常 GPIO43)
    pub uart0_tx: u8,
    /// UART0 RX (下载/日志, 通常 GPIO44)
    pub uart0_rx: u8,
    /// UART1 TX (RS485 #0 主站)
    pub uart1_tx: u8,
    /// UART1 RX (RS485 #0 主站)
    pub uart1_rx: u8,
    /// UART2 TX (RS485 #1 从站)
    pub uart2_tx: u8,
    /// UART2 RX (RS485 #1 从站)
    pub uart2_rx: u8,

    // ---- ADC1 ----
    pub adc1: Adc1Cfg,

    // ---- LEDC ----
    pub ledc_timer: LedcTimerCfg,
    /// (ledc_channel, gpio_num)
    pub ledc_channels: [(u8, u8); 4],

    // ---- GPIO: ETH / RS485 (所有版本共用) ----
    pub eth_int: u8,
    pub eth_rst: u8,
    /// RS485 #0 / #1 的 DE 引脚
    pub rs485_de: [u8; 2],

    // ---- DI / DO 引脚号 (仅 io-di-do 版本, F3/F4 用 I2C 扩展) ----
    #[cfg(all(feature = "io-di-do", not(any(feature = "f3", feature = "f4"))))]
    pub di: [u8; 8],
    #[cfg(all(feature = "io-di-do", not(any(feature = "f3", feature = "f4"))))]
    pub do_: [u8; 8],
}

impl HalPins {
    /// 拆分 `Peripherals::pins` 并按 `config::pins` 分组。
    ///
    /// `_pins` 被消费以独占引脚所有权（防止重复使用），
    /// 实际引脚号从全局配置常量读取。
    pub fn split(_pins: Pins) -> AppResult<Self> {
        Ok(Self {
            spi_mosi: cfg::ETH_SPI_MOSI,
            spi_miso: cfg::ETH_SPI_MISO,
            spi_sclk: cfg::ETH_SPI_SCLK,
            spi_cs: cfg::ETH_SPI_CS,

            // UART0: 下载/日志 (ESP32-S3 默认 GPIO43/44)
            uart0_tx: 43,
            uart0_rx: 44,
            // UART1: RS485 #0 (主站)
            uart1_tx: cfg::RS485_0_TX,
            uart1_rx: cfg::RS485_0_RX,
            // UART2: RS485 #1 (从站)
            uart2_tx: cfg::RS485_1_TX,
            uart2_rx: cfg::RS485_1_RX,

            adc1: Adc1Cfg {
                unit: cfg::AI_ADC_UNIT,
                channels: cfg::AI_CHANNELS,
            },

            ledc_timer: LedcTimerCfg {
                timer: 0,
                freq_hz: cfg::AO_FREQ_HZ,
                resolution_bits: cfg::AO_RESOLUTION_BITS,
            },
            ledc_channels: cfg::AO_CHANNELS,

            #[cfg(all(feature = "io-di-do", not(any(feature = "f3", feature = "f4"))))]
            di: cfg::DI_PINS,
            #[cfg(all(feature = "io-di-do", not(any(feature = "f3", feature = "f4"))))]
            do_: cfg::DO_PINS,
            eth_int: cfg::ETH_INT,
            eth_rst: cfg::ETH_RST,
            rs485_de: [cfg::RS485_0_DE, cfg::RS485_1_DE],
        })
    }
}

// 注: F3/F4 版本下 HalPins 不包含 di/do_ 字段 (用 IoExtender 经 MCP23017 扩展),
// split 也不设置 di/do_, 不引用 cfg::DI_PINS/DO_PINS。
