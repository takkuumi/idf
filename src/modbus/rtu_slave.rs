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
                        if let Err(e) = handle_request(&mut port, &backend, &buf[..n], slave_addr) {
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

fn handle_request(
    port: &mut Rs485Port,
    backend: &BusBackend,
    req: &[u8],
    addr: u8,
) -> AppResult<()> {
    // 最小帧: slave(1) + func(1) + crc(2) = 4
    if req.len() < 4 {
        // LOOP14: 帧过短 → 累加从站通信错误
        crate::modbus::shared::RS485_STATS.inc_slave_comerr();
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
        // LOOP14: CRC 错 → 累加从站通信错误
        crate::modbus::shared::RS485_STATS.inc_slave_comerr();
        return Ok(());
    }

    crate::modbus::shared::RS485_STATS.mark_slave_ok();

    let func = req[1];
    let resp = build_response(backend, slave, func, &req[2..n - 2]);

    // 广播不返回响应
    if slave == 0 {
        return Ok(());
    }

    port.write(&resp)?;
    Ok(())
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
}
