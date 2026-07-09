//! GPIO 引脚集合 (DI / DO / ETH / RS485_DE)
//!
//! 集中管理所有非 SPI/UART/ADC/LEDC 的 GPIO 资源：
//! - 8 路 DI 数字输入 (光耦隔离，Pull Down) — 仅默认版本
//! - 8 路 DO 数字输出 (OC 输出) — 仅默认版本
//! - ETH_INT 输入 (W5500 中断，低有效)
//! - ETH_RST 输出 (W5500 复位，低有效)
//! - 2 路 RS485 DE 引脚 (高=发送，低=接收)
//!
//! # 版本说明
//!
//! - 默认版本 (8 DI + 8 DO): GpioBank 包含 di/do_ 字段, GPIO 直驱
//! - F3/F4 版本: GpioBank 不包含 di/do_ (用 `IoExtender` 经 MCP23017 扩展),
//!   释放的 GPIO21/33 给 I2C 总线使用
//!
//! # 线程安全
//!
//! 输入引脚 `is_high`/`is_level` 仅需 `&self`，无需互斥。
//! 输出引脚 `set_level` 需 `&mut self`，通过 `parking_lot::Mutex` 串行化。

use std::time::Duration;

use parking_lot::Mutex;

use esp_idf_hal::gpio::{AnyInputPin, AnyOutputPin, Input, Output, PinDriver, Pull};

use crate::error::{AppError, AppResult};

/// GPIO 引脚集合
pub struct GpioBank {
    /// W5500 中断输入 (低有效)
    eth_int: PinDriver<'static, Input>,
    /// W5500 复位输出 (低脉冲)
    eth_rst: Mutex<PinDriver<'static, Output>>,
    /// RS485 #0 / #1 的 DE 引脚
    rs485_de: [Mutex<PinDriver<'static, Output>>; 2],

    /// 8 路 DI 输入 (仅 io-di-do 版本使用 GPIO 直驱, 否则由 PCA9555/MCP23017 管理)
    di: Option<[PinDriver<'static, Input>; 8]>,
    /// 8 路 DO 输出
    do_: Option<[Mutex<PinDriver<'static, Output>>; 8]>,
    /// DO 输出状态缓存
    do_cache: Mutex<u64>,
}

impl GpioBank {
    /// 初始化 GPIO 集合 (GPIO 直驱 DI/DO — 保留, 需 feature_gpio_di_do)
    #[cfg(feature_gpio_di_do)]
    #[allow(dead_code)]
    pub fn init(
        di_pins: [u8; 8],
        do_pins: [u8; 8],
        eth_int: u8,
        eth_rst: u8,
        rs485_de: [u8; 2],
    ) -> AppResult<Self> {
        // 1. 创建 8 路 DI (Pull Down 默认，光耦隔离输入)
        //    SAFETY: 引脚号来自 config，确保未被其它驱动占用
        let di: [PinDriver<'static, Input>; 8] = [
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[0]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 0: {e:?}")))?,
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[1]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 1: {e:?}")))?,
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[2]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 2: {e:?}")))?,
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[3]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 3: {e:?}")))?,
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[4]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 4: {e:?}")))?,
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[5]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 5: {e:?}")))?,
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[6]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 6: {e:?}")))?,
            PinDriver::input(unsafe { AnyInputPin::steal(di_pins[7]) }, Pull::Down)
                .map_err(|e| AppError::Hal(format!("di 7: {e:?}")))?,
        ];

        // 2. 创建 8 路 DO 输出 (默认低电平)
        //    SAFETY: 引脚号来自 config
        let do_: [Mutex<PinDriver<'static, Output>>; 8] = [
            Mutex::new(make_output(do_pins[0])?),
            Mutex::new(make_output(do_pins[1])?),
            Mutex::new(make_output(do_pins[2])?),
            Mutex::new(make_output(do_pins[3])?),
            Mutex::new(make_output(do_pins[4])?),
            Mutex::new(make_output(do_pins[5])?),
            Mutex::new(make_output(do_pins[6])?),
            Mutex::new(make_output(do_pins[7])?),
        ];

        // 3. ETH_INT + ETH_RST + RS485_DE
        let (eth_int_pin, eth_rst_pin, rs485_de_pins) = init_eth_and_rs485(eth_int, eth_rst, rs485_de)?;

        Ok(Self {
            eth_int: eth_int_pin,
            eth_rst: eth_rst_pin,
            rs485_de: rs485_de_pins,
            di: Some(di),
            do_: Some(do_),
            do_cache: Mutex::new(0),
        })
    }

    /// 初始化 GPIO 集合 (无 DI/DO: DI/DO 由 IoExtender/PCA9555 管理)
    pub fn init(
        eth_int: u8,
        eth_rst: u8,
        rs485_de: [u8; 2],
    ) -> AppResult<Self> {
        let (eth_int_pin, eth_rst_pin, rs485_de_pins) = init_eth_and_rs485(eth_int, eth_rst, rs485_de)?;
        Ok(Self {
            eth_int: eth_int_pin,
            eth_rst: eth_rst_pin,
            rs485_de: rs485_de_pins,
            di: None,
            do_: None,
            do_cache: Mutex::new(0),
        })
    }

    /// 读 DI idx (0..8)。仅 gpio_di_do 版本可用
    #[cfg(feature_gpio_di_do)]
    pub fn di_read(&self, idx: u8) -> bool {
        if idx >= 8 {
            log::warn!("[gpio] di_read idx out of range: {idx}");
            return false;
        }
        self.di[idx as usize].is_high()
    }

    /// 写 DO idx (0..8)。仅 gpio_di_do 版本可用
    #[cfg(feature_gpio_di_do)]
    pub fn do_write(&self, idx: u8, on: bool) {
        if idx >= 8 {
            log::warn!("[gpio] do_write idx out of range: {idx}");
            return;
        }
        let mut pin = self.do_[idx as usize].lock();
        if let Err(e) = pin.set_level(on.into()) {
            log::warn!("[gpio] do_write {idx} failed: {e:?}");
            return;
        }
        // 同步更新缓存
        let mut cache = self.do_cache.lock();
        if on {
            *cache |= 1u64 << idx;
        } else {
            *cache &= !(1u64 << idx);
        }
    }

    /// 复位 W5500: HIGH(250ms) → LOW(50ms) → HIGH(350ms)
    /// 序列来自参考固件 ETHClass.cpp beginSPI()
    pub fn eth_reset(&self) {
        let mut pin = self.eth_rst.lock();
        // 1. 先拉高确保不在复位状态
        if let Err(e) = pin.set_high() {
            log::warn!("[gpio] eth_reset set_high(initial) failed: {e:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
        // 2. 拉低触发复位
        if let Err(e) = pin.set_low() {
            log::warn!("[gpio] eth_reset set_low failed: {e:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
        // 3. 释放复位 (拉高)
        if let Err(e) = pin.set_high() {
            log::warn!("[gpio] eth_reset set_high(release) failed: {e:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(350));
        log::info!("[gpio] W5500 reset pulse applied (250ms H → 50ms L → 350ms H)");
    }

    /// 读 ETH_INT 电平。true = 高 (无中断)，false = 低 (中断触发)。
    pub fn eth_int_level(&self) -> bool {
        self.eth_int.is_high()
    }

    /// 控制 RS485 #{port} (0/1) 的 DE 引脚。
    pub fn rs485_set_de(&self, port: u8, on: bool) {
        if port >= 2 {
            log::warn!("[gpio] rs485_set_de port out of range: {port}");
            return;
        }
        let mut pin = self.rs485_de[port as usize].lock();
        if let Err(e) = pin.set_level(on.into()) {
            log::warn!("[gpio] rs485_set_de {port} failed: {e:?}");
        }
    }
}

/// 创建 ETH_INT / ETH_RST / RS485_DE 三类公共引脚 (所有版本共用)
fn init_eth_and_rs485(
    eth_int: u8,
    eth_rst: u8,
    rs485_de: [u8; 2],
) -> AppResult<(
    PinDriver<'static, Input>,
    Mutex<PinDriver<'static, Output>>,
    [Mutex<PinDriver<'static, Output>>; 2],
)> {
    let eth_int_pin = PinDriver::input(unsafe { AnyInputPin::steal(eth_int) }, Pull::Up)
        .map_err(|e| AppError::Hal(format!("eth_int: {e:?}")))?;
    let eth_rst_pin = Mutex::new(make_output(eth_rst)?);
    let rs485_de_pins: [Mutex<PinDriver<'static, Output>>; 2] = [
        Mutex::new(make_output(rs485_de[0])?),
        Mutex::new(make_output(rs485_de[1])?),
    ];
    Ok((eth_int_pin, eth_rst_pin, rs485_de_pins))
}

/// 构造一个 AnyOutputPin 的 PinDriver<Output>，初始电平为低。
fn make_output(gpio: u8) -> AppResult<PinDriver<'static, Output>> {
    // SAFETY: 引脚号来自 config，确保未被其它驱动占用
    let pin = unsafe { AnyOutputPin::steal(gpio) };
    PinDriver::output(pin)
        .map_err(|e| AppError::Hal(format!("gpio output {gpio}: {e:?}")))
}

// ============================================================================
// DigitalIo trait 实现 (默认版本, GPIO 直驱)
// ============================================================================
// 上层通过 `hal.dio() -> &dyn DigitalIo` 统一访问 DI/DO, 屏蔽 GPIO/I2C 差异。
// 仅默认版本 (8 DI + 8 DO) 为 GpioBank 实现此 trait;
// F3/F4 版本由 IoExtender 实现, GpioBank 不含 DI/DO 字段。
#[cfg(feature_gpio_di_do)]
impl crate::hal::digital_io::DigitalIo for GpioBank {
    fn di_count(&self) -> usize {
        8
    }

    fn do_count(&self) -> usize {
        8
    }

    /// 逐通道读取 8 路 DI, 拼接为 u64 (bit i = DI i)
    ///
    /// GPIO 读取无通信延迟, <1μs
    fn read_di_all(&self) -> AppResult<u64> {
        let mut bits: u64 = 0;
        for idx in 0..8 {
            if self.di[idx].is_high() {
                bits |= 1u64 << idx;
            }
        }
        Ok(bits)
    }

    /// 读取单路 DI
    fn read_di(&self, idx: usize) -> AppResult<bool> {
        if idx >= 8 {
            return Err(AppError::Io(format!("DI idx {} out of range", idx)));
        }
        Ok(self.di[idx].is_high())
    }

    /// 逐通道写入 8 路 DO
    ///
    /// GPIO 写入无通信延迟, 8 次写 <10μs
    fn write_do_all(&self, value: u64) -> AppResult<()> {
        for idx in 0..8 {
            let on = value & (1u64 << idx) != 0;
            let mut pin = self.do_[idx].lock();
            if let Err(e) = pin.set_level(on.into()) {
                log::warn!("[gpio] write_do_all idx {} failed: {:?}", idx, e);
            }
        }
        // 更新缓存
        *self.do_cache.lock() = value;
        Ok(())
    }

    /// 写入单路 DO
    fn write_do(&self, idx: usize, on: bool) -> AppResult<()> {
        if idx >= 8 {
            return Err(AppError::Io(format!("DO idx {} out of range", idx)));
        }
        let mut pin = self.do_[idx].lock();
        pin.set_level(on.into())
            .map_err(|e| AppError::Hal(format!("DO {} set_level: {:?}", idx, e)))?;
        // 更新缓存
        let mut cache = self.do_cache.lock();
        if on {
            *cache |= 1u64 << idx;
        } else {
            *cache &= !(1u64 << idx);
        }
        Ok(())
    }

    /// 读取 DO 输出缓存 (PinDriver<Output> 不支持 is_high 读回, 用缓存)
    ///
    /// 缓存读取 <1μs, 记录的是上次写入的目标电平 (非实际硬件读回)。
    fn read_do_cached(&self) -> u64 {
        *self.do_cache.lock()
    }

    /// 读取 DO 实际硬件状态 (同 read_do_cached, GPIO 读回不支持 Output 模式)
    fn read_do_actual(&self) -> AppResult<u64> {
        Ok(self.read_do_cached())
    }
}
