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
            #[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
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
            #[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
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
/// 供外部调用者直接构造异常帧时使用。
pub fn build_exception_response(func: u8, code: u8) -> [u8; 3] {
    [func | 0x80, code, 0] // 最后一个字节由调用者补 CRC
}

/// 统一的 Modbus PDU 处理入口。
/// 输入 `func` (功能码) + `pdu` (去除功能码后的 PDU 载荷),
/// 返回待拼入响应的 body (不含功能码, 也不含 CRC 或 MBAP 头)。
/// 失败时返回 [0x80 | func, code] 的异常响应体。
/// 统一的 Modbus PDU 处理入口。
/// 输入 `func` (功能码) + `pdu` (去除功能码后的 PDU 载荷),
/// 返回完整的 Modbus PDU ([func, body...] 或异常 [func|0x80, code])。
/// 统一的 Modbus PDU 处理入口。
/// 输入 `func` (功能码) + `pdu` (去除功能码后的 PDU 载荷),
/// 返回完整的 Modbus PDU ([func, body...] 或 [func|0x80, code])。
/// 内部函数返回的 Vec 第一个字节可能是 exc code（len=1 < 0x80），
/// 此时 handle_pdu 会将其转换为完整异常 PDU。
pub fn handle_pdu<B: ModbusBackend>(
    backend: &B,
    func: u8,
    pdu: &[u8],
) -> Vec<u8> {
    let body = match func {
        0x01 => read_bits_pdu(backend, pdu, |b, a, c| b.read_coils(a, c)),
        0x02 => read_bits_pdu(backend, pdu, |b, a, c| b.read_discrete_inputs(a, c)),
        0x03 => read_regs_pdu(backend, pdu, |b, a, c| b.read_holding_registers(a, c)),
        0x04 => read_regs_pdu(backend, pdu, |b, a, c| b.read_input_registers(a, c)),
        0x05 => write_single_coil_pdu(backend, pdu),
        0x06 => write_single_reg_pdu(backend, pdu),
        0x0F => write_multi_coils_pdu(backend, pdu),
        0x10 => write_multi_regs_pdu(backend, pdu),
        _ => return exception_pdu(func, exc::ILLEGAL_FUNCTION),
    };
    // 异常检测: 仅当 body 长度为 1 且是合法异常码 (1-4) 时才视为异常
    // 正常响应的 body 至少 2 字节: [byte_count, ...data]
    if body.len() == 1 && (1..=4).contains(&body[0]) {
        return exception_pdu(func, body[0]);
    }
    let mut out = Vec::with_capacity(1 + body.len());
    out.push(func);
    out.extend_from_slice(&body);
    out
}

/// 返回 [func|0x80, code], 供内部 handle_pdu 直接返回。
fn exception_pdu(func: u8, code: u8) -> Vec<u8> {
    vec![func | 0x80, code]
}

fn read_bits_pdu<B, F>(backend: &B, pdu: &[u8], f: F) -> Vec<u8>
where
    B: ModbusBackend,
    F: Fn(&B, u16, u16) -> Vec<bool>,
{
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];  // error body, handle_pdu will wrap
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 2000 {
        return vec![exc::ILLEGAL_DATA_VALUE];
    }
    let bits = f(backend, addr, count);
    let byte_count = ((count as usize) + 7) / 8;
    let mut out = Vec::with_capacity(1 + byte_count);
    out.push(byte_count as u8);
    let mut bytes = vec![0u8; byte_count];
    for (i, b) in bits.iter().enumerate() {
        if *b {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    out.extend_from_slice(&bytes);
    out
}

fn read_regs_pdu<B, F>(backend: &B, pdu: &[u8], f: F) -> Vec<u8>
where
    B: ModbusBackend,
    F: Fn(&B, u16, u16) -> Vec<u16>,
{
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 125 {
        return vec![exc::ILLEGAL_DATA_VALUE];
    }
    let regs = f(backend, addr, count);
    let mut out = Vec::with_capacity(1 + regs.len() * 2);
    out.push((regs.len() * 2) as u8);
    for r in regs {
        out.extend_from_slice(&r.to_be_bytes());
    }
    out
}

fn write_single_coil_pdu<B: ModbusBackend>(backend: &B, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    let on = value == 0xFF00;
    if !on && value != 0 {
        return vec![exc::ILLEGAL_DATA_VALUE];
    }
    if !backend.write_single_coil(addr, on) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    vec![pdu[0], pdu[1], pdu[2], pdu[3]]
}

fn write_single_reg_pdu<B: ModbusBackend>(backend: &B, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 4 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    if !backend.write_single_register(addr, value) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    vec![pdu[0], pdu[1], pdu[2], pdu[3]]
}

fn write_multi_coils_pdu<B: ModbusBackend>(backend: &B, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 5 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    if pdu.len() < 5 + byte_count {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let mut bits = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let byte = pdu[5 + i / 8];
        bits.push(byte & (1 << (i % 8)) != 0);
    }
    if !backend.write_multiple_coils(addr, &bits) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    vec![pdu[0], pdu[1], pdu[2], pdu[3]]
}

fn write_multi_regs_pdu<B: ModbusBackend>(backend: &B, pdu: &[u8]) -> Vec<u8> {
    if pdu.len() < 5 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    if pdu.len() < 5 + byte_count || byte_count != count as usize * 2 {
        return vec![exc::SLAVE_DEVICE_FAILURE];
    }
    let mut regs = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        regs.push(u16::from_be_bytes([pdu[5 + 2 * i], pdu[6 + 2 * i]]));
    }
    if !backend.write_multiple_registers(addr, &regs) {
        return vec![exc::ILLEGAL_DATA_ADDRESS];
    }
    vec![pdu[0], pdu[1], pdu[2], pdu[3]]
}

/// 构造异常响应 body: [0x80|func, code]
fn build_exception_body(func: u8, code: u8) -> Vec<u8> {
    vec![func | 0x80, code]
}

// ============================================================================
// 单元测试 (#[cfg(test)] 在 ESP-IDF 项目中无法运行, 仅为代码完整性保留)
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc16_known_vectors() {
        // Modbus 官方测试向量 (Modbus RTU spec)
        // 0x01 0x03 0x00 0x00 0x00 0x0A → CRC = 0xC5CD
        assert_eq!(modbus_crc16(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]), 0xC5CD);
        // 0x01 0x04 0x02 0xFF 0xFF → CRC = 0xB880
        assert_eq!(modbus_crc16(&[0x01, 0x04, 0x02, 0xFF, 0xFF]), 0xB880);
        // 0x01 0x06 0x00 0x01 0x00 0x03 → CRC = 0xD9AA
        assert_eq!(modbus_crc16(&[0x01, 0x06, 0x00, 0x01, 0x00, 0x03]), 0xD9AA);
    }

    #[test]
    fn test_crc16_empty() {
        // CRC16 of empty data should be 0xFFFF (initial value)
        assert_eq!(modbus_crc16(&[]), 0xFFFF);
    }

    #[test]
    fn test_crc16_single_byte() {
        // CRC16 of single byte 0x01 = 0x807E
        assert_eq!(modbus_crc16(&[0x01]), 0x807E);
    }

    #[test]
    fn test_exception_response() {
        let exc = build_exception_response(0x06, exc::ILLEGAL_DATA_ADDRESS);
        assert_eq!(exc[0], 0x86); // func | 0x80
        assert_eq!(exc[1], exc::ILLEGAL_DATA_ADDRESS);
    }

    /// Dummy backend for testing
    struct DummyBackend {
        coils: [bool; 256],
        disc: [bool; 256],
        holding: [u16; 256],
        input_reg: [u16; 256],
    }
    impl DummyBackend {
        fn new() -> Self {
            Self {
                coils: [false; 256],
                disc: [false; 256],
                holding: [0; 256],
                input_reg: [0; 256],
            }
        }
    }
    impl ModbusBackend for DummyBackend {
        fn read_coils(&self, addr: u16, count: u16) -> Vec<bool> {
            (0..count).map(|i| self.coils[(addr + i) as usize]).collect()
        }
        fn read_discrete_inputs(&self, addr: u16, count: u16) -> Vec<bool> {
            (0..count).map(|i| self.disc[(addr + i) as usize]).collect()
        }
        fn read_holding_registers(&self, addr: u16, count: u16) -> Vec<u16> {
            (0..count).map(|i| self.holding[(addr + i) as usize]).collect()
        }
        fn read_input_registers(&self, addr: u16, count: u16) -> Vec<u16> {
            (0..count).map(|i| self.input_reg[(addr + i) as usize]).collect()
        }
        fn write_single_coil(&self, _addr: u16, _value: bool) -> bool { true }
        fn write_single_register(&self, _addr: u16, _value: u16) -> bool { true }
        fn write_multiple_coils(&self, _addr: u16, _values: &[bool]) -> bool { true }
        fn write_multiple_registers(&self, _addr: u16, _values: &[u16]) -> bool { true }
    }

    #[test]
    fn test_read_holding_registers_pdu() {
        let backend = DummyBackend::new();
        let mut b = backend;
        b.holding[0] = 0x1234;
        b.holding[1] = 0x5678;
        // FC=03: read_holding_registers, start=0, count=2
        let pdu = [0x00, 0x00, 0x00, 0x02];
        let body = handle_pdu(&b, 0x03, &pdu);
        assert_eq!(body[0], 0x03); // func
        assert_eq!(body[1], 0x04); // byte count = 2 * 2
        assert_eq!(&body[2..6], &[0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn test_write_single_register_pdu() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x05, 0x00, 0xCA];
        let body = handle_pdu(&backend, 0x06, &pdu);
        // Echo response: addr(2) + value(2)
        assert_eq!(&body[..], &pdu);
    }

    #[test]
    fn test_read_coils_pdu() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x0A]; // 10 coils
        let body = handle_pdu(&backend, 0x01, &pdu);
        assert_eq!(body[0], 0x01); // func
        assert_eq!(body[1], 0x02); // byte count (10 bits → 2 bytes)
        assert_eq!(body.len(), 4); // func + byte_count + 2 bytes
    }

    #[test]
    fn test_read_discrete_inputs_exception() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x00]; // count=0 invalid
        let body = handle_pdu(&backend, 0x02, &pdu);
        // Exception: [0x82, ILLEGAL_DATA_VALUE]
        assert_eq!(body[0], 0x82);
        assert_eq!(body[1], exc::ILLEGAL_DATA_VALUE);
    }

    #[test]
    fn test_write_multiple_coils_pdu() {
        let backend = DummyBackend::new();
        // FC=0x0F: write 10 coils starting at 0
        // pdu: start_addr(2) + count(2) + byte_count(1) + data(2) = 7
        let pdu = [0x00, 0x00, 0x00, 0x0A, 0x02, 0xFF, 0x03]; // bits 0-9 = 1,1,1,1,1,1,1,1,0,1
        let body = handle_pdu(&backend, 0x0F, &pdu);
        // Echo: addr(2) + count(2) = 4 bytes after func
        assert_eq!(body[0], 0x0F);
        assert_eq!(&body[1..5], &[0x00, 0x00, 0x00, 0x0A]);
    }

    #[test]
    fn test_illegal_function_exception() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00];
        let body = handle_pdu(&backend, 0x07, &pdu); // FC=07 not supported
        assert_eq!(body[0], 0x87); // func | 0x80
        assert_eq!(body[1], exc::ILLEGAL_FUNCTION);
    }

    #[test]
    fn test_illegal_data_value() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x00]; // count=0 for FC=03
        let body = handle_pdu(&backend, 0x03, &pdu);
        assert_eq!(body[0], 0x83);
        assert_eq!(body[1], exc::ILLEGAL_DATA_VALUE);
    }

    #[test]
    fn test_count_too_large() {
        let backend = DummyBackend::new();
        // FC=03 max is 125 regs
        let pdu = [0x00, 0x00, 0x00, 0x80]; // 128 regs
        let body = handle_pdu(&backend, 0x03, &pdu);
        assert_eq!(body[0], 0x83);
        assert_eq!(body[1], exc::ILLEGAL_DATA_VALUE);
    }
}
