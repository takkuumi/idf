//! DI 数字输入扫描
//!
//! - main_loop 每 20ms 读取一次扩展 DI
//! - 连续三次相同采样确认变化，避免机械触点抖动
use std::sync::Arc;

use crate::config::hw_version;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::sync::MainLoopCell;

/// 去抖需要的连续相同采样次数
const DEBOUNCE_COUNT: u8 = 3;

// 启动 DI 扫描任务
// 架构改造 Phase 2: DI 扫描合并到 main_loop
// 原始: 独立 pthread 5ms 高频扫描
// 改造后: main_loop 20ms tick 调用
// 牺牲: 5ms → 20ms 响应延迟, 对工业 50Hz 信号仍足够 (20ms 周期采 1 次)

struct DiState {
    stable: u64,
    candidate: u64,
    candidate_count: u8,
}

static DI_STATE: MainLoopCell<DiState> = MainLoopCell::new();

#[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
pub fn start_scan_task(_hal: Arc<Hal>) -> AppResult<()> {
    DI_STATE
        .init(DiState {
            stable: 0,
            candidate: 0,
            candidate_count: 0,
        })
        .map_err(|_| crate::error::AppError::Io("DI state busy during init".into()))?;
    let _ = _hal;
    log::info!(
        "[di] scan task registered in main_loop (period=20ms, channels={} version {})",
        hw_version::DI_COUNT,
        hw_version::NAME
    );
    Ok(())
}

/// main_loop 每 20ms 调用一次
pub fn tick_di_scan(hal: &Hal) {
    let _ = DI_STATE.with_mut(|state| {
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
    });
}
