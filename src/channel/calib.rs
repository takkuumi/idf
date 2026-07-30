//! ADC 自动校准 (开机 8 秒窗口)
//!
//! 移植自参考固件 MCA_F16V2_1_F48_BLE.ino:1946-1989:
//! - 开机后 8 秒内持续采样指定 ADC 通道
//! - 记录传感器 min/max 原始值，通过有效性检查后写入保持寄存器 (2280-2295)
//! - 每次重启仅校准一个通道（未校准通道 min/max 为 0 时触发）
//! - 校准值由 tick_ai_sample 读取，用于线性映射:
//!   scaled = map(avg_raw, cal_max, cal_min, 4095, 0) → 0..4095
//!
//! 存储: 保持寄存器 (holding_buf) 2280+i (SENSOR_MIN) / 2288+i (SENSOR_MAX)
//!       通过 RCU STORAGE 快照写入，actor 异步落盘 NVS

use std::time::Duration;

use crate::config::ai_calib;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// 校准任务心跳 (阈值 12s, 8s 校准窗口 + 2s 余量)
static TASK_HB: TaskHb = TaskHb::new_with_stall("adc-calib", 12);

/// 传感器校准寄存器通道数 (对齐参考固件 SENSOR_NUM=8)
const SENSOR_CHANNELS: usize = 8;

/// LOOP9: 标记 calib 任务为一次性任务已完成.
/// 校准结束后心跳不再递增, 若不标记则 check_all 误判为停滞 → 强制重启.
pub fn mark_task_completed() {
    TASK_HB.mark_completed();
    log::debug!("[adc-calib] task marked completed (one-shot)");
}

fn finish_calib() {
    mark_task_completed();
}

/// 开机时执行 ADC 自动校准
///
/// 必须在 device::init() 完成后、进入主循环前调用。
/// 校准结果写入保持寄存器 2280-2295 (SENSOR_MIN/MAX × 8)，
/// tick_ai_sample 可通过 holding_buf 读取使用。
///
/// 机制: 每次重启仅校准一个通道。遍历所有通道，
/// 找到第一个 min=0 且 max=0 的通道进行校准。
pub fn run_auto_calibration(hal: &Hal) -> AppResult<()> {
    // 读取现有校准值 (从 RCU STORAGE 快照的 holding_buf)
    let holding = crate::bus::storage_state::storage_read()
        .map(|s| s.holding_buf.clone())
        .unwrap_or_default();

    let mut cal_ch: Option<usize> = None;

    for ch in 0..SENSOR_CHANNELS {
        let min_idx = (crate::config::regs::HOLD_SENSOR_MIN_BASE as usize)
            .checked_sub(crate::config::regs::HOLD_CFG_BASE as usize);
        let max_idx = (crate::config::regs::HOLD_SENSOR_MAX_BASE as usize)
            .checked_sub(crate::config::regs::HOLD_CFG_BASE as usize);

        if let (Some(mi), Some(ma)) = (min_idx, max_idx) {
            let v_min = *holding.get(mi + ch).unwrap_or(&0);
            let v_max = *holding.get(ma + ch).unwrap_or(&0);
            if v_min == 0 && v_max == 0 {
                cal_ch = Some(ch);
                break;
            }
        }
    }

    let ch = match cal_ch {
        Some(c) => c,
        None => {
            log::info!("[adc-calib] 所有通道已校准, 跳过");
            finish_calib();
            return Ok(());
        }
    };

    log::info!(
        "[adc-calib] AI{} 开始 8 秒校准 (SENSOR_MIN={} SENSOR_MAX={})",
        ch,
        ai_calib::SENSOR_MIN,
        ai_calib::SENSOR_MAX
    );

    // 8 秒窗口: 每 CALIB_SAMPLE_INTERVAL_MS 采样一次
    let start = std::time::Instant::now();
    let window = Duration::from_millis(ai_calib::CALIB_WINDOW_MS);
    let interval = Duration::from_millis(ai_calib::CALIB_SAMPLE_INTERVAL_MS);
    let mut raw_min = u16::MAX;
    let mut raw_max = 0u16;
    let mut samples = 0u32;

    while start.elapsed() < window {
        TASK_HB.tick();
        health::feed_wdt(); // 校准期间喂狗, 防止 WDT 误触发

        let raw = hal.adc.sample(ch);
        if raw < raw_min {
            raw_min = raw;
        }
        if raw > raw_max {
            raw_max = raw;
        }
        samples += 1;
        std::thread::sleep(interval);
    }

    log::info!(
        "[adc-calib] AI{}: raw_min={} raw_max={} samples={}",
        ch,
        raw_min,
        raw_max,
        samples
    );

    // 有效性检查 (对齐参考固件: SENSOR_MIN-100 < min < SENSOR_MIN+200)
    let min_lo = ai_calib::SENSOR_MIN.saturating_sub(100);
    let min_hi = ai_calib::SENSOR_MIN + 200;
    let max_lo = ai_calib::SENSOR_MAX.saturating_sub(100);
    let max_hi = ai_calib::SENSOR_MAX + 200;

    let smin_ok = raw_min > min_lo && raw_min < min_hi;
    let smax_ok = raw_max > max_lo && raw_max < max_hi;

    if !smin_ok {
        log::warn!(
            "[adc-calib] AI{} min={} 不在有效范围 ({}..{}), 跳过 min",
            ch,
            raw_min,
            min_lo,
            min_hi
        );
    }
    if !smax_ok {
        log::warn!(
            "[adc-calib] AI{} max={} 不在有效范围 ({}..{}), 跳过 max",
            ch,
            raw_max,
            max_lo,
            max_hi
        );
    }

    if !smin_ok && !smax_ok {
        log::warn!("[adc-calib] AI{} 校准跳过 (min/max 均超出范围)", ch);
        finish_calib();
        return Ok(());
    }

    // 写入保持寄存器 (RCU STORAGE 快照, actor 异步落盘 NVS)
    crate::bus::backends::storage_modify_holding(|holding| {
        let base_min = (crate::config::regs::HOLD_SENSOR_MIN_BASE as usize)
            .saturating_sub(crate::config::regs::HOLD_CFG_BASE as usize);
        let base_max = (crate::config::regs::HOLD_SENSOR_MAX_BASE as usize)
            .saturating_sub(crate::config::regs::HOLD_CFG_BASE as usize);

        if smin_ok {
            holding[base_min + ch] = raw_min;
        }
        if smax_ok {
            holding[base_max + ch] = raw_max;
        }
    });

    crate::device::request_persist_holding();

    log::info!(
        "[adc-calib] AI{} 校准完成: min={} max={} → 保持寄存器 [{},{}]",
        ch,
        if smin_ok { raw_min } else { 0 },
        if smax_ok { raw_max } else { 0 },
        crate::config::regs::HOLD_SENSOR_MIN_BASE + ch as u16,
        crate::config::regs::HOLD_SENSOR_MAX_BASE + ch as u16,
    );

    finish_calib();
    Ok(())
}
