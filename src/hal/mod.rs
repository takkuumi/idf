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
//! # I2C 总线
//!
//! 默认构建由 [`io_ext`] 管理 MCP23017；F3/F4 构建由 [`pca9555`] 管理 PCA9555。

use esp_idf_hal::peripherals::Peripherals;

// AppError 在 HAL 初始化和各 IO 变体中都会使用；保留统一导入以避免 cfg 分叉。
#[allow(unused_imports)]
use crate::error::{AppError, AppResult};

pub mod adc;
pub mod digital_io;
pub mod gpio;
#[cfg(not(any(feature = "f3", feature = "f4")))]
pub mod i2c_bus;
#[cfg(not(any(feature = "f3", feature = "f4")))]
pub mod io_ext;
pub mod ledc;
#[cfg(not(any(feature = "f3", feature = "f4")))]
pub mod mcp23017;
/// PCA9555 IO 扩展芯片驱动 (F3/F4)
#[cfg(any(feature = "f3", feature = "f4"))]
pub mod pca9555;
pub mod pins;
/// Software I2C bit-bang (LOOP12: 任何 feature 下都需保留, 因为 NFC ST25DV64KC
/// 通过 bit-bang 访问 LED/IO 总线引脚, 不依赖硬件 I2C0).
pub mod sw_i2c;
pub mod uart;

pub use adc::AdcHandle;
#[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
pub use digital_io::DigitalIo;
pub use gpio::GpioBank;
#[cfg(not(any(feature = "f3", feature = "f4")))]
pub use io_ext::IoExtender;
pub use ledc::LedcHandle;
#[cfg(any(feature = "f3", feature = "f4"))]
pub use pca9555::Pca9555Duo;
pub use pins::HalPins;
pub use uart::UartPort;

/// Legacy FIB1/FIB2 detection inputs are active-low (GPIO39/GPIO40).
/// Shared by the physical indicators and Web; this is an input status,
/// not a test of optical payload delivery or ring redundancy.
pub fn fiber_status() -> [bool; 2] {
    [crate::config::pins::FIB1_PIN, crate::config::pins::FIB2_PIN]
        .map(|pin| unsafe { esp_idf_sys::gpio_get_level(pin as esp_idf_sys::gpio_num_t) } == 0)
}

/// 硬件抽象总句柄
pub struct Hal {
    pub pins: HalPins,
    pub uart: UartPort,
    pub adc: AdcHandle,
    pub ledc: LedcHandle,
    pub gpio: GpioBank,
    /// Wi-Fi 驱动需要拥有 modem 单例；放入有界同步槽，启动时只取一次。
    #[cfg(feature = "wifi")]
    wifi_modem: crate::sync::Spin<Option<esp_idf_hal::modem::Modem<'static>>>,
    /// MCP23017 IO 扩展 (默认构建)
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub io_ext: IoExtender,
    /// PCA9555 IO + LED 多芯片组 (F3/F4, 通过软件 I2C)
    #[cfg(any(feature = "f3", feature = "f4"))]
    pub pca9555: Pca9555Duo,
}

impl Hal {
    pub fn init(peripherals: Peripherals) -> AppResult<Self> {
        let pins = HalPins::split(peripherals.pins)?;
        let uart = UartPort::init(
            pins.uart0_tx,
            pins.uart0_rx,
            pins.uart1_tx,
            pins.uart1_rx,
            pins.uart2_tx,
            pins.uart2_rx,
        )?;
        let adc = AdcHandle::init(pins.adc1)?;
        let ledc = LedcHandle::init(pins.ledc_timer, pins.ledc_channels)?;

        // GpioBank 初始化: DI/DO 由 MCP23017 (default) 或 PCA9555 (F3/F4) 管理
        // GPIO 仅管理辅助引脚 (ETH_INT, ETH_RST, RS485_DE)
        let gpio = GpioBank::init(pins.eth_int, pins.eth_rst, pins.rs485_de, None)?;

        // 原 MCA 在 setup() 中将 GPIO39/40 配置为普通输入后再读取光纤检测状态。
        // 该配置属于硬件初始化职责，不能放到 Web 请求热路径中反复执行。
        {
            use esp_idf_sys::{gpio_config, gpio_config_t, gpio_mode_t_GPIO_MODE_INPUT};
            let cfg = gpio_config_t {
                pin_bit_mask: (1u64 << crate::config::pins::FIB1_PIN)
                    | (1u64 << crate::config::pins::FIB2_PIN),
                mode: gpio_mode_t_GPIO_MODE_INPUT,
                pull_up_en: 0,
                pull_down_en: 0,
                intr_type: 0,
            };
            let ret = unsafe { gpio_config(&cfg) };
            if ret != 0 {
                return Err(AppError::Io(format!(
                    "fiber input gpio config: esp_err=0x{ret:08X}"
                )));
            }
        }

        // 默认版本: 初始化 I2C IO 扩展 (MCP23017)
        #[cfg(not(any(feature = "f3", feature = "f4")))]
        let io_ext = {
            use crate::config::pins as p;
            let i2c_bus =
                crate::hal::i2c_bus::I2cBus::init(peripherals.i2c0, p::I2C_SDA, p::I2C_SCL)?;
            IoExtender::init(i2c_bus)?
        };

        // F3/F4 版本: 先给 PCA9555 IO/LED 板供电，再初始化软件 I2C
        // GPIO21: 灯板电源使能 (HIGH=上电)
        // GPIO33: 继电器板 JDQ_24V_EN (HIGH=上电)
        #[cfg(any(feature = "f3", feature = "f4"))]
        {
            use esp_idf_sys::{
                gpio_config, gpio_config_t, gpio_mode_t_GPIO_MODE_OUTPUT, gpio_set_level,
            };
            for &pwr_pin in &[
                crate::config::pins::POWER_LED_EN,
                crate::config::pins::POWER_RELAY_EN,
            ] {
                let cfg = gpio_config_t {
                    pin_bit_mask: 1u64 << (pwr_pin as u64),
                    mode: gpio_mode_t_GPIO_MODE_OUTPUT,
                    pull_up_en: 0u32,
                    pull_down_en: 0u32,
                    intr_type: 0,
                };
                let ret = unsafe { gpio_config(&cfg) };
                if ret != 0 {
                    return Err(AppError::Io(format!(
                        "power_en gpio{pwr_pin} config: esp_err=0x{ret:08X}"
                    )));
                }
                unsafe { gpio_set_level(pwr_pin as i32, 1) }; // HIGH = 上电
            }
            log::info!("[hal] IO/LED board power enabled (gpio21 + gpio33 HIGH)");
        }

        // F3/F4 版本: 初始化 PCA9555 多芯片 IO + LED 组
        // IO 总线:   SDA=GPIO35, SCL=GPIO36 (DI@0x40, DO@0x42)
        // LED 总线:  SDA=GPIO38, SCL=GPIO37 (DI_LED@0x40, DO_LED@0x48)
        #[cfg(any(feature = "f3", feature = "f4"))]
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
            #[cfg(feature = "wifi")]
            wifi_modem: crate::sync::Spin::new(Some(peripherals.modem)),
            #[cfg(not(any(feature = "f3", feature = "f4")))]
            io_ext,
            #[cfg(any(feature = "f3", feature = "f4"))]
            pca9555,
        })
    }

    #[cfg(feature = "wifi")]
    pub fn take_wifi_modem(&self) -> AppResult<esp_idf_hal::modem::Modem<'static>> {
        self.wifi_modem
            .lock()
            .take()
            .ok_or_else(|| AppError::Sys("wifi modem already taken".into()))
    }

    /// 获取数字 IO 抽象接口 (DI/DO 统一访问)
    ///
    /// 根据编译期硬件版本返回不同的实现:
    /// - 默认版本: 返回 `&IoExtender` (I2C MCP23017 扩展)
    /// - F3/F4 版本: 返回 `&Pca9555Duo` (软件 I2C PCA9555 组)
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
    /// 默认返回 MCP23017，F3/F4 返回 PCA9555 组。
    #[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
    pub fn dio(&self) -> &dyn DigitalIo {
        // 默认版本: MCP23017 通过硬件 I2C (GPIO21/33)
        #[cfg(not(any(feature = "f3", feature = "f4")))]
        {
            &self.io_ext
        }
        // F3/F4 版本: PCA9555 通过软件 I2C (GPIO35/36 + GPIO38/37)
        #[cfg(any(feature = "f3", feature = "f4"))]
        {
            &self.pca9555
        }
    }

    /// Update the physical Bluetooth status LED where this board variant has
    /// the MCA-compatible status expander.
    pub fn set_bt_led(&self, connected: bool) -> AppResult<()> {
        #[cfg(any(feature = "f3", feature = "f4"))]
        {
            self.pca9555.set_bt_led(connected)
        }
        #[cfg(not(any(feature = "f3", feature = "f4")))]
        {
            let _ = connected;
            Ok(())
        }
    }

    /// Update RS485 and fiber indicators together on PCA9555 boards.
    pub fn set_port_leds(&self, rs485: [bool; 2], fiber: [bool; 2]) -> AppResult<()> {
        #[cfg(any(feature = "f3", feature = "f4"))]
        {
            self.pca9555.set_port_leds(rs485, fiber)
        }
        #[cfg(not(any(feature = "f3", feature = "f4")))]
        {
            let _ = (rs485, fiber);
            Ok(())
        }
    }
}
