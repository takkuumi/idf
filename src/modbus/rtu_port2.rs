//! Modbus RTU 第 3 逻辑端口从站 (UART0 / 板载 E22 透明透传)
//!
//! `RS485-3` 是沿用的配置与协议端口名称；F3 LoRa profile 下，UART0
//! 直接连接板载 E22，由 E22 透明传输 Modbus RTU 字节流，不代表 UART0
//! 物理接了 RS485 收发器。
//!
//! 对齐参考固件 MCA_F16V2_1_F48_BLE 的第三路串口配置 (Serial/UART0):
//! - 默认 9600 8N1, 从站监听模式
//! - 与 RS485 #1 (rtu_slave.rs) 共享同一 Modbus 后端, 仅 UART 不同
//! - 板级 BCT4227 根据 USB VBUS 将 UART0 路由到 USB-UART 或板载 E22
//! - LoRa build profile 关闭控制台，防止日志进入空中数据流
//!
//! 通过 `lora` Cargo feature 启用，并配套 `sdkconfig.lora.defaults`。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::modbus::rtu_port2 as cfg;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::shared::BusBackend;
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
                // LOOP15: UART0 可能与其它任务共享, port.read 期间也须被保护
                health::feed_wdt();
                match port.read(&mut buf, 1000) {
                    Ok(0) => continue,
                    Ok(n) => {
                        if let Err(e) = crate::modbus::rtu_slave::handle_request_stream(
                            &mut port,
                            &backend,
                            &buf[..n],
                            slave_addr,
                            2,
                        ) {
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
