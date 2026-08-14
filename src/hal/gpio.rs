//! GPIO 辅助引脚集合 (ETH_INT / ETH_RST / RS485_DE / RUN_LED)
//!
//! 集中管理所有非 SPI/UART/ADC/LEDC/I2C 的 GPIO 资源：
//! - ETH_INT 输入 (W5500 中断，低有效)
//! - ETH_RST 输出 (W5500 复位，低脉冲)
//! - 2 路 RS485 DE 引脚 (高=发送，低=接收)
//! - 板载 RUN_LED（运行状态指示, 心跳闪烁）
//!
//! # 数字 IO (DI/DO) 归属
//!
//! 默认版本 (8 DI + 8 DO): 由 [`crate::hal::pca9555`] 通过软件 I2C 管理
//! F3/F4 版本 (16/48 DI + 16 DO): 由 [`crate::hal::io_ext`] 通过硬件 I2C + MCP23017 管理
//!
//! 上层通过 `crate::hal::Hal::dio()` trait object 统一访问, 无需关心底层硬件。
//!
//! # 线程安全
//!
//! 输入引脚 `is_high` 仅需 `&self`。
//! 输出引脚 `set_level` 需 `&mut self`，通过 `Spin` 短临界区串行化 (不阻塞内核).

use std::time::Duration;

use crate::sync::Spin;

use esp_idf_hal::gpio::{AnyInputPin, AnyOutputPin, Input, Level, Output, PinDriver};

use crate::error::{AppError, AppResult};

/// GPIO 辅助引脚集合 (不含 DI/DO, 后者由专门模块管理)
pub struct GpioBank {
    /// W5500 中断输入 (低有效)
    pub eth_int: PinDriver<'static, Input>,
    /// W5500 复位输出 (低脉冲)
    pub eth_rst: Spin<PinDriver<'static, Output>>,
    /// RS485 #0 / #1 的 DE 引脚
    pub rs485_de: [Spin<PinDriver<'static, Output>>; 2],
    /// 板载运行状态 LED (蓝灯, 用于 heartbeat)
    pub run_led: Spin<Option<PinDriver<'static, Output>>>,
}

impl GpioBank {
    /// 初始化 GPIO 辅助引脚 (DI/DO 由 PCA9555 或 MCP23017 管理)
    pub fn init(
        eth_int_pin: u8,
        eth_rst_pin: u8,
        rs485_de_pins: [u8; 2],
        run_led_pin: Option<u8>,
    ) -> AppResult<Self> {
        // W5500 INT (输入, 上拉)
        let eth_int = PinDriver::input(
            unsafe { AnyInputPin::steal(eth_int_pin) },
            esp_idf_hal::gpio::Pull::Up,
        )
        .map_err(|e| AppError::Hal(format!("eth_int gpio{eth_int_pin}: {e:?}")))?;

        // W5500 RST (输出, 启动时拉低 50ms 复位)
        let eth_rst = PinDriver::output(unsafe { AnyOutputPin::steal(eth_rst_pin) })
            .map_err(|e| AppError::Hal(format!("eth_rst gpio{eth_rst_pin}: {e:?}")))?;
        {
            let mut pin = eth_rst;
            let _ = pin.set_low();
            std::thread::sleep(Duration::from_millis(50));
            let _ = pin.set_high();
        }

        // RS485 #0/#1 DE (输出, 默认低 = 接收模式)
        let rs485_de = [
            Spin::new(
                PinDriver::output(unsafe { AnyOutputPin::steal(rs485_de_pins[0]) }).map_err(
                    |e| AppError::Hal(format!("rs485_de[0] gpio{}: {e:?}", rs485_de_pins[0])),
                )?,
            ),
            Spin::new(
                PinDriver::output(unsafe { AnyOutputPin::steal(rs485_de_pins[1]) }).map_err(
                    |e| AppError::Hal(format!("rs485_de[1] gpio{}: {e:?}", rs485_de_pins[1])),
                )?,
            ),
        ];
        for pin in rs485_de.iter() {
            let _ = pin.lock().set_level(Level::Low);
        }

        // 板载运行 LED (无引脚时为 None, heartbeat 跳过)
        let run_led = Spin::new(match run_led_pin {
            Some(p) => Some(
                PinDriver::output(unsafe { AnyOutputPin::steal(p) })
                    .map_err(|e| AppError::Hal(format!("run_led gpio{p}: {e:?}")))?,
            ),
            None => None,
        });

        Ok(Self {
            eth_int,
            eth_rst: Spin::new(
                PinDriver::output(unsafe { AnyOutputPin::steal(eth_rst_pin) })
                    .map_err(|e| AppError::Hal(format!("eth_rst reinit: {e:?}")))?,
            ),
            rs485_de,
            run_led,
        })
    }

    /// 切换 RS485 方向 (true=发送, false=接收)
    pub fn rs485_set_tx(&self, idx: usize, tx: bool) {
        if idx >= self.rs485_de.len() {
            log::warn!("[gpio] rs485_set_tx idx {idx} out of range");
            return;
        }
        let level = if tx { Level::High } else { Level::Low };
        if let Err(e) = self.rs485_de[idx].lock().set_level(level) {
            log::warn!("[gpio] rs485_de[{idx}] set_level: {e:?}");
        }
    }

    /// W5500 硬件复位 (低脉冲 50ms)
    pub fn eth_reset_pulse(&self) {
        if let Some(mut pin) = self.eth_rst.try_lock() {
            let _ = pin.set_low();
            std::thread::sleep(Duration::from_millis(50));
            let _ = pin.set_high();
        }
    }

    /// 切换运行 LED (用于 heartbeat, 占位实现)
    pub fn toggle_run_led(&self) {
        let mut guard = self.run_led.lock();
        if let Some(pin) = guard.as_mut() {
            let cur = pin.is_set_high();
            let _ = pin.set_level((!cur).into());
        }
    }
}
