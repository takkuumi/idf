//! Modbus 共享工具与总线后端
//!
//! - `ModbusBackend` trait: 抽象所有 Modbus 寄存器访问
//! - `BusBackend`: 实现 `ModbusBackend`, 全部代理到 `crate::bus::backends::*`
//!   (无锁自由函数: 读 RCU / 写 RCU RMW + atomic, legacy `Spin<Bus>` 已退役)
//! - `modbus_crc16`: 标准 Modbus CRC-16 (0xA001 polynomial)
//!
//! ## 无堆分配设计 (7×24 稳定性)
//!
//! 所有 PDU 处理函数使用固定大小栈缓冲区替代 `Vec<u8>`。
//! 位读/写直接处理压缩位图；寄存器读直接编码到最终 PDU 缓冲区。
//! 这消除了 Modbus 热路径上的所有堆分配，防止长期运行时的 heap 碎片化。

/// Modbus Application Protocol v1.1b3 的 PDU 最大长度。
pub const MODBUS_MAX_PDU_LEN: usize = 253;
/// Modbus TCP MBAP `length` 最大值：Unit ID (1) + PDU (253)。
pub const MODBUS_TCP_MAX_MBAP_LENGTH: usize = 254;
/// Modbus TCP ADU 最大长度：MBAP 前缀 (6) + `length` (254)。
pub const MODBUS_TCP_MAX_ADU_LEN: usize = 260;
/// Modbus 读寄存器最大数量 (FC=03/04 标准限制 125)
pub const MAX_REGS_PER_READ: usize = 125;
/// Modbus FC=16 标准最大写寄存器数 (PDU 253B: 1+2+2+1+123*2)
pub const MAX_REGS_PER_WRITE: usize = 123;
/// Modbus 读线圈最大数量 (FC=01/02)
pub const MAX_BITS_PER_READ: usize = 2000;
/// PDU 输出缓冲区大小，恰好等于 Modbus 标准上限。
pub const PDU_BUF_SIZE: usize = MODBUS_MAX_PDU_LEN;

/// Modbus 寄存器访问后端
///
/// 位读按地址返回，避免 FC=01/02 最大 2,000 位请求在任务栈构造 2KB 临时 Vec。
/// 多位写保留 Modbus 原始 packed bitmap，避免解包成 2,000 个 bool。
pub trait ModbusBackend {
    fn read_coil(&self, addr: u16) -> Option<bool>;
    fn read_discrete_input(&self, addr: u16) -> Option<bool>;
    fn encode_holding_registers(&self, addr: u16, count: u16, out: &mut [u8]) -> bool;
    fn encode_input_registers(&self, addr: u16, count: u16, out: &mut [u8]) -> bool;
    fn write_single_coil(&self, addr: u16, value: bool) -> bool;
    fn write_single_register(&self, addr: u16, value: u16) -> bool;
    fn write_multiple_coils(&self, addr: u16, count: u16, packed_values: &[u8]) -> bool;
    fn write_multiple_registers(&self, addr: u16, values: &[u16]) -> bool;
}

/// 总线后端, 全部代理到 `bus::backends::*` (无锁 RCU + atomic)
pub struct BusBackend;

impl ModbusBackend for BusBackend {
    #[inline]
    fn read_coil(&self, addr: u16) -> Option<bool> {
        crate::bus::backends::read_coil(addr)
    }

    #[inline]
    fn read_discrete_input(&self, addr: u16) -> Option<bool> {
        crate::bus::backends::read_disc(addr)
    }

    fn encode_holding_registers(&self, addr: u16, count: u16, out: &mut [u8]) -> bool {
        crate::bus::backends::encode_hold_regs_be(addr, count, out)
    }

    fn encode_input_registers(&self, addr: u16, count: u16, out: &mut [u8]) -> bool {
        if out.len() != count as usize * 2 {
            return false;
        }
        let last = (addr as u32) + (count as u32) - 1;
        if last > u16::MAX as u32 {
            return false;
        }
        for i in 0..count {
            let Some(value) = crate::bus::backends::read_input_reg(addr.wrapping_add(i)) else {
                return false;
            };
            let offset = i as usize * 2;
            out[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
        }
        true
    }

    fn write_single_coil(&self, addr: u16, value: bool) -> bool {
        // 阶段 B: 写 DO 走无锁 bus::backends::write_coil
        // LOOP13: notify() 已在 backends::write_coil 内部内化, 无需显式调用
        crate::bus::backends::write_coil(addr, value)
    }

    fn write_single_register(&self, addr: u16, value: u16) -> bool {
        // 阶段 B: 写 holding 走无锁 backends::write_hold_reg (RCU RMW)
        crate::bus::backends::write_hold_reg(addr, value)
    }

    fn write_multiple_coils(&self, addr: u16, count: u16, packed_values: &[u8]) -> bool {
        crate::bus::backends::write_coils(addr, count, packed_values)
    }

    fn write_multiple_registers(&self, addr: u16, values: &[u16]) -> bool {
        // 整批共用一次写者锁，避免 PC 配置工具的 60-word 块被其它写者穿插。
        let last = (addr as u32) + (values.len() as u32);
        if last > (u16::MAX as u32) + 1 {
            return false;
        }
        crate::bus::backends::write_hold_regs(addr, values)
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

// ----------------------------------------------------------------------------
// LOOP14: RS485 错误计数器 (0x0880-0x0883)
// ----------------------------------------------------------------------------
//
// metuory 仪表盘显示这 4 个寄存器诊断 RS485 通信健康:
//   0x0880 RS485_1_COMERR: 主站通信错误 (超时/CRC 错/从站不匹配), 累加.
//   0x0881 RS485_1_APPERR: 主站收到异常响应 (Illegal F/R/V/Slave Failure), 累加.
//   0x0882 RS485_2_COMERR: 从站通信错误 (CRC 错/帧截断), 累加.
//   0x0883 RS485_2_APPERR: 从站请求解析错误 (功能码非法/寄存器非法), 累加.
//
// 对齐 MCA modbus_master.cpp 的错误计数语义, metuory 端按此显示诊断值.
// 实现: 4 个 AtomicU32 无锁累加, 在 rtu_master/slave 异常点调用.
// read_hold_reg(0x0880..=0x0883) 前置拦截读取 RS485_STATS, 不走 holding_buf 兜底.

use std::sync::atomic::{AtomicU32, Ordering};

/// Web 状态灯保留最近一次有效通信的时间窗。原 MCA 每轮扫描先灭灯，收到有效帧
/// 后点亮；1 秒窗口在 500ms 页面轮询下可见，同时不会把历史成功永久显示为在线。
const RS485_ACTIVITY_WINDOW_MS: u32 = 1_000;

/// RS485 通信/应用错误计数器 (无锁原子, 全局静态)
pub struct Rs485Stats {
    pub master_comerr: AtomicU32, // 0x0880
    pub master_apperr: AtomicU32, // 0x0881
    pub slave_comerr: AtomicU32,  // 0x0882
    pub slave_apperr: AtomicU32,  // 0x0883
    master_last_ok_ms: AtomicU32,
    slave_last_ok_ms: AtomicU32,
}

impl Rs485Stats {
    pub const fn new() -> Self {
        Self {
            master_comerr: AtomicU32::new(0),
            master_apperr: AtomicU32::new(0),
            slave_comerr: AtomicU32::new(0),
            slave_apperr: AtomicU32::new(0),
            master_last_ok_ms: AtomicU32::new(0),
            slave_last_ok_ms: AtomicU32::new(0),
        }
    }

    /// 读 RS485 主站通信错误 (0x0880)
    #[inline]
    pub fn master_comerr(&self) -> u16 {
        self.master_comerr.load(Ordering::Relaxed) as u16
    }

    /// 读 RS485 主站应用错误 (0x0881)
    #[inline]
    pub fn master_apperr(&self) -> u16 {
        self.master_apperr.load(Ordering::Relaxed) as u16
    }

    /// 读 RS485 从站通信错误 (0x0882)
    #[inline]
    pub fn slave_comerr(&self) -> u16 {
        self.slave_comerr.load(Ordering::Relaxed) as u16
    }

    /// 读 RS485 从站应用错误 (0x0883)
    #[inline]
    pub fn slave_apperr(&self) -> u16 {
        self.slave_apperr.load(Ordering::Relaxed) as u16
    }

    /// 累加主站通信错误 (timeout/CRC/slave mismatch)
    #[inline]
    pub fn inc_master_comerr(&self) {
        self.master_comerr.fetch_add(1, Ordering::Relaxed);
    }

    /// 累加主站应用错误 (Illegal F/R/V/Slave Failure)
    #[inline]
    pub fn inc_master_apperr(&self) {
        self.master_apperr.fetch_add(1, Ordering::Relaxed);
    }

    /// 累加从站通信错误 (CRC 错/帧截断)
    #[inline]
    pub fn inc_slave_comerr(&self) {
        self.slave_comerr.fetch_add(1, Ordering::Relaxed);
    }

    /// 累加从站应用错误 (非法功能码/非法寄存器)
    #[inline]
    pub fn inc_slave_apperr(&self) {
        self.slave_apperr.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    fn uptime_ms() -> u32 {
        // 低 32 位约 49.7 天回绕，wrapping_sub 可在回绕前后保持窗口判断正确。
        (unsafe { esp_idf_sys::esp_timer_get_time() } as u64 / 1_000) as u32
    }

    #[inline]
    pub fn mark_master_ok(&self) {
        self.master_last_ok_ms
            .store(Self::uptime_ms().max(1), Ordering::Relaxed);
    }

    #[inline]
    pub fn mark_slave_ok(&self) {
        self.slave_last_ok_ms
            .store(Self::uptime_ms().max(1), Ordering::Relaxed);
    }

    #[inline]
    pub fn master_active(&self) -> bool {
        activity_is_recent(
            self.master_last_ok_ms.load(Ordering::Relaxed),
            Self::uptime_ms(),
        )
    }

    #[inline]
    pub fn slave_active(&self) -> bool {
        activity_is_recent(
            self.slave_last_ok_ms.load(Ordering::Relaxed),
            Self::uptime_ms(),
        )
    }
}

#[inline]
fn activity_is_recent(last_ms: u32, now_ms: u32) -> bool {
    last_ms != 0 && now_ms.wrapping_sub(last_ms) <= RS485_ACTIVITY_WINDOW_MS
}

/// 全局 RS485 错误计数静态实例
pub static RS485_STATS: Rs485Stats = Rs485Stats::new();

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

// ============================================================================
// PDU 处理 — 无堆分配版本 (写入调用方提供的栈缓冲区)
// ============================================================================

/// PDU 处理结果: 写入字节数，或异常码
///
/// Ok(n) = 成功，写入 out[..n]
/// Err(code) = 异常，调用方需构造 [func|0x80, code] 异常 PDU
enum PduResult {
    Ok(usize),
    Err(u8),
}

/// 统一的 Modbus PDU 处理入口 (无堆分配版本)。
///
/// 输入 `func` (功能码) + `pdu` (去除功能码后的 PDU 载荷) + `out` (输出缓冲区)，
/// 返回写入 `out` 的字节数。
///
/// 正常响应写入 `[func, body...]` 到 `out`。
/// 异常响应写入 `[func|0x80, code]` 到 `out`。
pub fn handle_pdu<B: ModbusBackend>(
    backend: &B,
    func: u8,
    pdu: &[u8],
    out: &mut [u8; PDU_BUF_SIZE],
) -> usize {
    // 内部函数从 out[1] 开始写 body (out[0] 留给 func)
    // 返回 body 长度 (不含 func), 或异常码
    let result = match func {
        0x01 => read_bits_pdu(backend, pdu, |b, a| b.read_coil(a), out),
        0x02 => read_bits_pdu(backend, pdu, |b, a| b.read_discrete_input(a), out),
        0x03 => read_regs_pdu(
            backend,
            pdu,
            |b, a, c, dst| b.encode_holding_registers(a, c, dst),
            out,
        ),
        0x04 => read_regs_pdu(
            backend,
            pdu,
            |b, a, c, dst| b.encode_input_registers(a, c, dst),
            out,
        ),
        0x05 => write_single_coil_pdu(backend, pdu, out),
        0x06 => write_single_reg_pdu(backend, pdu, out),
        0x0F => write_multi_coils_pdu(backend, pdu, out),
        0x10 => write_multi_regs_pdu(backend, pdu, out),
        _ => {
            crate::error::log_warn(crate::error::module_id::MODBUS, 200, func as u32);
            out[0] = func | 0x80;
            out[1] = exc::ILLEGAL_FUNCTION;
            return 2;
        }
    };

    match result {
        PduResult::Ok(body_len) => {
            // 内部函数已写 body 到 out[1..1+body_len], 现在填 func
            out[0] = func;
            1 + body_len
        }
        PduResult::Err(code) => {
            // LOOP14: 从站发送异常响应 → 累加从站应用错误
            // (RTU slave 接收方在收到非法 F/R/V 请求时累加, 表示上层语义错误)
            RS485_STATS.inc_slave_apperr();
            out[0] = func | 0x80;
            out[1] = code;
            2
        }
    }
}

fn read_bits_pdu<B, F>(backend: &B, pdu: &[u8], f: F, out: &mut [u8; PDU_BUF_SIZE]) -> PduResult
where
    B: ModbusBackend,
    F: Fn(&B, u16) -> Option<bool>,
{
    if pdu.len() != 4 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 2000 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    if (addr as u32) + (count as u32) - 1 > u16::MAX as u32 {
        return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
    }
    let byte_count = (count as usize).div_ceil(8);
    // out[0] = func (由 handle_pdu 填写), out[1] = byte_count, out[2..] = data
    // 注意: 调用方会在 out 开头写 func, 所以我们写 body 部分
    // 实际上 handle_pdu 期望我们写完整 PDU (含 func)
    // 改为: out[0] 预留给 func, 从 out[1] 开始写 body
    // 但为了与现有模式兼容，我们在 read_bits_pdu 内部写完整 PDU
    // 这里只返回 body 长度，让 handle_pdu 拼 func
    // 重构: 直接写完整 PDU
    out[0] = 0; // func 占位, 由 handle_pdu 覆写
    out[1] = byte_count as u8;
    // 清零 bit 字节区域
    for i in 0..byte_count {
        out[2 + i] = 0;
    }
    for i in 0..count as usize {
        let Some(bit) = f(backend, addr + i as u16) else {
            return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
        };
        if bit {
            out[2 + i / 8] |= 1 << (i % 8);
        }
    }
    // body = byte_count(1) + packed bits(N)，不含 out[0] 的功能码。
    PduResult::Ok(1 + byte_count)
}

fn read_regs_pdu<B, F>(backend: &B, pdu: &[u8], f: F, out: &mut [u8; PDU_BUF_SIZE]) -> PduResult
where
    B: ModbusBackend,
    F: Fn(&B, u16, u16, &mut [u8]) -> bool,
{
    if pdu.len() != 4 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    if count == 0 || count > 125 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    if (addr as u32) + (count as u32) - 1 > u16::MAX as u32 {
        return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
    }
    let byte_count = count as usize * 2;
    if !f(backend, addr, count, &mut out[2..2 + byte_count]) {
        return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
    }
    out[0] = 0; // func 占位
    out[1] = byte_count as u8;
    // body = byte_count(1) + register bytes(N)，不含 out[0] 的功能码。
    PduResult::Ok(1 + byte_count)
}

fn write_single_coil_pdu<B: ModbusBackend>(
    backend: &B,
    pdu: &[u8],
    out: &mut [u8; PDU_BUF_SIZE],
) -> PduResult {
    if pdu.len() != 4 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    let on = value == 0xFF00;
    if !on && value != 0 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    if !backend.write_single_coil(addr, on) {
        return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
    }
    // Echo: addr(2) + value(2)
    out[0] = 0; // func 占位
    out[1] = pdu[0];
    out[2] = pdu[1];
    out[3] = pdu[2];
    out[4] = pdu[3];
    PduResult::Ok(4)
}

fn write_single_reg_pdu<B: ModbusBackend>(
    backend: &B,
    pdu: &[u8],
    out: &mut [u8; PDU_BUF_SIZE],
) -> PduResult {
    if pdu.len() != 4 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let value = u16::from_be_bytes([pdu[2], pdu[3]]);
    if !backend.write_single_register(addr, value) {
        return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
    }
    // Echo: addr(2) + value(2)
    out[0] = 0; // func 占位
    out[1] = pdu[0];
    out[2] = pdu[1];
    out[3] = pdu[2];
    out[4] = pdu[3];
    PduResult::Ok(4)
}

fn write_multi_coils_pdu<B: ModbusBackend>(
    backend: &B,
    pdu: &[u8],
    out: &mut [u8; PDU_BUF_SIZE],
) -> PduResult {
    if pdu.len() < 5 {
        return PduResult::Err(exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    // LOOP9: count 范围校验 (对齐 read_bits_pdu: count==0 或 count>2000 非法)
    if count == 0 || count as usize > MAX_BITS_PER_READ {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    if pdu.len() != 5 + byte_count || byte_count != (count as usize).div_ceil(8) {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    if !backend.write_multiple_coils(addr, count, &pdu[5..]) {
        return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
    }
    // Echo: addr(2) + count(2)
    out[0] = 0; // func 占位
    out[1] = pdu[0];
    out[2] = pdu[1];
    out[3] = pdu[2];
    out[4] = pdu[3];
    PduResult::Ok(4)
}

fn write_multi_regs_pdu<B: ModbusBackend>(
    backend: &B,
    pdu: &[u8],
    out: &mut [u8; PDU_BUF_SIZE],
) -> PduResult {
    if pdu.len() < 5 {
        return PduResult::Err(exc::SLAVE_DEVICE_FAILURE);
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]);
    let byte_count = pdu[4] as usize;
    // FC=16 标准上限 123；125 仅适用于 FC=03/04 读取。
    if count == 0 || count as usize > MAX_REGS_PER_WRITE {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    if pdu.len() != 5 + byte_count || byte_count != count as usize * 2 {
        return PduResult::Err(exc::ILLEGAL_DATA_VALUE);
    }
    // 用栈上固定数组替代 Vec::with_capacity
    let mut regs: heapless::Vec<u16, MAX_REGS_PER_READ> = heapless::Vec::new();
    for i in 0..count as usize {
        let _ = regs.push(u16::from_be_bytes([pdu[5 + 2 * i], pdu[6 + 2 * i]]));
    }
    if !backend.write_multiple_registers(addr, &regs) {
        return PduResult::Err(exc::ILLEGAL_DATA_ADDRESS);
    }
    // Echo: addr(2) + count(2)
    out[0] = 0; // func 占位
    out[1] = pdu[0];
    out[2] = pdu[1];
    out[3] = pdu[2];
    out[4] = pdu[3];
    PduResult::Ok(4)
}

/// 构造异常响应 body: [0x80|func, code]
fn build_exception_body(func: u8, code: u8) -> [u8; 2] {
    [func | 0x80, code]
}

// ============================================================================
// 单元测试
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc16_known_vectors() {
        assert_eq!(modbus_crc16(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]), 0xC5CD);
        assert_eq!(modbus_crc16(&[0x01, 0x04, 0x02, 0xFF, 0xFF]), 0xB880);
        assert_eq!(modbus_crc16(&[0x01, 0x06, 0x00, 0x01, 0x00, 0x03]), 0xD9AA);
    }

    #[test]
    fn test_crc16_empty() {
        assert_eq!(modbus_crc16(&[]), 0xFFFF);
    }

    #[test]
    fn test_crc16_single_byte() {
        assert_eq!(modbus_crc16(&[0x01]), 0x807E);
    }

    #[test]
    fn test_exception_response() {
        let exc = build_exception_response(0x06, exc::ILLEGAL_DATA_ADDRESS);
        assert_eq!(exc[0], 0x86);
        assert_eq!(exc[1], exc::ILLEGAL_DATA_ADDRESS);
    }

    /// Dummy backend for testing (heapless version)
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
        fn read_coil(&self, addr: u16) -> Option<bool> {
            self.coils.get(addr as usize).copied()
        }
        fn read_discrete_input(&self, addr: u16) -> Option<bool> {
            self.disc.get(addr as usize).copied()
        }
        fn encode_holding_registers(&self, addr: u16, count: u16, out: &mut [u8]) -> bool {
            if out.len() != count as usize * 2 {
                return false;
            }
            for i in 0..count {
                let offset = i as usize * 2;
                out[offset..offset + 2]
                    .copy_from_slice(&self.holding[(addr + i) as usize].to_be_bytes());
            }
            true
        }
        fn encode_input_registers(&self, addr: u16, count: u16, out: &mut [u8]) -> bool {
            if out.len() != count as usize * 2 {
                return false;
            }
            for i in 0..count {
                let offset = i as usize * 2;
                out[offset..offset + 2]
                    .copy_from_slice(&self.input_reg[(addr + i) as usize].to_be_bytes());
            }
            true
        }
        fn write_single_coil(&self, _addr: u16, _value: bool) -> bool {
            true
        }
        fn write_single_register(&self, _addr: u16, _value: u16) -> bool {
            true
        }
        fn write_multiple_coils(&self, _addr: u16, _count: u16, _packed_values: &[u8]) -> bool {
            true
        }
        fn write_multiple_registers(&self, _addr: u16, _values: &[u16]) -> bool {
            true
        }
    }

    struct FullRangeBackend;

    impl ModbusBackend for FullRangeBackend {
        fn read_coil(&self, _: u16) -> Option<bool> {
            Some(true)
        }
        fn read_discrete_input(&self, _: u16) -> Option<bool> {
            Some(false)
        }
        fn encode_holding_registers(&self, _: u16, count: u16, out: &mut [u8]) -> bool {
            if out.len() != count as usize * 2 {
                return false;
            }
            for chunk in out.chunks_exact_mut(2) {
                chunk.copy_from_slice(&0xA55Au16.to_be_bytes());
            }
            true
        }
        fn encode_input_registers(&self, _: u16, count: u16, out: &mut [u8]) -> bool {
            self.encode_holding_registers(0, count, out)
        }
        fn write_single_coil(&self, _: u16, _: bool) -> bool {
            true
        }
        fn write_single_register(&self, _: u16, _: u16) -> bool {
            true
        }
        fn write_multiple_coils(&self, _: u16, _: u16, _: &[u8]) -> bool {
            true
        }
        fn write_multiple_registers(&self, _: u16, _: &[u16]) -> bool {
            true
        }
    }

    #[test]
    fn test_read_holding_registers_pdu() {
        let mut backend = DummyBackend::new();
        backend.holding[0] = 0x1234;
        backend.holding[1] = 0x5678;
        let pdu = [0x00, 0x00, 0x00, 0x02];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x03, &pdu, &mut out);
        assert_eq!(out[0], 0x03); // func
        assert_eq!(out[1], 0x04); // byte count = 2 * 2
        assert_eq!(&out[2..6], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(n, 6);
    }

    #[test]
    fn test_read_response_rejects_incomplete_backend_range() {
        struct EmptyBackend;
        impl ModbusBackend for EmptyBackend {
            fn read_coil(&self, _: u16) -> Option<bool> {
                None
            }
            fn read_discrete_input(&self, _: u16) -> Option<bool> {
                None
            }
            fn encode_holding_registers(&self, _: u16, _: u16, _: &mut [u8]) -> bool {
                false
            }
            fn encode_input_registers(&self, _: u16, _: u16, _: &mut [u8]) -> bool {
                false
            }
            fn write_single_coil(&self, _: u16, _: bool) -> bool {
                false
            }
            fn write_single_register(&self, _: u16, _: u16) -> bool {
                false
            }
            fn write_multiple_coils(&self, _: u16, _: u16, _: &[u8]) -> bool {
                false
            }
            fn write_multiple_registers(&self, _: u16, _: &[u16]) -> bool {
                false
            }
        }

        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&EmptyBackend, 0x03, &[0, 0, 0, 1], &mut out);
        assert_eq!(&out[..n], &[0x83, exc::ILLEGAL_DATA_ADDRESS]);
    }

    #[test]
    fn test_write_single_register_pdu() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x05, 0x00, 0xCA];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x06, &pdu, &mut out);
        assert_eq!(n, 5);
        assert_eq!(&out[1..5], &pdu);
    }

    #[test]
    fn test_read_coils_pdu() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x0A]; // 10 coils
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x01, &pdu, &mut out);
        assert_eq!(out[0], 0x01); // func (占位未被覆写, 因为 handle_pdu 后续会设)
        assert_eq!(out[1], 0x02); // byte count (10 bits → 2 bytes)
        assert_eq!(n, 4); // func + byte_count + 2 bytes
    }

    #[test]
    fn test_standard_maximum_bit_read_uses_exact_pdu_size() {
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&FullRangeBackend, 0x01, &[0, 0, 0x07, 0xD0], &mut out);
        assert_eq!(n, 252); // func + byte_count + ceil(2000 / 8)
        assert_eq!(out[1], 250);
        assert!(out[2..n].iter().all(|&byte| byte == 0xFF));
    }

    #[test]
    fn test_standard_maximum_register_read_encodes_directly() {
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&FullRangeBackend, 0x03, &[0, 0, 0, 125], &mut out);
        assert_eq!(n, 252); // func + byte_count + 125 words
        assert_eq!(out[1], 250);
        assert!(
            out[2..n]
                .chunks_exact(2)
                .all(|word| word == 0xA55Au16.to_be_bytes())
        );
    }

    #[test]
    fn test_standard_modbus_size_constants() {
        assert_eq!(MODBUS_MAX_PDU_LEN, 253);
        assert_eq!(MODBUS_TCP_MAX_MBAP_LENGTH, 254);
        assert_eq!(MODBUS_TCP_MAX_ADU_LEN, 260);
        assert_eq!(PDU_BUF_SIZE, MODBUS_MAX_PDU_LEN);
    }

    #[test]
    fn test_rs485_activity_window_handles_timeout_and_wraparound() {
        assert!(!activity_is_recent(0, 500));
        assert!(activity_is_recent(500, 1_500));
        assert!(!activity_is_recent(500, 1_501));
        assert!(activity_is_recent(u32::MAX - 200, 300));
    }

    #[test]
    fn test_read_discrete_inputs_exception() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x00]; // count=0 invalid
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x02, &pdu, &mut out);
        assert_eq!(out[0], 0x82);
        assert_eq!(out[1], exc::ILLEGAL_DATA_VALUE);
        assert_eq!(n, 2);
    }

    #[test]
    fn test_write_multiple_coils_pdu() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x0A, 0x02, 0xFF, 0x03];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x0F, &pdu, &mut out);
        assert_eq!(n, 5);
        assert_eq!(&out[1..5], &[0x00, 0x00, 0x00, 0x0A]);
    }

    #[test]
    fn test_illegal_function_exception() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x07, &pdu, &mut out);
        assert_eq!(out[0], 0x87);
        assert_eq!(out[1], exc::ILLEGAL_FUNCTION);
        assert_eq!(n, 2);
    }

    #[test]
    fn test_illegal_data_value() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x00]; // count=0 for FC=03
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x03, &pdu, &mut out);
        assert_eq!(out[0], 0x83);
        assert_eq!(out[1], exc::ILLEGAL_DATA_VALUE);
        assert_eq!(n, 2);
    }

    #[test]
    fn test_count_too_large() {
        let backend = DummyBackend::new();
        let pdu = [0x00, 0x00, 0x00, 0x80]; // 128 regs
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x03, &pdu, &mut out);
        assert_eq!(out[0], 0x83);
        assert_eq!(out[1], exc::ILLEGAL_DATA_VALUE);
        assert_eq!(n, 2);
    }

    // LOOP9 回归测试: FC=0F/FC=10 缺少 count 校验 (旧实现 count=0 返回成功)
    #[test]
    fn test_write_multi_coils_count_zero_rejected() {
        let backend = DummyBackend::new();
        // FC=0F, addr=0, count=0, byte_count=0
        let pdu = [0x00, 0x00, 0x00, 0x00, 0x00];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x0F, &pdu, &mut out);
        assert_eq!(out[0], 0x8F);
        assert_eq!(out[1], exc::ILLEGAL_DATA_VALUE);
        assert_eq!(n, 2);
    }

    #[test]
    fn test_write_multi_coils_count_too_large_rejected() {
        let backend = DummyBackend::new();
        // FC=0F, addr=0, count=2001 (> MAX_BITS_PER_READ=2000)
        let pdu = [0x00, 0x00, 0x07, 0xD1, 0x00];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x0F, &pdu, &mut out);
        assert_eq!(out[0], 0x8F);
        assert_eq!(out[1], exc::ILLEGAL_DATA_VALUE);
        assert_eq!(n, 2);
    }

    #[test]
    fn test_write_multi_regs_count_zero_rejected() {
        let backend = DummyBackend::new();
        // FC=10, addr=0, count=0, byte_count=0
        let pdu = [0x00, 0x00, 0x00, 0x00, 0x00];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x10, &pdu, &mut out);
        assert_eq!(out[0], 0x90);
        assert_eq!(out[1], exc::ILLEGAL_DATA_VALUE);
        assert_eq!(n, 2);
    }

    #[test]
    fn test_write_multi_regs_above_standard_123_rejected() {
        let backend = DummyBackend::new();
        // 124 regs 已超过 FC=16 PDU 的 253-byte 上限，无需提供 data 即应先拒绝。
        let pdu = [0x00, 0x00, 0x00, 0x7C, 0xF8];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x10, &pdu, &mut out);
        assert_eq!(&out[..n], &[0x90, exc::ILLEGAL_DATA_VALUE]);
    }

    #[test]
    fn test_write_multi_regs_count_too_large_rejected() {
        let backend = DummyBackend::new();
        // FC=10, addr=0, count=126 (> MAX_REGS_PER_READ=125)
        let pdu = [0x00, 0x00, 0x00, 0x7E, 0x00];
        let mut out = [0u8; PDU_BUF_SIZE];
        let n = handle_pdu(&backend, 0x10, &pdu, &mut out);
        assert_eq!(out[0], 0x90);
        assert_eq!(out[1], exc::ILLEGAL_DATA_VALUE);
        assert_eq!(n, 2);
    }
}
