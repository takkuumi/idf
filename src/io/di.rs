//! DI 数字输入扫描任务
//!
//! - 默认版本: 周期 1ms 读取 8 路 GPIO DI (光耦隔离输入)
//! - F3/F4 版本: 周期 1ms 读取 16/48 路 I2C MCP23017 扩展 DI
use std::sync::Arc;

use crate::config::hw_version;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::TaskHb;
use crate::sync::MainLoopCell;

/// 扫描周期 (ms)
/// DI 扫描周期:
/// - F16 (PCA9555 软件 I2C): 5ms — 留足 I2C 时间, 避免阻塞网络中断
///   (读 2 端口 + 写 2 LED ≈ 4×100μs @250kHz, < 5% CPU)
/// - F3/F4 (I2C MCP23017): 5ms 同理
const SCAN_PERIOD_MS: u64 = 5;
/// 去抖需要的连续相同采样次数
const DEBOUNCE_COUNT: u8 = 3;
/// 心跳节流: 每 100ms 上报一次, 避免原子操作过载
/// 默认版本 (1ms 周期): HB_DIV=100 → 100ms
/// F3/F4 版本 (5ms 周期): HB_DIV=20 → 100ms
#[cfg(not(any(feature = "f3", feature = "f4")))]
const HB_DIV: u32 = 100;
#[cfg(any(feature = "f3", feature = "f4"))]
const HB_DIV: u32 = 20;

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("di-scan");

/// 启动 DI 扫描任务
// 架构改造 Phase 2: DI 扫描合并到 main_loop
// 原始: 独立 pthread 5ms 高频扫描
// 改造后: main_loop 100ms tick 调用 (HAL 间隔 20ms 一次, main_loop 5 分频)
// 牺牲: 5ms → 20ms 响应延迟, 对工业 50Hz 信号仍足够 (20ms 周期采 1 次)


struct DiState {
    stable: u64,
    candidate: u64,
    candidate_count: u8,
    tick_div: u32,
    last_hb: u32,
}

static DI_STATE: MainLoopCell<DiState> = MainLoopCell::new();

#[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
pub fn start_scan_task(_hal: Arc<Hal>) -> AppResult<()> {
    DI_STATE.init(DiState {
        stable: 0,
        candidate: 0,
        candidate_count: 0,
        tick_div: 0,
        last_hb: 0,
    });
    let _ = _hal;
    log::info!(
        "[di] scan task registered in main_loop (period=20ms, channels={} version {})",
        hw_version::DI_COUNT, hw_version::NAME
    );
    Ok(())
}

/// main_loop 每 20ms 调用一次 (5 分频)
pub fn tick_di_scan(hal: &Hal) {
    let state = match DI_STATE.get_mut() {
        Some(s) => s,
        None => return,
    };
    TASK_HB.tick();
    state.tick_div = state.tick_div.wrapping_add(1);

    let bits: u64 = match hal.dio().read_di_all() {
        Ok(v) => v,
        Err(e) => {
            // LOOP9: 读取失败时不应推进去抖计数 (旧实现用 state.candidate 作为 bits,
            // 导致 bits==candidate 恒成立, 错误 candidate 被快速确认为 stable, 误报 DI 变化)
            log::warn!("[di] read_di_all failed: {}", e);
            return;
        }
    };

    if bits == state.candidate {
        state.candidate_count = state.candidate_count.saturating_add(1);
    } else {
        state.candidate = bits;
        state.candidate_count = 1;
    }

    if state.candidate_count >= DEBOUNCE_COUNT && state.candidate != state.stable {
        let changed = state.candidate ^ state.stable;
        let rising = state.candidate & changed;
        let falling = state.stable & changed;
        if rising != 0 {
            log::debug!("[di] rising  : 0x{:016X}", rising);
        }
        if falling != 0 {
            log::debug!("[di] falling : 0x{:016X}", falling);
        }
        state.stable = state.candidate;
        crate::bus::IO.di.store_bits(state.stable);
        crate::bus::send_event(crate::bus::IoEvent::DiChanged);
    }
}
