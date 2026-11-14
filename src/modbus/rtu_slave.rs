//! Modbus RTU 从站
//!
//! 通过 RS485 #1 (UART2) 响应外部主站请求。
//! 支持 FC=01/02/03/04/05/06/0F/10, 错误时返回 Modbus 异常响应。
//!
//! 本模块**手写 Modbus RTU 帧 + CRC16**, 不依赖 umodbus crate。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::modbus::rtu_slave as cfg;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::shared::{BusBackend, modbus_crc16};
use crate::rs485::{Rs485Config, Rs485Port};

/// 任务心跳记录 (静态分配, main_loop 监控)
/// 最大停滞阈值放宽到 10 (从站监听周期 1s, 允许长时间无请求)
static TASK_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-slave", 10);
static STARTED: AtomicBool = AtomicBool::new(false);

pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    let port_cfg = Rs485Config::from_rtu_slave();

    // 优先读取 SystemConfig.rs485[1] 寄存器的从站地址 (可由 Modbus/AT 动态配置),
    // 回退到编译期常量 ADDR.
    let slave_addr = crate::bus::config_state::config_read()
        .map(|cs| cs.cfg.rs485[1].slave_addr.max(1))
        .unwrap_or(cfg::ADDR);

    let mut port = Rs485Port::open(&port_cfg)?;
    let backend = BusBackend;

    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mb-rtu-slave".into())
        .stack_size(crate::safety::stack_budget::MODBUS_RTU_SLAVE)
        .spawn(move || {
            // LOOP14: 所有长期运行 pthread 必须订阅 WDT
            health::subscribe_wdt();
            let mut buf = [0u8; 256];
            loop {
                // 心跳: 每次循环 (即使无请求也 1s 返回一次)
                TASK_HB.tick();
                // LOOP15: 必须喂 WDT — port.read(1000ms) + retry 累计可能 > 10s 触发系统复位
                health::feed_wdt();
                match port.read(&mut buf, 1000) {
                    Ok(0) => continue,
                    Ok(n) => {
                        if let Err(e) =
                            handle_request(&mut port, &backend, &buf[..n], slave_addr, 1)
                        {
                            log::warn!("[mb-rtu-slave] handle: {}", e);
                        }
                    }
                    Err(e) => {
                        log::warn!("[mb-rtu-slave] read: {}", e);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        });
    health::reset_thread_core();
    result.map_err(|e| crate::error::AppError::Modbus(format!("spawn: {e}")))?;
    STARTED.store(true, Ordering::Release);
    health::register_with_stack(&TASK_HB, crate::safety::stack_budget::MODBUS_RTU_SLAVE);

    log::info!(
        "[mb-rtu-slave] started on uart{} addr={}",
        cfg::UART_PORT,
        slave_addr
    );
    Ok(())
}

pub(crate) fn handle_request(
    port: &mut Rs485Port,
    backend: &BusBackend,
    req: &[u8],
    addr: u8,
    port_index: usize,
) -> AppResult<()> {
    // 最小帧: slave(1) + func(1) + crc(2) = 4
    if req.len() < 4 {
        // LOOP14: 帧过短 → 置从站通信错误=0x04
        crate::modbus::shared::RS485_STATS.inc_port_comerr(port_index);
        return Ok(());
    }

    let slave = req[0];
    // 广播 0 不响应
    if slave != 0 && slave != addr {
        return Ok(());
    }

    // CRC 校验
    let n = req.len();
    let crc = modbus_crc16(&req[..n - 2]);
    let recv_crc = u16::from_le_bytes([req[n - 2], req[n - 1]]);
    if crc != recv_crc {
        log::debug!(
            "[mb-rtu-slave] crc mismatch: {:#06x}!={:#06x}",
            crc,
            recv_crc
        );
        // LOOP14: CRC 错 → 置从站通信错误=0x04
        crate::modbus::shared::RS485_STATS.inc_port_comerr(port_index);
        return Ok(());
    }

    crate::modbus::shared::RS485_STATS.mark_port_ok(port_index);

    let service_started = std::time::Instant::now();
    let func = req[1];
    let resp = build_response(backend, slave, func, &req[2..n - 2]);
    if resp
        .get(1)
        .is_some_and(|response_func| response_func & 0x80 != 0)
    {
        let code = resp.get(2).copied().unwrap_or(1);
        crate::modbus::shared::RS485_STATS.set_port_apperr(port_index, code);
    }

    let build_time = service_started.elapsed();
    // 广播不返回响应
    if slave == 0 {
        return Ok(());
    }

    let result = port.write(&resp);
    crate::rs485::diagnostics::service(port.port(), build_time, service_started.elapsed());
    result
}

/// Dispatch complete Modbus request frames from a UART receive batch.
///
/// On F3 RS485-1, the physical link can return the slave's TX bytes to RX.
/// Under back-to-back polling, an echoed response and the following request
/// may be delivered as one UART batch. Passing the entire batch to
/// `handle_request` fails CRC validation and drops the real request. Scan for
/// bounded request candidates with a matching unit ID and CRC, while ignoring
/// response frames (including echoed exception responses).
pub(crate) fn handle_request_stream(
    port: &mut Rs485Port,
    backend: &BusBackend,
    batch: &[u8],
    addr: u8,
    port_index: usize,
) -> AppResult<()> {
    let mut cursor = 0usize;
    while let Some((start, length)) = find_valid_request(batch, cursor, addr) {
        handle_request(
            port,
            backend,
            &batch[start..start + length],
            addr,
            port_index,
        )?;
        cursor = start + length;
    }
    Ok(())
}

fn find_valid_request(batch: &[u8], from: usize, addr: u8) -> Option<(usize, usize)> {
    if batch.len() < 4 || from > batch.len() - 4 {
        return None;
    }
    let mut start = from;
    while start <= batch.len() - 4 {
        let slave = batch[start];
        if slave != 0 && slave != addr {
            start += 1;
            continue;
        }
        let function = batch[start + 1];
        if function & 0x80 != 0 {
            // Exception responses have fixed length and are never requests.
            let exception_len = 5;
            if start + exception_len <= batch.len() {
                let frame = &batch[start..start + exception_len];
                let received = u16::from_le_bytes([frame[3], frame[4]]);
                if modbus_crc16(&frame[..3]) == received {
                    start += exception_len;
                    continue;
                }
            }
            start += 1;
            continue;
        }
        // FC01..04 responses declare their byte length. Skip a valid echoed
        // response before trying the same function's 8-byte request shape.
        if (1..=4).contains(&function) && start + 3 <= batch.len() {
            let response_len = 5usize.saturating_add(usize::from(batch[start + 2]));
            if response_len <= 255 && start + response_len <= batch.len() {
                let frame = &batch[start..start + response_len];
                let received =
                    u16::from_le_bytes([frame[response_len - 2], frame[response_len - 1]]);
                if modbus_crc16(&frame[..response_len - 2]) == received {
                    start += response_len;
                    continue;
                }
            }
        }
        let length = match function {
            1..=6 => 8,
            15 | 16 => {
                if start + 7 > batch.len() {
                    start += 1;
                    continue;
                }
                9usize.saturating_add(usize::from(batch[start + 6]))
            }
            // Preserve the previous illegal-function exception behavior for
            // unknown, standalone RTU requests whose complete frame is here.
            _ => batch.len() - start,
        };
        if length > 256 || start + length > batch.len() {
            start += 1;
            continue;
        }
        let frame = &batch[start..start + length];
        let received = u16::from_le_bytes([frame[length - 2], frame[length - 1]]);
        if modbus_crc16(&frame[..length - 2]) == received {
            return Some((start, length));
        }
        start += 1;
    }
    None
}

fn build_response(backend: &BusBackend, slave: u8, func: u8, pdu: &[u8]) -> heapless::Vec<u8, 256> {
    let mut out: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = out.push(slave);
    // 无堆分配: handle_pdu 写入栈缓冲区
    let mut pdu_buf = [0u8; crate::modbus::shared::PDU_BUF_SIZE];
    let pdu_len = crate::modbus::shared::handle_pdu(backend, func, pdu, &mut pdu_buf);
    if pdu_len >= 2 {
        // handle_pdu 写完整 PDU: [func, body...] 或 [func|0x80, code]
        let _ = out.extend_from_slice(&pdu_buf[..pdu_len]);
    }
    let crc = modbus_crc16(&out);
    let _ = out.push(crc as u8);
    let _ = out.push((crc >> 8) as u8);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtu_read_response_has_exact_length_before_crc() {
        let response = build_response(&BusBackend, 1, 0x03, &[0x08, 0xA5, 0, 1]);
        assert_eq!(response.len(), 7); // slave + func + byte_count + word + CRC
        assert_eq!(&response[..3], &[1, 0x03, 0x02]);
        let n = response.len();
        assert_eq!(
            u16::from_le_bytes([response[n - 2], response[n - 1]]),
            modbus_crc16(&response[..n - 2])
        );
    }

    #[test]
    fn test_stream_finds_request_after_echoed_205_byte_fc03_response() {
        let request = [1, 3, 0x08, 0xFC, 0, 100, 0x86, 0x71];
        let response = build_response(&BusBackend, 1, 3, &[0x08, 0xFC, 0, 100]);
        assert_eq!(response.len(), 205);
        let mut batch = heapless::Vec::<u8, 256>::new();
        batch.extend_from_slice(&response).unwrap();
        batch.extend_from_slice(&request).unwrap();
        assert_eq!(find_valid_request(&batch, 0, 1), Some((205, 8)));
    }

    #[test]
    fn test_stream_ignores_echoed_exception_response() {
        let exception = [1, 0x83, 1, 0x80, 0xF0];
        assert_eq!(modbus_crc16(&exception[..3]), 0xF080);
        assert_eq!(find_valid_request(&exception, 0, 1), None);
    }

    #[test]
    fn test_stream_requires_request_crc_and_honors_nonzero_offset() {
        let mut batch = [0xAA; 17];
        batch[5..13].copy_from_slice(&[1, 3, 0x08, 0xFC, 0, 100, 0x86, 0x71]);
        assert_eq!(find_valid_request(&batch, 0, 1), Some((5, 8)));
        batch[12] ^= 1;
        assert_eq!(find_valid_request(&batch, 0, 1), None);
    }

    #[test]
    fn test_pc_device_mmp_83_word_rtu_response() {
        let response = build_response(&BusBackend, 1, 0x03, &[0x08, 0x94, 0, 83]);
        assert_eq!(response.len(), 1 + 2 + 83 * 2 + 2);
        assert_eq!(&response[..3], &[1, 0x03, 166]);
        let n = response.len();
        assert_eq!(
            u16::from_le_bytes([response[n - 2], response[n - 1]]),
            modbus_crc16(&response[..n - 2])
        );
    }

    #[test]
    fn test_rtu_fc16_writes_contiguous_logic_words() {
        let address = crate::config::regs::HOLD_DEVICE_CONFIG + 123;
        let pdu = [
            (address >> 8) as u8,
            address as u8,
            0,
            3,
            6,
            0x11,
            0x22,
            0x33,
            0x44,
            0x55,
            0x66,
        ];
        let response = build_response(&BusBackend, 1, 0x10, &pdu);
        assert_eq!(&response[..6], &[1, 0x10, pdu[0], pdu[1], 0, 3]);
        for (offset, expected) in [0x1122, 0x3344, 0x5566].into_iter().enumerate() {
            assert_eq!(
                crate::bus::backends::read_hold_reg(address + offset as u16),
                Some(expected)
            );
        }
    }

    #[test]
    fn test_rtu_fc16_invalid_tail_is_rejected_without_partial_write() {
        let address = crate::config::regs::HOLD_CFG_END;
        let before = crate::bus::backends::read_hold_reg(address);
        let response = build_response(
            &BusBackend,
            1,
            0x10,
            &[
                (address >> 8) as u8,
                address as u8,
                0,
                2,
                4,
                0xAA,
                0xAA,
                0xBB,
                0xBB,
            ],
        );
        assert_eq!(
            &response[..3],
            &[1, 0x90, crate::modbus::shared::exc::ILLEGAL_DATA_ADDRESS]
        );
        assert_eq!(crate::bus::backends::read_hold_reg(address), before);
    }
}
