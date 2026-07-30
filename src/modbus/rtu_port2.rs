//! Modbus RTU 第 3 端口从站 (RS485-3, UART0)
//!
//! 对齐参考固件 MCA_F16V2_1_F48_BLE 的 RS485-3 (Serial/UART0):
//! - 默认 9600 8N1, 从站监听模式
//! - 与 RS485 #1 (rtu_slave.rs) 共享同一 Modbus 后端, 仅 UART 不同
//! - UART0 与 USB CDC/JTAG 复用, 启用后失去调试串口
//!
//! 由 `config::modbus::rtu_port2::ENABLED` 控制是否启动 (默认 false).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::modbus::rtu_port2 as cfg;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::shared::{BusBackend, modbus_crc16};
use crate::rs485::{Rs485Config, Rs485Port};

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-port2", 10);
static STARTED: AtomicBool = AtomicBool::new(false);

pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    let port_cfg = Rs485Config::from_rtu_port2();

    // 优先读取 SystemConfig.rs485[2] 寄存器的从站地址
    let slave_addr = crate::bus::config_state::config_read()
        .map(|cs| cs.cfg.rs485[2].slave_addr.max(1))
        .unwrap_or(cfg::ADDR);

    let mut port = Rs485Port::open(&port_cfg)?;
    let backend = BusBackend;

    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mb-rtu-port2".into())
        .stack_size(crate::safety::stack_budget::MODBUS_RTU_PORT2)
        .spawn(move || {
            // LOOP14: 所有长期运行 pthread 必须订阅 WDT
            health::subscribe_wdt();
            let mut buf = [0u8; 256];
            loop {
                TASK_HB.tick();
                // LOOP15: 必须周期喂 WDT — UART0 可能与其他任务共享, port.read 期间也须被保护
                health::feed_wdt();
                match port.read(&mut buf, 1000) {
                    Ok(0) => continue,
                    Ok(n) => {
                        if let Err(e) = handle_request(&mut port, &backend, &buf[..n], slave_addr) {
                            log::warn!("[mb-rtu-port2] handle: {}", e);
                        }
                    }
                    Err(e) => {
                        log::warn!("[mb-rtu-port2] read: {}", e);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        });
    health::reset_thread_core();
    result.map_err(|e| crate::error::AppError::Modbus(format!("spawn: {e}")))?;
    STARTED.store(true, Ordering::Release);
    health::register_with_stack(&TASK_HB, crate::safety::stack_budget::MODBUS_RTU_PORT2);

    log::info!(
        "[mb-rtu-port2] started on uart{} addr={} (RS485-3, slave-only)",
        crate::config::pins::RS485_2_UART,
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
    if req.len() < 4 {
        return Ok(());
    }

    let slave = req[0];
    if slave != 0 && slave != addr {
        return Ok(());
    }

    let n = req.len();
    let crc = modbus_crc16(&req[..n - 2]);
    let recv_crc = u16::from_le_bytes([req[n - 2], req[n - 1]]);
    if crc != recv_crc {
        log::debug!(
            "[mb-rtu-port2] crc mismatch: {:#06x}!={:#06x}",
            crc,
            recv_crc
        );
        return Ok(());
    }

    let func = req[1];
    let resp = build_response(backend, slave, func, &req[2..n - 2]);

    if slave == 0 {
        return Ok(());
    }

    port.write(&resp)?;
    Ok(())
}

fn build_response(backend: &BusBackend, slave: u8, func: u8, pdu: &[u8]) -> heapless::Vec<u8, 256> {
    let mut out: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = out.push(slave);
    let mut pdu_buf = [0u8; crate::modbus::shared::PDU_BUF_SIZE];
    let pdu_len = crate::modbus::shared::handle_pdu(backend, func, pdu, &mut pdu_buf);
    if pdu_len >= 2 {
        let _ = out.extend_from_slice(&pdu_buf[..pdu_len]);
    }
    let crc = modbus_crc16(&out);
    let _ = out.push(crc as u8);
    let _ = out.push((crc >> 8) as u8);
    out
}
