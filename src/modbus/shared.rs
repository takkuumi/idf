//! Modbus 共享工具与总线后端
//!
//! - `ModbusBackend` trait: 抽象所有 Modbus 寄存器访问
//! - `BusBackend`: 实现 `ModbusBackend`, 全部代理到 `crate::bus::BUS`
//! - `modbus_crc16`: 标准 Modbus CRC-16 (0xA001 polynomial)

use crate::bus;

/// Modbus 寄存器访问后端
pub trait ModbusBackend {
    fn read_coils(&self, addr: u16, count: u16) -> Vec<bool>;
    fn read_discrete_inputs(&self, addr: u16, count: u16) -> Vec<bool>;
    fn read_holding_registers(&self, addr: u16, count: u16) -> Vec<u16>;
    fn read_input_registers(&self, addr: u16, count: u16) -> Vec<u16>;
    fn write_single_coil(&self, addr: u16, value: bool) -> bool;
    fn write_single_register(&self, addr: u16, value: u16) -> bool;
    fn write_multiple_coils(&self, addr: u16, values: &[bool]) -> bool;
    fn write_multiple_registers(&self, addr: u16, values: &[u16]) -> bool;
}

/// 总线后端, 全部代理到 `bus::BUS`
pub struct BusBackend;

impl ModbusBackend for BusBackend {
    fn read_coils(&self, addr: u16, count: u16) -> Vec<bool> {
        let mut v = Vec::with_capacity(count as usize);
        if let Some(b) = bus::lock_timeout() {
            for i in 0..count {
                v.push(b.read_coil(addr + i).unwrap_or(false));
            }
        }
        v
    }

    fn read_discrete_inputs(&self, addr: u16, count: u16) -> Vec<bool> {
        let mut v = Vec::with_capacity(count as usize);
        if let Some(b) = bus::lock_timeout() {
            for i in 0..count {
                v.push(b.read_disc(addr + i).unwrap_or(false));
            }
        }
        v
    }

    fn read_holding_registers(&self, addr: u16, count: u16) -> Vec<u16> {
        let mut v = Vec::with_capacity(count as usize);
        if let Some(b) = bus::lock_timeout() {
            for i in 0..count {
                v.push(b.read_hold_reg(addr + i).unwrap_or(0));
            }
        }
        v
    }

    fn read_input_registers(&self, addr: u16, count: u16) -> Vec<u16> {
        let mut v = Vec::with_capacity(count as usize);
        if let Some(b) = bus::lock_timeout() {
            for i in 0..count {
                v.push(b.read_input_reg(addr + i).unwrap_or(0));
            }
        }
        v
    }

    fn write_single_coil(&self, addr: u16, value: bool) -> bool {
        if let Some(mut b) = bus::lock_timeout() {
            let ok = b.write_coil(addr, value);
            #[cfg(any(feature_io_di_do, feature_f3, feature_f4))]
            if ok { crate::io::do_::notify(); }
            ok
        } else {
            false
        }
    }

    fn write_single_register(&self, addr: u16, value: u16) -> bool {
        if let Some(mut b) = bus::lock_timeout() {
            b.write_hold_reg(addr, value)
        } else {
            false
        }
    }

    fn write_multiple_coils(&self, addr: u16, values: &[bool]) -> bool {
        if let Some(mut b) = bus::lock_timeout() {
            let mut ok = true;
            for (i, &v) in values.iter().enumerate() {
                if !b.write_coil(addr + i as u16, v) {
                    ok = false;
                }
            }
            #[cfg(any(feature_io_di_do, feature_f3, feature_f4))]
            if ok { crate::io::do_::notify(); }
            ok
        } else {
            false
        }
    }

    fn write_multiple_registers(&self, addr: u16, values: &[u16]) -> bool {
        if let Some(mut b) = bus::lock_timeout() {
            for (i, &v) in values.iter().enumerate() {
                if !b.write_hold_reg(addr + i as u16, v) {
                    return false;
                }
            }
            true
        } else {
            false
        }
    }
}

/// 标准 Modbus CRC-16 (polynomial 0xA001, init 0xFFFF, LSB first)
#[inline]
pub fn modbus_crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc >>= 1;
                crc ^= 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// Modbus 异常码
pub mod exc {
    pub const ILLEGAL_FUNCTION: u8 = 0x01;
    pub const ILLEGAL_DATA_ADDRESS: u8 = 0x02;
    pub const ILLEGAL_DATA_VALUE: u8 = 0x03;
    pub const SLAVE_DEVICE_FAILURE: u8 = 0x04;
    pub const ACKNOWLEDGE: u8 = 0x05;
    pub const SLAVE_DEVICE_BUSY: u8 = 0x06;
}

/// 构造异常响应
pub fn build_exception_response(func: u8, code: u8) -> [u8; 3] {
    [func | 0x80, code, 0] // 最后一个字节由调用者补 CRC
}
