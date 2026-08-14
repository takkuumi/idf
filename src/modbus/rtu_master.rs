//! Modbus RTU 主站
//!
//! 通过 RS485 #0 (UART1) 轮询外部从站设备。
//! 轮询表硬编码为示例: 从站 1, FC=03, 起始地址 0, 数量 8。
//! 实际项目中应从 NVS 加载用户配置。
//!
//! 本模块**手写 Modbus RTU 帧 + CRC16**, 不依赖 umodbus crate。
//! (umodbus 0.1 API 在 embedded std 环境下不稳定)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::modbus::rtu_master as cfg;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::shared::modbus_crc16;
use crate::rs485::{Rs485Config, Rs485Port};

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("mb-rtu-master");
static STARTED: AtomicBool = AtomicBool::new(false);

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
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    let port_cfg = Rs485Config::from_rtu_master();
    let mut port = Rs485Port::open(&port_cfg)?;
    let (max_retry, timeout_ms, poll_interval_ms) =
        crate::bus::config_state::config_read_with(|state| {
            let saved = &state.cfg.rs485[0];
            (
                saved.retry_count.min(5) as u32,
                saved.timeout_ms.clamp(20, 5000) as u64,
                saved.interval_ms.clamp(20, 5000) as u64,
            )
        })
        .unwrap_or((0, cfg::TIMEOUT_MS, cfg::POLL_INTERVAL_MS));

    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mb-rtu-master".into())
        .stack_size(crate::safety::stack_budget::MODBUS_RTU_MASTER)
        .spawn(move || {
            // LOOP14: 所有长期运行 pthread 必须订阅 WDT
            health::subscribe_wdt();
            let mut last_warn = std::time::Instant::now() - Duration::from_secs(10);
            let mut suppressed_warns = 0u32;
            loop {
                // 心跳: 每轮询周期一次
                TASK_HB.tick();
                // LOOP15: 必须喂 WDT, 重试 + 轮询周期累加可能 > 10s 触发复位
                crate::health::feed_wdt();
                for item in POLL_TABLE {
                    if let Err(e) = poll_with_retry(&mut port, *item, max_retry, timeout_ms) {
                        suppressed_warns = suppressed_warns.saturating_add(1);
                        if last_warn.elapsed() >= Duration::from_secs(10) {
                            log::warn!(
                                "[mb-rtu-master] poll slave={} fc={:02x} failed after {} retries: {} (suppressed={})",
                                item.slave,
                                item.func,
                                max_retry,
                                e,
                                suppressed_warns.saturating_sub(1)
                            );
                            last_warn = std::time::Instant::now();
                            suppressed_warns = 0;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(poll_interval_ms));
            }
        });
    health::reset_thread_core();
    result.map_err(|e| crate::error::AppError::Modbus(format!("spawn: {e}")))?;
    STARTED.store(true, Ordering::Release);
    health::register_with_stack(&TASK_HB, crate::safety::stack_budget::MODBUS_RTU_MASTER);

    log::info!("[mb-rtu-master] started on uart{}", cfg::UART_PORT);
    Ok(())
}

/// 带重试的轮询: 失败重试 MAX_RETRY 次, 间隔 100ms
fn poll_with_retry(
    port: &mut Rs485Port,
    item: PollItem,
    max_retry: u32,
    timeout_ms: u64,
) -> AppResult<()> {
    let mut last_err: Option<crate::error::AppError> = None;
    for attempt in 0..=max_retry {
        // 单次超时最多 5s，最多 6 次尝试。每次尝试前喂狗，断线重试不能累积成
        // 30s 无喂狗窗口并造成非计划重启。
        crate::health::feed_wdt();
        match poll_once(port, item, timeout_ms) {
            Ok(()) => return Ok(()),
            Err(e) => {
                log::debug!("[mb-rtu-master] attempt {} failed: {}", attempt + 1, e);
                last_err = Some(e);
                if attempt < max_retry {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| crate::error::AppError::Modbus("unknown".into())))
}

fn poll_once(port: &mut Rs485Port, item: PollItem, timeout_ms: u64) -> AppResult<()> {
    let req = build_request(item);
    let resp = match port.send_recv(&req, timeout_ms) {
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
        return Err(crate::error::AppError::Modbus(format!(
            "short resp: {} bytes",
            resp.len()
        )));
    }

    // 校验从站地址
    if resp[0] != item.slave {
        // LOOP14: 从站地址不匹配 (通信错误) → 累加 COMERR
        crate::modbus::shared::RS485_STATS.inc_master_comerr();
        return Err(crate::error::AppError::Modbus(format!(
            "slave mismatch: {}!={}",
            resp[0], item.slave
        )));
    }

    // 在解释功能码和 payload 前先校验整帧 CRC。
    let n = resp.len();
    let crc = modbus_crc16(&resp[..n - 2]);
    let recv_crc = u16::from_le_bytes([resp[n - 2], resp[n - 1]]);
    if crc != recv_crc {
        // LOOP14: CRC 校验失败 (通信错误) → 累加 COMERR
        crate::modbus::shared::RS485_STATS.inc_master_comerr();
        return Err(crate::error::AppError::Modbus(format!(
            "crc mismatch: {:#06x}!={:#06x}",
            crc, recv_crc
        )));
    }

    // 异常响应必须对应本次请求，且标准长度固定为 5 字节。
    if resp[1] == (item.func | 0x80) {
        if n != 5 {
            crate::modbus::shared::RS485_STATS.inc_master_comerr();
            return Err(crate::error::AppError::Modbus(format!(
                "invalid exception length: {n}"
            )));
        }
        crate::modbus::shared::RS485_STATS.inc_master_apperr();
        return Err(crate::error::AppError::Modbus(format!(
            "exception: {:02x}",
            resp[2]
        )));
    }
    if resp[1] != item.func {
        crate::modbus::shared::RS485_STATS.inc_master_comerr();
        return Err(crate::error::AppError::Modbus(format!(
            "function mismatch: {:02x}!={:02x}",
            resp[1], item.func
        )));
    }

    // 解析寄存器数据 (FC=03/04) 并写回 bus
    if item.func == 0x03 || item.func == 0x04 {
        let data = register_data(&resp, item).inspect_err(|_| {
            crate::modbus::shared::RS485_STATS.inc_master_apperr();
        })?;
        // Modbus RTU FC=03/04 单帧最多 125 reg, 用 heapless::Vec 避免 heap 分配
        let mut regs: heapless::Vec<u16, 128> = heapless::Vec::new();
        for word in data.chunks_exact(2) {
            regs.push(u16::from_be_bytes([word[0], word[1]]))
                .map_err(|_| {
                    crate::error::AppError::Modbus("register response too large".into())
                })?;
        }
        log::debug!("[mb-rtu-master] slave={} fc=03 regs={:?}", item.slave, regs);

        // 写回 bus 协议存储区 (如果指定了 dest_reg)
        if let Some(dest_start) = item.dest_reg {
            let start = dest_start.saturating_sub(crate::config::regs::PROTO_BASE) as usize;
            let written = if dest_start >= crate::config::regs::PROTO_BASE
                && start < crate::config::regs::PROTO_COUNT as usize
            {
                let count = regs
                    .len()
                    .min(crate::config::regs::PROTO_COUNT as usize - start);
                // 整帧只发布一次，禁止逐寄存器 COW 造成持续 3KB heap 抖动。
                crate::bus::backends::storage_modify(|snap| {
                    let data = std::sync::Arc::make_mut(&mut snap.proto.data);
                    data[start..start + count].copy_from_slice(&regs[..count]);
                    snap.proto.dirty = true;
                    snap.proto.status = crate::bus::proto_status();
                });
                count as u16
            } else {
                0
            };
            if written > 0 {
                log::debug!(
                    "[mb-rtu-master] wrote {} regs to RCU storage at {:#06X}",
                    written,
                    dest_start
                );
            }
        }
    }

    crate::modbus::shared::RS485_STATS.mark_master_ok();

    Ok(())
}

fn register_data(resp: &[u8], item: PollItem) -> AppResult<&[u8]> {
    let Some(&declared) = resp.get(2) else {
        return Err(crate::error::AppError::Modbus(
            "register response missing byte count".into(),
        ));
    };
    let byte_count = declared as usize;
    let expected = item.count as usize * 2;
    if byte_count != expected || !byte_count.is_multiple_of(2) || resp.len() != byte_count + 5 {
        return Err(crate::error::AppError::Modbus(format!(
            "invalid register payload: declared={byte_count}, expected={expected}, frame={}",
            resp.len()
        )));
    }
    Ok(&resp[3..3 + byte_count])
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

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: PollItem = PollItem {
        slave: 1,
        func: 0x03,
        start: 0,
        count: 2,
        dest_reg: None,
    };

    #[test]
    fn test_register_data_rejects_declared_length_larger_than_frame() {
        let response = [1, 3, 250, 0, 0];
        assert!(register_data(&response, ITEM).is_err());
    }

    #[test]
    fn test_register_data_requires_requested_word_count() {
        let response = [1, 3, 2, 0x12, 0x34, 0, 0];
        assert!(register_data(&response, ITEM).is_err());
        let response = [1, 3, 4, 0x12, 0x34, 0x56, 0x78, 0, 0];
        assert_eq!(
            register_data(&response, ITEM).unwrap(),
            &[0x12, 0x34, 0x56, 0x78]
        );
    }
}
