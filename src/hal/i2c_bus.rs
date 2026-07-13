//! I2C 总线封装 (用于 MCP23017 IO 扩展, 仅 F3/F4 版本)

use esp_idf_hal::gpio::AnyIOPin;
use esp_idf_hal::i2c::{I2cConfig, I2cDriver};

use crate::config::pins;
use crate::error::{AppError, AppResult};

pub struct I2cBus {
    driver: I2cDriver<'static>,
}

impl I2cBus {
    pub fn init(i2c: impl esp_idf_hal::i2c::I2c + 'static, sda_num: u8, scl_num: u8) -> AppResult<Self> {
        let sda = unsafe { AnyIOPin::steal(sda_num) };
        let scl = unsafe { AnyIOPin::steal(scl_num) };
        let cfg = I2cConfig::new().baudrate(pins::I2C_FREQ_HZ.into());
        let driver = I2cDriver::new(i2c, sda, scl, &cfg)
            .map_err(|e| AppError::Hal(format!("i2c init: {e:?}")))?;
        log::info!(
            "[hal] I2C{}: SDA={}, SCL={}, freq={}Hz",
            pins::I2C_PORT, pins::I2C_SDA, pins::I2C_SCL, pins::I2C_FREQ_HZ
        );
        Ok(Self { driver })
    }

    pub fn write_reg(&mut self, dev_addr: u8, reg: u8, data: &[u8]) -> AppResult<()> {
        let mut buf: heapless::Vec<u8, 16> = heapless::Vec::new();
        let _ = buf.push(reg);
        let _ = buf.extend_from_slice(data);
        self.driver.write(dev_addr, &buf, 1000)
            .map_err(|e| AppError::Hal(format!("i2c write 0x{dev_addr:02X}: {e:?}")))?;
        Ok(())
    }

    pub fn write_reg_byte(&mut self, dev_addr: u8, reg: u8, value: u8) -> AppResult<()> {
        self.write_reg(dev_addr, reg, &[value])
    }

    pub fn read_reg(&mut self, dev_addr: u8, reg: u8, buf: &mut [u8]) -> AppResult<()> {
        self.driver.write_read(dev_addr, &[reg], buf, 1000)
            .map_err(|e| AppError::Hal(format!("i2c read 0x{dev_addr:02X}: {e:?}")))?;
        Ok(())
    }

    pub fn read_reg_byte(&mut self, dev_addr: u8, reg: u8) -> AppResult<u8> {
        let mut buf = [0u8; 1];
        self.read_reg(dev_addr, reg, &mut buf)?;
        Ok(buf[0])
    }
}
