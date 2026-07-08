//! I2C 总线封装 (ESP32-S3 I2C0, 用于 MCP23017 IO 扩展)
//!
//! 仅 F3/F4 版本启用, 提供 I2cDriver 共享句柄。
//! 默认版本不创建 I2C 总线 (DI/DO 走 GPIO 直驱)。

use esp_idf_hal::gpio::AnyPin;
use esp_idf_hal::i2c::{I2cConfig, I2cDriver, I2CVendor};
use esp_idf_hal::peripheral::Peripheral;

use crate::config::pins;
use crate::error::{AppError, AppResult};

/// I2C 总线句柄 (共享给所有 MCP23017 芯片)
pub struct I2cBus {
    driver: I2cDriver<'static>,
}

impl I2cBus {
    /// 初始化 I2C 总线
    ///
    /// 注: SDA/SCL 引脚在 F3/F4 版本下从原 DI 引脚释放 (GPIO21/33)。
    /// unsafe: 调用者需确保 GPIO 未被其他驱动占用。
    pub fn init(
        i2c: impl Peripheral<P = I2CVendor> + 'static,
        sda_num: u8,
        scl_num: u8,
    ) -> AppResult<Self> {
        let sda = unsafe { AnyPin::new(sda_num as i32) };
        let scl = unsafe { AnyPin::new(scl_num as i32) };
        let cfg = I2cConfig::new().baudrate(pins::I2C_FREQ_HZ.into());
        let driver = I2cDriver::new(i2c, sda, scl, &cfg)
            .map_err(|e| AppError::Hal(format!("i2c init: {e:?}")))?;
        log::info!(
            "[hal] I2C{} initialized: SDA={}, SCL={}, freq={}Hz",
            pins::I2C_PORT, pins::I2C_SDA, pins::I2C_SCL, pins::I2C_FREQ_HZ
        );
        Ok(Self { driver })
    }

    /// 写寄存器: `[addr, reg, data...]`
    #[inline]
    pub fn write_reg(&mut self, dev_addr: u8, reg: u8, data: &[u8]) -> AppResult<()> {
        let mut buf: heapless::Vec<u8, 16> = heapless::Vec::new();
        let _ = buf.push(reg);
        let _ = buf.extend_from_slice(data);
        self.driver
            .write(dev_addr, &buf, esp_idf_hal::i2c::I2cConfig::new())
            .map_err(|e| AppError::Hal(format!("i2c write 0x{:02X}: {e:?}", dev_addr)))?;
        Ok(())
    }

    /// 写单字节寄存器
    #[inline]
    pub fn write_reg_byte(&mut self, dev_addr: u8, reg: u8, value: u8) -> AppResult<()> {
        self.write_reg(dev_addr, reg, &[value])
    }

    /// 读寄存器: 先写 reg, 再读 N 字节
    #[inline]
    pub fn read_reg(&mut self, dev_addr: u8, reg: u8, buf: &mut [u8]) -> AppResult<()> {
        self.driver
            .write_read(dev_addr, &[reg], buf, esp_idf_hal::i2c::I2cConfig::new())
            .map_err(|e| AppError::Hal(format!("i2c read 0x{:02X}: {e:?}", dev_addr)))?;
        Ok(())
    }

    /// 读单字节寄存器
    #[inline]
    pub fn read_reg_byte(&mut self, dev_addr: u8, reg: u8) -> AppResult<u8> {
        let mut buf = [0u8; 1];
        self.read_reg(dev_addr, reg, &mut buf)?;
        Ok(buf[0])
    }
}
