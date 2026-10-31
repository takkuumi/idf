//! 非阻塞开机校准。main_loop tick 执行，每次最多一个样本；服务启动不等待 8 秒。
//! 保留原有 min/max 有效范围、每次仅选一个零校准通道及异步 holding 落盘规则。

use super::calibration_window::CalibrationWindow;
use crate::config::ai_calib;
use crate::error::AppResult;
use crate::hal::Hal;
use crate::sync::MainLoopCell;
use std::sync::atomic::{AtomicU8, Ordering};

const SENSOR_CHANNELS: usize = 8;
// 0=未启动、1=采样、2=完成、3=无有效端点/无需校准、4=现场参数已更改。
static STATUS: AtomicU8 = AtomicU8::new(0);
struct Session {
    channel: usize,
    window: CalibrationWindow,
    finished: bool,
}
static SESSION: MainLoopCell<Session> = MainLoopCell::new();

fn now_ms() -> u32 {
    (unsafe { esp_idf_sys::esp_timer_get_time() } / 1000) as u32
}

pub fn status_code() -> u8 {
    STATUS.load(Ordering::Acquire)
}

pub fn start_auto_calibration() -> AppResult<()> {
    if STATUS.load(Ordering::Acquire) != 0 {
        return Ok(());
    }
    let selected = crate::bus::storage_state::storage_read_with(|snap| {
        let min = usize::from(
            crate::config::regs::HOLD_SENSOR_MIN_BASE - crate::config::regs::HOLD_CFG_BASE,
        );
        let max = usize::from(
            crate::config::regs::HOLD_SENSOR_MAX_BASE - crate::config::regs::HOLD_CFG_BASE,
        );
        (0..calibration_channel_count(crate::config::hw_version::AI_COUNT)).find(|&ch| {
            snap.holding_buf.get(min + ch) == Some(&0) && snap.holding_buf.get(max + ch) == Some(&0)
        })
    })
    .flatten();
    let Some(channel) = selected else {
        STATUS.store(3, Ordering::Release);
        log::info!("[adc-calib] 无待校准物理通道，跳过");
        return Ok(());
    };
    SESSION
        .init(Session {
            channel,
            window: CalibrationWindow::new(now_ms()),
            finished: false,
        })
        .map_err(|_| crate::error::AppError::Channel("calibration state busy".into()))?;
    STATUS.store(1, Ordering::Release);
    log::info!("[adc-calib] AI{channel} 开始非阻塞 8 秒校准，通信服务继续启动");
    Ok(())
}

pub fn tick_auto_calibration(hal: &Hal) {
    if STATUS.load(Ordering::Acquire) != 1 {
        return;
    }
    let _ = SESSION.with_mut(|session| {
        if session.finished {
            return;
        }
        let now = now_ms();
        if !session.window.done(now, ai_calib::CALIB_WINDOW_MS as u32) {
            if session
                .window
                .sample_due(now, ai_calib::CALIB_SAMPLE_INTERVAL_MS as u32)
            {
                session.window.observe(now, hal.adc.sample(session.channel));
            }
            return;
        }
        session.finished = true;
        let window = &session.window;
        let min_ok = window.samples != 0
            && window.raw_min > ai_calib::SENSOR_MIN.saturating_sub(100)
            && window.raw_min < ai_calib::SENSOR_MIN + 200;
        let max_ok = window.samples != 0
            && window.raw_max > ai_calib::SENSOR_MAX.saturating_sub(100)
            && window.raw_max < ai_calib::SENSOR_MAX + 200;
        log::info!(
            "[adc-calib] AI{} raw_min={} raw_max={} samples={}",
            session.channel,
            window.raw_min,
            window.raw_max,
            window.samples
        );
        if !min_ok && !max_ok {
            STATUS.store(3, Ordering::Release);
            log::warn!(
                "[adc-calib] AI{} 无有效校准端点，保留默认标定",
                session.channel
            );
            return;
        }
        let mut changed = false;
        crate::bus::backends::storage_modify_holding(|holding| {
            let min = usize::from(
                crate::config::regs::HOLD_SENSOR_MIN_BASE - crate::config::regs::HOLD_CFG_BASE,
            ) + session.channel;
            let max = usize::from(
                crate::config::regs::HOLD_SENSOR_MAX_BASE - crate::config::regs::HOLD_CFG_BASE,
            ) + session.channel;
            // 服务已经可写。现场在校准期间修改参数时，不能用启动采样覆盖它。
            if holding[min] != 0 || holding[max] != 0 {
                return;
            }
            if min_ok {
                holding[min] = window.raw_min;
            }
            if max_ok {
                holding[max] = window.raw_max;
            }
            changed = true;
        });
        if changed {
            crate::device::request_persist_holding();
            STATUS.store(2, Ordering::Release);
            log::info!("[adc-calib] AI{} 校准完成，已请求异步保存", session.channel);
        } else {
            STATUS.store(4, Ordering::Release);
            log::info!("[adc-calib] 现场标定已更改，取消启动采样回写");
        }
    });
}

fn calibration_channel_count(configured_channels: u16) -> usize {
    usize::from(configured_channels).min(SENSOR_CHANNELS)
}

#[cfg(test)]
mod channel_count_tests {
    use super::*;
    #[test]
    fn test_calibration_ignores_legacy_slots_without_physical_channels() {
        assert_eq!(calibration_channel_count(0), 0);
        assert_eq!(calibration_channel_count(4), 4);
        assert_eq!(calibration_channel_count(6), 6);
        assert_eq!(calibration_channel_count(8), 8);
        assert_eq!(calibration_channel_count(9), 8);
    }
}
