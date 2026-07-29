//! Modbus RTU 主站
//!
//! 通过 RS485 #0 (UART1) 轮询外部从站设备。
//! 轮询表硬编码为示例: 从站 1, FC=03, 起始地址 0, 数量 8。
//! 实际项目中应从 NVS 加载用户配置。
//!
//! 本模块**手写 Modbus RTU 帧 + CRC16**, 不依赖 umodbus crate。
//! (umodbus 0.1 API 在 embedded std 环境下不稳定)

use std::sync::Arc;
use std::time::Duration;

use crate::config::modbus::rtu_master as cfg;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::shared::modbus_crc16;
use crate::rs485::{Rs485Config, Rs485Port};

/// 失败重试次数 (不含首次)
const MAX_RETRY: u32 = 0; // 无从站时不重试, 避免长时间阻塞

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("mb-rtu-master");

/// 单条轮询任务
#[derive(Clone, Copy)]
struct PollItem {
    slave: u8,
    func: u8,
    start: u16,
    count: u16,
    /// 收到数据后写回 bus 的目标起始寄存器地址 (None 表示不写回)
    dest_reg: Option<u16>,
}

/// 默认轮询表
const POLL_TABLE: &[PollItem] = &[
    PollItem {
        slave: 1,
        func: 0x03,
        start: 0,
        count: 8,
        dest_reg: Some(crate::config::regs::PROTO_BASE),
    },
    // TODO: 从 NVS 加载用户配置的轮询表
];

pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    let port_cfg = Rs485Config::from_rtu_master();
    let mut port = Rs485Port::open(&port_cfg)?;

    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mb-rtu-master".into())
        .stack_size(crate::safety::stack_budget::MODBUS_RTU_MASTER)
        .spawn(move || {
            // LOOP14: 所有长期运行 pthread 必须订阅 WDT
            health::subscribe_wdt();
            loop {
                // 心跳: 每轮询周期一次
                TASK_HB.tick();
                // LOOP15: 必须喂 WDT, 重试 + 轮询周期累加可能 > 10s 触发复位
                crate::health::feed_wdt();
                for item in POLL_TABLE {
                    if let Err(e) = poll_with_retry(&mut port, *item) {
                        log::warn!(
                            "[mb-rtu-master] poll slave={} fc={:02x} failed after {} retries: {}",
                            item.slave, item.func, MAX_RETRY, e
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(cfg::POLL_INTERVAL_MS));
            }
        });
    health::reset_thread_core();
    result.map_err(|e| crate::error::AppError::Modbus(format!("spawn: {e}")))?;
    health::register_with_stack(&TASK_HB, crate::safety::stack_budget::MODBUS_RTU_MASTER);

    log::info!("[mb-rtu-master] started on uart{}", cfg::UART_PORT);
    Ok(())
}

/// 带重试的轮询: 失败重试 MAX_RETRY 次, 间隔 100ms
fn poll_with_retry(port: &mut Rs485Port, item: PollItem) -> AppResult<()> {
    let mut last_err: Option<crate::error::AppError> = None;
    for attempt in 0..=MAX_RETRY {
        match poll_once(port, item) {
            Ok(()) => return Ok(()),
            Err(e) => {
                log::debug!(
                    "[mb-rtu-master] attempt {} failed: {}",
                    attempt + 1, e
                );
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| crate::error::AppError::Modbus("unknown".into())))
}

fn poll_once(port: &mut Rs485Port, item: PollItem) -> AppResult<()> {
    let req = build_request(item);
    let resp = match port.send_recv(&req, cfg::TIMEOUT_MS) {
        Ok(r) => r,
        Err(_) => {
            // LOOP14: 主站通信错误 (超时) → 累加 RS485_1_COMERR
            crate::modbus::shared::RS485_STATS.inc_master_comerr();
            return Err(crate::error::AppError::Modbus("timeout".into()));
        }
    };

    if resp.len() < 5 {
        // LOOP14: 帧太短 (通信错误) → 累加 COMERR
        crate::modbus::shared::RS485_STATS.inc_master_comerr();
        return Err(crate::error::AppError::Modbus(format!("short resp: {} bytes", resp.len())));
    }

    // 校验从站地址
    if resp[0] != item.slave {
        // LOOP14: 从站地址不匹配 (通信错误) → 累加 COMERR
        crate::modbus::shared::RS485_STATS.inc_master_comerr();
        return Err(crate::error::AppError::Modbus(format!("slave mismatch: {}!={}", resp[0], item.slave)));
    }

    // 异常响应 (Modbus spec: func | 0x80, code: Illegal F/R/V/Slave Failure)
    if resp[1] & 0x80 != 0 {
        // LOOP14: 异常响应 → 累加 APPERR
        crate::modbus::shared::RS485_STATS.inc_master_apperr();
        return Err(crate::error::AppError::Modbus(format!("exception: {:02x}", resp[2])));
    }

    // CRC 校验
    let n = resp.len();
    let crc = modbus_crc16(&resp[..n - 2]);
    let recv_crc = u16::from_le_bytes([resp[n - 2], resp[n - 1]]);
    if crc != recv_crc {
        // LOOP14: CRC 校验失败 (通信错误) → 累加 COMERR
        crate::modbus::shared::RS485_STATS.inc_master_comerr();
        return Err(crate::error::AppError::Modbus(format!("crc mismatch: {:#06x}!={:#06x}", crc, recv_crc)));
    }

    // 解析寄存器数据 (FC=03/04) 并写回 bus
    if item.func == 0x03 || item.func == 0x04 {
        let byte_count = resp[2] as usize;
        if byte_count + 5 != n {
            log::warn!("[mb-rtu-master] byte_count {} != payload {}", byte_count, n - 5);
        }
        // Modbus RTU FC=03/04 单帧最多 125 reg, 用 heapless::Vec 避免 heap 分配
        let mut regs: heapless::Vec<u16, 128> = heapless::Vec::new();
        for i in 0..byte_count / 2 {
            let v = u16::from_be_bytes([resp[3 + 2 * i], resp[4 + 2 * i]]);
            if regs.push(v).is_err() {
                log::warn!("[mb-rtu-master] regs overflow at {}, byte_count={}", i, byte_count);
                break;
            }
        }
        log::debug!("[mb-rtu-master] slave={} fc=03 regs={:?}", item.slave, regs);

        // 写回 bus 协议存储区 (如果指定了 dest_reg)
        if let Some(dest_start) = item.dest_reg {
            let mut written = 0u16;
            for (i, &v) in regs.iter().enumerate() {
                let addr = dest_start.wrapping_add(i as u16);
                if (addr >= crate::config::regs::PROTO_BASE)
                    && (addr < crate::config::regs::PROTO_END)
                {
                    let idx = (addr - crate::config::regs::PROTO_BASE) as usize;
                    // 阶段 B: 无锁 RMW — clone snap → 修改 proto.data[idx] → Rcu::write
                    crate::bus::backends::storage_modify(|snap| {
                        snap.proto.data[idx] = v;
                        snap.proto.dirty = true;
                        snap.proto.status = crate::bus::proto_status();
                    });
                    written += 1;
                }
            }
            if written > 0 {
                log::debug!(
                    "[mb-rtu-master] wrote {} regs to RCU storage at {:#06X}",
                    written, dest_start
                );
            }
        }
    }

    Ok(())
}

/// 构造 RTU 请求帧: [slave, func, start_hi, start_lo, count_hi, count_lo, crc_lo, crc_hi]
fn build_request(item: PollItem) -> heapless::Vec<u8, 16> {
    let mut req = heapless::Vec::new();
    let _ = req.push(item.slave);
    let _ = req.push(item.func);
    let _ = req.extend_from_slice(&item.start.to_be_bytes());
    let _ = req.extend_from_slice(&item.count.to_be_bytes());
    let crc = modbus_crc16(&req);
    let _ = req.extend_from_slice(&crc.to_le_bytes());
    req
}
