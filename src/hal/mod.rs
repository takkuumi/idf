//! 硬件抽象层
//!
//! 统一封装 ESP32-S3 的 GPIO/UART/ADC/LEDC/I2C 资源。
//! `Hal::init` 一次性创建所有外设句柄并按 `config::pins` 配置引脚。
//! 其它模块通过 `Arc<Hal>` 共享这些句柄。
//!
//! # SPI 总线
//!
//! SPI2_HOST 由 [`crate::ethernet::w5500`] 模块独占管理 (W5500 是 SPI 总线上
//! 唯一外设)，故 HAL 不再持有 SpiBus。引脚号仍集中在 [`crate::config::pins`]
//! 中 (`ETH_SPI_MOSI/MISO/SCLK/CS`)，由 ethernet 模块读取使用。
//!
//! # I2C 总线 (F3/F4 版本)
//!
//! I2C0 由 [`io_ext`] 模块独占管理 (接 MCP23017 IO 扩展芯片)。
//! 默认版本 (8 DI + 8 DO) 不创建 I2C, DI/DO 走 GPIO 直驱。

use esp_idf_hal::peripherals::Peripherals;

use crate::error::{AppError, AppResult};

pub mod pins;
pub mod uart;
pub mod adc;
pub mod ledc;
pub mod gpio;
pub mod digital_io;
#[cfg(any(feature_f3, feature_f4))]
pub mod i2c_bus;
#[cfg(any(feature_f3, feature_f4))]
pub mod mcp23017;
#[cfg(any(feature_f3, feature_f4))]
pub mod io_ext;
/// Software I2C + PCA9555 (default version: 8 DI + 8 DO via NCA9555)
#[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
pub mod sw_i2c;
#[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
pub mod pca9555;

pub use pins::HalPins;
pub use uart::UartPort;
pub use adc::AdcHandle;
pub use ledc::LedcHandle;
pub use gpio::GpioBank;
pub use digital_io::DigitalIo;
#[cfg(any(feature_f3, feature_f4))]
pub use io_ext::IoExtender;
#[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
pub use pca9555::Pca9555Duo;

/// 硬件抽象总句柄
pub struct Hal {
    pub pins: HalPins,
    pub uart: UartPort,
    pub adc: AdcHandle,
    pub ledc: LedcHandle,
    pub gpio: GpioBank,
    /// I2C IO 扩展 (仅 F3/F4 版本)
    #[cfg(any(feature_f3, feature_f4))]
    pub io_ext: IoExtender,
    /// PCA9555 IO + LED 双芯片 (默认版本, 通过软件 I2C)
    #[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
    pub pca9555: Pca9555Duo,
}

impl Hal {
    pub fn init(peripherals: Peripherals) -> AppResult<Self> {
        let pins = HalPins::split(peripherals.pins)?;
        let uart = UartPort::init(
            pins.uart0_tx, pins.uart0_rx,
            pins.uart1_tx, pins.uart1_rx,
            pins.uart2_tx, pins.uart2_rx,
        )?;
        let adc = AdcHandle::init(pins.adc1)?;
        let ledc = LedcHandle::init(pins.ledc_timer, pins.ledc_channels)?;

        // GpioBank 初始化: DI/DO 由 PCA9555 (default) 或 MCP23017 (F3/F4) 管理
        // GPIO 仅管理辅助引脚 (ETH_INT, ETH_RST, RS485_DE)
        let gpio = GpioBank::init(pins.eth_int, pins.eth_rst, pins.rs485_de)?;

        // F3/F4 版本: 初始化 I2C IO 扩展 (MCP23017)
        // I2C 引脚 (GPIO21 SDA + GPIO33 SCL) 在 F3/F4 版本下从原 DI 释放
        #[cfg(any(feature_f3, feature_f4))]
        let io_ext = {
            use crate::config::pins as p;
            let i2c_bus = crate::hal::i2c_bus::I2cBus::init(
                peripherals.i2c0,
                p::I2C_SDA,
                p::I2C_SCL,
            )?;
            IoExtender::init(i2c_bus)?
        };

        // 默认版本 (8 DI + 8 DO): 先给 IO 扩展板供电, 再初始化 PCA9555
        // GPIO21: 灯板电源使能 (HIGH=上电)
        // GPIO33: 继电器板 JDQ_24V_EN (HIGH=上电)
        #[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
        {
            use esp_idf_sys::{gpio_config, gpio_config_t, gpio_mode_t_GPIO_MODE_OUTPUT, gpio_set_level};
            for &pwr_pin in &[crate::config::pins::POWER_LED_EN, crate::config::pins::POWER_RELAY_EN] {
                let cfg = gpio_config_t {
                    pin_bit_mask: 1u64 << (pwr_pin as u64),
                    mode: gpio_mode_t_GPIO_MODE_OUTPUT,
                    pull_up_en: 0u32,
                    pull_down_en: 0u32,
                    intr_type: 0,
                };
                let ret = unsafe { gpio_config(&cfg) };
                if ret != 0 {
                    return Err(AppError::Io(format!("power_en gpio{pwr_pin} config: esp_err=0x{ret:08X}")));
                }
                unsafe { gpio_set_level(pwr_pin as i32, 1) }; // HIGH = 上电
            }
            log::info!("[hal] IO/LED board power enabled (gpio21 + gpio33 HIGH)");
        }

        // 默认版本 (8 DI + 8 DO): 初始化双 PCA9555 (IO + LED)
        // IO 总线:   SDA=GPIO35, SCL=GPIO36 (DI@0x40, DO@0x42)
        // LED 总线:  SDA=GPIO38, SCL=GPIO37 (DI_LED@0x40, DO_LED@0x48)
        #[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
        let pca9555 = {
            Pca9555Duo::init(
                crate::config::pins::NCA9555_IIC_SDA as i32,
                crate::config::pins::NCA9555_IIC_SCL as i32,
                crate::config::pins::NCA9555_LED_SDA as i32,
                crate::config::pins::NCA9555_LED_SCL as i32,
            )?
        };

        Ok(Self {
            pins,
            uart,
            adc,
            ledc,
            gpio,
            #[cfg(any(feature_f3, feature_f4))]
            io_ext,
            #[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
            pca9555,
        })
    }

    /// 获取数字 IO 抽象接口 (DI/DO 统一访问)
    ///
    /// 根据编译期硬件版本返回不同的实现:
    /// - 默认版本 (8 DI + 8 DO): 返回 `&GpioBank` (GPIO 直驱)
    /// - F3/F4 版本: 返回 `&IoExtender` (I2C MCP23017 扩展)
    ///
    /// 上层通过 `hal.dio()` 访问 DI/DO, 无需关心底层是 GPIO 还是 I2C。
    /// trait object 的 vtable 调用约 1-2ns, 相对 I2C 600μs 完全可忽略。
    ///
    /// # 示例
    /// ```no_run
    /// let bits = hal.dio().read_di_all()?;
    /// hal.dio().write_do_all(0xFFFF)?;
    /// ```
    /// 获取数字 IO 抽象接口 (DI/DO 统一访问)
    ///
    /// 仅在 io-di-do 或 F3/F4 feature 启用时可用。
    /// 实际硬件 DI/DO 走 PCA9555 I2C 扩展, 待实现后此处返回 PCA9555 实例。
    #[cfg(any(feature_io_di_do, feature_f3, feature_f4))]
    pub fn dio(&self) -> &dyn DigitalIo {
        // 默认版本: PCA9555 通过软件 I2C (NCA9555 on GPIO 35/36)
        #[cfg(all(feature_io_di_do, not(any(feature_f3, feature_f4))))]
        {
            &self.pca9555
        }
        // F3/F4 版本: IoExtender 实现 DigitalIo (I2C MCP23017 扩展)
        #[cfg(any(feature_f3, feature_f4))]
        {
            &self.io_ext
        }
    }
}
