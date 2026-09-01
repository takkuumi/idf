//! Modbus RTU 主站
//!
//! 通过 RS485 #0 (UART1) 轮询外部从站设备。
//!
//! 轮询表直接解析原 MCA 固件的 `SLAVE_DEVICE_CONFIG` (2300+) 变长布局，
//! 响应按每条功能的 `master_addr` 发布到 0..127 或 4000..4223 镜像区。
//! 不得把主站响应写入 `PROTO_BASE`，十进制 4000 与十六进制 0x4000
//! 是两个完全不同的地址域。
//!
//! 本模块**手写 Modbus RTU 帧 + CRC16**, 不依赖 umodbus crate。
//! (umodbus 0.1 API 在 embedded std 环境下不稳定)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::time::Duration;

use crate::config::modbus::rtu_master as cfg;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};
use crate::modbus::shared::modbus_crc16;
use crate::rs485::{Rs485Config, Rs485Port};

/// 任务心跳记录 (静态分配, main_loop 监控)
// 一轮合法组态最多 64 项，每项可等待 5 秒并重试；心跳按进度更新，
// 不能用默认 3 秒停滞阈值误报主站卡死。
static TASK_HB: TaskHb = TaskHb::new_with_stall("mb-rtu-master", 30);
static STARTED: AtomicBool = AtomicBool::new(false);

const MAX_POLL_ITEMS: usize = 64;
const MONITOR_RESULT_COUNT: usize = crate::config::regs::MONITOR_PLC_COUNT as usize;
const USER_RESULT_BASE: u16 = crate::config::regs::HOLD_USER_BASE;
const USER_RESULT_COUNT: usize = crate::config::regs::HOLD_USER_COUNT as usize;
const RESULT_COUNT: usize = MONITOR_RESULT_COUNT + USER_RESULT_COUNT;
const RESULT_MASK_BITS: usize = 32;
const RESULT_MASK_WORDS: usize = RESULT_COUNT.div_ceil(RESULT_MASK_BITS);

/// RS485 主站结果是运行时状态，不应随 20ms 轮询频率写入 NVS。
///
/// 值与有效位分离：先写值，再一次发布有效位掩码。Modbus 读者始终无锁，
/// 且 FC03/FC04 都能看到与 C++ `PRegBuf[master_addr]` 相同的结果。
static RESULT_VALUES: [AtomicU16; RESULT_COUNT] = [const { AtomicU16::new(0) }; RESULT_COUNT];
static RESULT_VALID: [AtomicU32; RESULT_MASK_WORDS] =
    [const { AtomicU32::new(0) }; RESULT_MASK_WORDS];
static RESULT_VERSION: AtomicU32 = AtomicU32::new(0);
static RESULT_WRITERS: AtomicU32 = AtomicU32::new(0);

/// 单条轮询任务
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PollItem {
    /// 组态中的端口号，1 = RS485-1。
    pub(crate) port: u8,
    pub(crate) slave: u8,
    pub(crate) func: u8,
    pub(crate) start: u16,
    pub(crate) count: u16,
    /// 收到数据后写回的主机镜像起始地址。
    pub(crate) dest_reg: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PollConfigSummary {
    pub logic_count: usize,
    pub poll_item_count: usize,
    pub active_port_item_count: usize,
}

/// 读取已配置的主站结果。供 FC03/FC04 寄存器后端调用。
pub fn read_result_reg(addr: u16) -> Option<u16> {
    let index = result_index(addr)?;
    let mask = RESULT_VALID[index / RESULT_MASK_BITS].load(Ordering::Acquire);
    if mask & (1u32 << (index % RESULT_MASK_BITS)) == 0 {
        return None;
    }
    Some(RESULT_VALUES[index].load(Ordering::Acquire))
}

#[inline]
pub fn result_version() -> u32 {
    let writers = RESULT_WRITERS.load(Ordering::Acquire);
    RESULT_VERSION.load(Ordering::Acquire) | u32::from(writers != 0)
}

fn begin_result_write() {
    RESULT_WRITERS.fetch_add(1, Ordering::AcqRel);
}

fn end_result_write() {
    RESULT_VERSION.fetch_add(2, Ordering::Release);
    RESULT_WRITERS.fetch_sub(1, Ordering::Release);
}

/// 批量编码主站镜像。`None` 表示起始地址不属于镜像区，`Some(false)` 表示
/// 地址属于镜像区但有未配置字或写入并发过于频繁。
pub fn encode_result_regs_be(addr: u16, count: u16, out: &mut [u8]) -> Option<bool> {
    let start = result_index(addr)?;
    if count == 0 || out.len() != count as usize * 2 || !result_range_is_valid(addr, count) {
        return Some(false);
    }
    for attempt in 0..4 {
        let before = result_version();
        if before & 1 != 0 {
            if attempt < 3 {
                std::hint::spin_loop();
                continue;
            }
            return Some(false);
        }
        for offset in 0..count as usize {
            let index = start + offset;
            let mask = RESULT_VALID[index / RESULT_MASK_BITS].load(Ordering::Acquire);
            if mask & (1u32 << (index % RESULT_MASK_BITS)) == 0 {
                return Some(false);
            }
            out[offset * 2..offset * 2 + 2]
                .copy_from_slice(&RESULT_VALUES[index].load(Ordering::Acquire).to_be_bytes());
        }
        if result_version() == before {
            return Some(true);
        }
    }
    Some(false)
}

fn result_index(addr: u16) -> Option<usize> {
    if (addr as usize) < MONITOR_RESULT_COUNT {
        return Some(addr as usize);
    }
    let user_index = addr.checked_sub(USER_RESULT_BASE)? as usize;
    (user_index < USER_RESULT_COUNT).then_some(MONITOR_RESULT_COUNT + user_index)
}

fn result_range_is_valid(start: u16, count: u16) -> bool {
    if count == 0 {
        return false;
    }
    let Some(last) = start.checked_add(count - 1) else {
        return false;
    };
    match (result_index(start), result_index(last)) {
        (Some(first), Some(last)) => last - first + 1 == count as usize,
        _ => false,
    }
}

pub(crate) fn publish_result_ranges(items: &[PollItem]) {
    begin_result_write();
    for value in &RESULT_VALUES {
        value.store(0, Ordering::Release);
    }
    let mut masks = [0u32; RESULT_MASK_WORDS];
    for item in items {
        let start = result_index(item.dest_reg).expect("validated poll result address");
        for index in start..start + item.count as usize {
            masks[index / RESULT_MASK_BITS] |= 1u32 << (index % RESULT_MASK_BITS);
        }
    }
    for (target, mask) in RESULT_VALID.iter().zip(masks) {
        target.store(mask, Ordering::Release);
    }
    end_result_write();
}

fn write_result(item: PollItem, values: &[u16]) {
    begin_result_write();
    let start = result_index(item.dest_reg).expect("validated poll result address");
    for (offset, value) in values.iter().copied().enumerate() {
        if offset >= item.count as usize {
            break;
        }
        RESULT_VALUES[start + offset].store(value, Ordering::Release);
    }
    end_result_write();
}

pub(crate) fn clear_result(item: PollItem) {
    begin_result_write();
    let start = result_index(item.dest_reg).expect("validated poll result address");
    for value in &RESULT_VALUES[start..start + item.count as usize] {
        value.store(0, Ordering::Release);
    }
    end_result_write();
}

fn holding_word(words: &[u16], addr: u16) -> Option<u16> {
    let index = addr.checked_sub(crate::config::regs::HOLD_PXX_BASE)? as usize;
    words.get(index).copied()
}

/// 解析 `sys-cmi::logic::write_logics` 与原 C++ `MODMRTU_Poll` 共用的布局。
fn parse_poll_table(
    words: &[u16],
) -> Result<heapless::Vec<PollItem, MAX_POLL_ITEMS>, &'static str> {
    let base = crate::config::regs::HOLD_DEVICE_CONFIG;
    let logic_count = holding_word(words, base).ok_or("missing logic count")? as usize;
    let mut items = heapless::Vec::new();
    if logic_count == 0 {
        return Ok(items);
    }
    let available_words = words
        .len()
        .saturating_sub((base - crate::config::regs::HOLD_PXX_BASE) as usize);
    if logic_count > u16::MAX as usize - 2 || logic_count + 2 > available_words {
        return Err("logic offset table too large");
    }

    let total_offset_addr = base
        .checked_add(logic_count as u16 + 1)
        .ok_or("logic offset overflow")?;
    let total_offset = holding_word(words, total_offset_addr).ok_or("missing logic end offset")?;
    let total_end = base.checked_add(total_offset).ok_or("logic end overflow")?;
    if total_offset < logic_count as u16 + 2 || total_end > crate::config::regs::HOLD_USER_BASE {
        return Err("invalid logic end offset");
    }

    for logic_index in 0..logic_count {
        let offset_addr = base + 1 + logic_index as u16;
        let start_offset = holding_word(words, offset_addr).ok_or("missing logic offset")?;
        let end_offset = if logic_index + 1 < logic_count {
            holding_word(words, offset_addr + 1).ok_or("missing next logic offset")?
        } else {
            total_offset
        };
        if start_offset >= end_offset {
            return Err("unordered logic offsets");
        }
        let start = base
            .checked_add(start_offset)
            .ok_or("logic start overflow")?;
        let end = base.checked_add(end_offset).ok_or("logic end overflow")?;
        if end > total_end || end - start < 6 {
            return Err("truncated logic header");
        }

        let attribute = (holding_word(words, start).ok_or("missing logic attribute")? & 0xff) as u8;
        if attribute != 2 && attribute != 4 {
            continue;
        }
        let [port, slave] = holding_word(words, start + 4)
            .ok_or("missing port/slave")?
            .to_be_bytes();
        if !(1..=3).contains(&port) || !(1..=247).contains(&slave) {
            return Err("invalid port or slave id");
        }
        let [function_bytes, function_count] = holding_word(words, start + 5)
            .ok_or("missing function header")?
            .to_be_bytes();
        // 参考固件和 sys-cmi 都把 function_bytes 定义为功能码、从机地址、
        // 主机地址三项共 6 字节；随后的 register header 是独立 1 word。
        if function_bytes != 6 {
            return Err("invalid function header size");
        }

        let mut cursor = start + 6;
        for _ in 0..function_count {
            if cursor.checked_add(4).is_none_or(|next| next > end) {
                return Err("truncated function");
            }
            let func_word = holding_word(words, cursor).ok_or("missing function code")?;
            let func = u8::try_from(func_word).map_err(|_| "invalid function code")?;
            if !(1..=4).contains(&func) {
                return Err("unsupported read function");
            }
            let start_addr = holding_word(words, cursor + 1).ok_or("missing slave address")?;
            let dest_reg = holding_word(words, cursor + 2).ok_or("missing master address")?;
            let [register_bytes, register_count] = holding_word(words, cursor + 3)
                .ok_or("missing register header")?
                .to_be_bytes();
            if register_bytes == 0 || !register_bytes.is_multiple_of(2) || register_count == 0 {
                return Err("invalid register layout");
            }
            let count = register_count as u16;
            let max_count = if func <= 2 { 2000 } else { 125 };
            if count > max_count {
                return Err("modbus quantity out of range");
            }
            if !result_range_is_valid(dest_reg, count) {
                return Err("master result outside monitor/user areas");
            }

            items
                .push(PollItem {
                    port,
                    slave,
                    func,
                    start: start_addr,
                    count,
                    dest_reg,
                })
                .map_err(|_| "too many poll items")?;

            let register_words = (register_bytes as u16 / 2)
                .checked_mul(count)
                .ok_or("register layout overflow")?;
            cursor = cursor
                .checked_add(4 + register_words)
                .ok_or("function offset overflow")?;
            if cursor > end {
                return Err("register metadata exceeds logic record");
            }
        }
    }
    Ok(items)
}

pub(crate) fn validate_poll_config(words: &[u16]) -> Result<PollConfigSummary, &'static str> {
    let logic_count = holding_word(words, crate::config::regs::HOLD_DEVICE_CONFIG)
        .ok_or("missing logic count")? as usize;
    let items = parse_poll_table(words)?;
    Ok(PollConfigSummary {
        logic_count,
        poll_item_count: items.len(),
        active_port_item_count: items.iter().filter(|item| item.port == 1).count(),
    })
}

pub(crate) fn load_poll_table() -> Result<heapless::Vec<PollItem, MAX_POLL_ITEMS>, &'static str> {
    let storage = crate::bus::storage_state::storage_read().ok_or("storage unavailable")?;
    parse_poll_table(&storage.holding_buf)
}

pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    let initial_table = load_poll_table().unwrap_or_else(|error| {
        log::warn!("[mb-rtu-master] invalid initial device configuration: {error}");
        heapless::Vec::new()
    });
    if initial_table.is_empty() {
        log::warn!(
            "[mb-rtu-master] no configured RS485 poll items; 4001/4005 remain unavailable until project sync writes 2300+"
        );
    }
    let unsupported_ports = initial_table.iter().filter(|item| item.port != 1).count();
    if unsupported_ports > 0 {
        log::warn!(
            "[mb-rtu-master] ignored {unsupported_ports} poll items on RS485-2/3; only RS485-1 master is active"
        );
    }
    publish_result_ranges(&initial_table);

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
            let mut poll_table = initial_table;
            loop {
                // 心跳: 每轮询周期一次
                TASK_HB.tick();
                // LOOP15: 必须喂 WDT, 重试 + 轮询周期累加可能 > 10s 触发复位
                crate::health::feed_wdt();
                match load_poll_table() {
                    Ok(updated) if updated != poll_table => {
                        publish_result_ranges(&updated);
                        poll_table = updated;
                        let unsupported_ports =
                            poll_table.iter().filter(|item| item.port != 1).count();
                        log::info!(
                            "[mb-rtu-master] loaded {} configured poll items",
                            poll_table.len()
                        );
                        if unsupported_ports > 0 {
                            log::warn!(
                                "[mb-rtu-master] ignored {unsupported_ports} poll items on RS485-2/3; only RS485-1 master is active"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(error) if last_warn.elapsed() >= Duration::from_secs(10) => {
                        log::warn!("[mb-rtu-master] ignored incomplete configuration: {error}");
                        last_warn = std::time::Instant::now();
                    }
                    Err(_) => {}
                }
                for item in poll_table.iter().copied().filter(|item| item.port == 1) {
                    TASK_HB.tick();
                    if let Err(e) = poll_with_retry(&mut port, item, max_retry, timeout_ms) {
                        // 对齐 C++ Modbus_Clear_Result：重试耗尽或异常响应后清零，
                        // 禁止把断线前的旧值继续当作当前测量值。
                        clear_result(item);
                        suppressed_warns = suppressed_warns.saturating_add(1);
                        if last_warn.elapsed() >= Duration::from_secs(10) {
                            log::warn!(
                                "[mb-rtu-master] poll slave={} fc={:02x} failed after {} retries: {} (suppressed={})",
                                item.slave, item.func,
                                max_retry,
                                e,
                                suppressed_warns.saturating_sub(1)
                            );
                            last_warn = std::time::Instant::now();
                            suppressed_warns = 0;
                        }
                    }
                    TASK_HB.tick();
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
pub(crate) fn poll_with_retry(
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
    let stats_port = usize::from(item.port.saturating_sub(1));
    let req = build_request(item);
    let resp = match port.send_recv(&req, timeout_ms) {
        Ok(r) => r,
        Err(_) => {
            // LOOP14: 主站通信错误 (超时) → 置 RS485_1_COMERR=0x04
            crate::modbus::shared::RS485_STATS.inc_port_comerr(stats_port);
            return Err(crate::error::AppError::Modbus("timeout".into()));
        }
    };

    if resp.is_empty() {
        // send_recv 返回空帧表示在 deadline 内没有收到首字节，语义是超时而非
        // 从站返回了一个格式错误的短帧；两者分别计入 COMERR/APPERR。
        crate::modbus::shared::RS485_STATS.inc_port_comerr(stats_port);
        return Err(crate::error::AppError::Modbus(
            "timeout: empty response".into(),
        ));
    }
    if resp.len() < 5 {
        // LOOP14: 帧太短 (通信错误) → 置 COMERR=0x04
        crate::modbus::shared::RS485_STATS.inc_port_comerr(stats_port);
        return Err(crate::error::AppError::Modbus(format!(
            "short resp: {} bytes",
            resp.len()
        )));
    }

    // 校验从站地址
    if resp[0] != item.slave {
        // LOOP14: 从站地址不匹配 (通信错误) → 置 COMERR=0x04
        crate::modbus::shared::RS485_STATS.inc_port_comerr(stats_port);
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
        // LOOP14: CRC 校验失败 (通信错误) → 置 COMERR=0x04
        crate::modbus::shared::RS485_STATS.inc_port_comerr(stats_port);
        return Err(crate::error::AppError::Modbus(format!(
            "crc mismatch: {:#06x}!={:#06x}",
            crc, recv_crc
        )));
    }

    // 异常响应必须对应本次请求，且标准长度固定为 5 字节。
    if resp[1] == (item.func | 0x80) {
        if n != 5 {
            crate::modbus::shared::RS485_STATS.inc_port_comerr(stats_port);
            return Err(crate::error::AppError::Modbus(format!(
                "invalid exception length: {n}"
            )));
        }
        // 异常响应本身证明链路有效：清除通信错误并记录从站返回的
        // Modbus 异常码，和旧 MCA 的 Modbus_App_Error 语义一致。
        crate::modbus::shared::RS485_STATS.mark_port_ok(stats_port);
        crate::modbus::shared::RS485_STATS.set_port_apperr(stats_port, resp[2]);
        return Err(crate::error::AppError::Modbus(format!(
            "exception: {:02x}",
            resp[2]
        )));
    }
    if resp[1] != item.func {
        crate::modbus::shared::RS485_STATS.inc_port_comerr(stats_port);
        return Err(crate::error::AppError::Modbus(format!(
            "function mismatch: {:02x}!={:02x}",
            resp[1], item.func
        )));
    }

    // 地址、CRC 和功能码均正确，说明物理链路已恢复。后续 payload
    // 解析失败属于应用层错误，不应继续保留旧的通信错误状态。
    crate::modbus::shared::RS485_STATS.mark_port_ok(stats_port);

    // 解析寄存器数据 (FC=03/04) 并写回 bus
    if item.func == 0x03 || item.func == 0x04 {
        let data = register_data(&resp, item).inspect_err(|_| {
            crate::modbus::shared::RS485_STATS.inc_port_apperr(stats_port);
        })?;
        // Modbus RTU FC=03/04 单帧最多 125 reg, 用 heapless::Vec 避免 heap 分配
        let mut regs: heapless::Vec<u16, 128> = heapless::Vec::new();
        for word in data.chunks_exact(2) {
            regs.push(u16::from_be_bytes([word[0], word[1]]))
                .map_err(|_| {
                    crate::error::AppError::Modbus("register response too large".into())
                })?;
        }
        log::debug!(
            "[mb-rtu-master] slave={} fc={:02x} regs={:?}",
            item.slave,
            item.func,
            regs
        );
        write_result(item, &regs);
    } else if item.func == 0x01 || item.func == 0x02 {
        let data = response_data(&resp, item).inspect_err(|_| {
            crate::modbus::shared::RS485_STATS.inc_port_apperr(stats_port);
        })?;
        // 对齐 C++ BEBufToUint16：位响应按相邻两个数据字节组成一个结果字，
        // 奇数字节在低位补 0。未覆盖的目标字保留，失败路径才整体清零。
        let mut words: heapless::Vec<u16, 128> = heapless::Vec::new();
        for pair in data.chunks(2) {
            let value = u16::from_be_bytes([pair[0], pair.get(1).copied().unwrap_or(0)]);
            words
                .push(value)
                .map_err(|_| crate::error::AppError::Modbus("bit response too large".into()))?;
        }
        write_result(item, &words);
    }

    crate::modbus::shared::RS485_STATS.mark_port_ok(stats_port);

    Ok(())
}

fn register_data(resp: &[u8], item: PollItem) -> AppResult<&[u8]> {
    response_data(resp, item)
}

fn response_data(resp: &[u8], item: PollItem) -> AppResult<&[u8]> {
    let Some(&declared) = resp.get(2) else {
        return Err(crate::error::AppError::Modbus(
            "register response missing byte count".into(),
        ));
    };
    let byte_count = declared as usize;
    let expected = if item.func <= 2 {
        (item.count as usize).div_ceil(8)
    } else {
        item.count as usize * 2
    };
    if byte_count != expected || resp.len() != byte_count + 5 {
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
        port: 1,
        slave: 1,
        func: 0x03,
        start: 0,
        count: 2,
        dest_reg: 4001,
    };

    fn configured_words() -> [u16; crate::config::regs::HOLD_PXX_COUNT] {
        let mut words = [0u16; crate::config::regs::HOLD_PXX_COUNT];
        let index = |addr: u16| (addr - crate::config::regs::HOLD_PXX_BASE) as usize;
        let base = crate::config::regs::HOLD_DEVICE_CONFIG;

        // write_logics: [count, first_offset, total_end, logic...]
        words[index(base)] = 1;
        words[index(base + 1)] = 3;
        words[index(base + 2)] = 37;

        let logic = base + 3;
        words[index(logic)] = 0x0002; // index=0, attribute=RS485
        words[index(logic + 1)] = 0; // icon
        words[index(logic + 2)] = 0; // label
        words[index(logic + 3)] = 0; // name
        words[index(logic + 4)] = 0x0111; // RS485-1, slave 17
        words[index(logic + 5)] = 0x0604; // function header: 6 bytes, 4 entries

        words[index(logic + 6)] = 4;
        words[index(logic + 7)] = 4116;
        words[index(logic + 8)] = 4001;
        words[index(logic + 9)] = 0x0402;
        words[index(logic + 10)] = 100;
        words[index(logic + 11)] = 4116;
        words[index(logic + 12)] = 102;
        words[index(logic + 13)] = 4117;

        words[index(logic + 14)] = 4;
        words[index(logic + 15)] = 4096;
        words[index(logic + 16)] = 3;
        words[index(logic + 17)] = 0x0401;
        words[index(logic + 18)] = 104;
        words[index(logic + 19)] = 4096;

        words[index(logic + 20)] = 4;
        words[index(logic + 21)] = 4096;
        words[index(logic + 22)] = 4;
        words[index(logic + 23)] = 0x0401;
        words[index(logic + 24)] = 106;
        words[index(logic + 25)] = 4096;

        words[index(logic + 26)] = 4;
        words[index(logic + 27)] = 4146;
        words[index(logic + 28)] = 4005;
        words[index(logic + 29)] = 0x0402;
        words[index(logic + 30)] = 108;
        words[index(logic + 31)] = 4146;
        words[index(logic + 32)] = 110;
        words[index(logic + 33)] = 4147;
        words
    }

    #[test]
    fn test_parse_real_layout_maps_4001_and_4005() {
        let items = parse_poll_table(&configured_words()).expect("valid C++/sys-cmi layout");
        assert_eq!(
            items.as_slice(),
            &[
                PollItem {
                    port: 1,
                    slave: 17,
                    func: 4,
                    start: 4116,
                    count: 2,
                    dest_reg: 4001,
                },
                PollItem {
                    port: 1,
                    slave: 17,
                    func: 4,
                    start: 4096,
                    count: 1,
                    dest_reg: 3,
                },
                PollItem {
                    port: 1,
                    slave: 17,
                    func: 4,
                    start: 4096,
                    count: 1,
                    dest_reg: 4,
                },
                PollItem {
                    port: 1,
                    slave: 17,
                    func: 4,
                    start: 4146,
                    count: 2,
                    dest_reg: 4005,
                },
            ]
        );
    }

    #[test]
    fn test_parse_rejects_result_range_overflow() {
        let mut words = configured_words();
        let logic = crate::config::regs::HOLD_DEVICE_CONFIG + 3;
        let index = (logic + 28 - crate::config::regs::HOLD_PXX_BASE) as usize;
        words[index] =
            crate::config::regs::HOLD_USER_BASE + crate::config::regs::HOLD_USER_COUNT - 1;
        assert_eq!(
            parse_poll_table(&words),
            Err("master result outside monitor/user areas")
        );
    }

    #[test]
    fn test_result_publish_write_and_failure_clear() {
        let items = parse_poll_table(&configured_words()).unwrap();
        publish_result_ranges(&items);
        write_result(items[0], &[0x1234, 0x5678]);
        assert_eq!(read_result_reg(4001), Some(0x1234));
        assert_eq!(read_result_reg(4002), Some(0x5678));
        assert_eq!(read_result_reg(4003), None);
        let mut encoded = [0u8; 4];
        assert_eq!(encode_result_regs_be(4001, 2, &mut encoded), Some(true));
        assert_eq!(encoded, [0x12, 0x34, 0x56, 0x78]);
        write_result(items[1], &[0xabcd]);
        assert_eq!(read_result_reg(3), Some(0xabcd));

        clear_result(items[0]);
        assert_eq!(read_result_reg(4001), Some(0));
        assert_eq!(read_result_reg(4002), Some(0));
    }

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
