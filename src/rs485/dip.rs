//! RS485 拨码开关地址读取
//!
//! 对齐参考固件 MCA_F16V2_1_F48_BLE.ino:1997-2021:
//!   RS485_AD0=digitalRead(19), AD1=20, AD2=48, AD3=47
//!   RS485_ADDRESS = (AD3<<3)|(AD2<<2)|(AD1<<1)|AD0
//!
//! 拨码 > 0 时: RS485-1 强制从站模式, 地址 = 拨码值 (1-15), 9600 8N1.
//! 拨码 = 0 时: 使用 SystemConfig.rs485[0] 寄存器配置.
//!
//! 实现: 使用 ESP-IDF 原生 GPIO API (gpio_set_direction + gpio_get_level),
//! 不依赖 esp-idf-hal PinDriver 所有权, 可在 Hal::init 之后随时调用.

use crate::config::pins;

/// 拨码开关读取结果
#[derive(Clone, Copy, Debug, Default)]
pub struct DipAddress {
    /// 4-bit 拨码值 (0-15). 0 = 未拨码 (使用配置), 1-15 = 强制从站地址
    pub address: u8,
    /// ESP_STOP 引脚电平 (参考固件读取但未使用, 此处保留用于诊断)
    pub esp_stop: bool,
}

/// 从硬件读取拨码开关 (使用 ESP-IDF 原生 GPIO API).
///
/// 可在 Hal::init 之后随时调用, 不消费 Peripherals 句柄.
pub fn read_dip_address() -> DipAddress {
    // 配置 5 个 GPIO 为输入模式 (gpio_set_direction 是幂等的, 多次调用安全)
    for &pin in &pins::RS485_ADDR_PINS {
        unsafe {
            esp_idf_sys::gpio_set_direction(pin as esp_idf_sys::gpio_num_t, esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT);
            esp_idf_sys::gpio_set_pull_mode(pin as esp_idf_sys::gpio_num_t, esp_idf_sys::gpio_pull_mode_t_GPIO_FLOATING);
        }
    }
    unsafe {
        esp_idf_sys::gpio_set_direction(pins::ESP_STOP_PIN as esp_idf_sys::gpio_num_t, esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT);
        esp_idf_sys::gpio_set_pull_mode(pins::ESP_STOP_PIN as esp_idf_sys::gpio_num_t, esp_idf_sys::gpio_pull_mode_t_GPIO_FLOATING);
    }

    // 读取电平
    let ad0 = unsafe { esp_idf_sys::gpio_get_level(pins::RS485_ADDR_PINS[0] as esp_idf_sys::gpio_num_t) } as u8;
    let ad1 = unsafe { esp_idf_sys::gpio_get_level(pins::RS485_ADDR_PINS[1] as esp_idf_sys::gpio_num_t) } as u8;
    let ad2 = unsafe { esp_idf_sys::gpio_get_level(pins::RS485_ADDR_PINS[2] as esp_idf_sys::gpio_num_t) } as u8;
    let ad3 = unsafe { esp_idf_sys::gpio_get_level(pins::RS485_ADDR_PINS[3] as esp_idf_sys::gpio_num_t) } as u8;
    let esp_stop = unsafe { esp_idf_sys::gpio_get_level(pins::ESP_STOP_PIN as esp_idf_sys::gpio_num_t) } != 0;

    let address = (ad3 << 3) | (ad2 << 2) | (ad1 << 1) | ad0;
    log::info!(
        "[dip] AD0={} AD1={} AD2={} AD3={} → address={} (gpio {}/{}/{}/{})",
        ad0, ad1, ad2, ad3, address,
        pins::RS485_ADDR_PINS[0], pins::RS485_ADDR_PINS[1],
        pins::RS485_ADDR_PINS[2], pins::RS485_ADDR_PINS[3]
    );
    DipAddress { address, esp_stop }
}

/// 应用 DIP 拨码结果到 SystemConfig.rs485[0]
///
/// 当拨码值 > 0: 强制从站模式, 地址 = 拨码值, 9600 8N1, 覆盖寄存器配置.
/// 当拨码值 == 0: 不修改, 保留寄存器配置.
pub fn apply_to_config(dip: DipAddress, cfg: &mut crate::device::system_config::SystemConfig) {
    if dip.address == 0 {
        return;
    }
    let port0 = &mut cfg.rs485[0];
    port0.mode = 1; // Slave
    port0.slave_addr = dip.address;
    port0.baudrate = 9600;
    port0.data_bits = 8;
    port0.stop_bits = 1;
    port0.parity = 0;
    log::warn!(
        "[dip] override RS485-1: address={} mode=Slave baud=9600 8N1 (DIP switch dominant)",
        dip.address
    );
}
