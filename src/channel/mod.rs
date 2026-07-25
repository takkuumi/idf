//! AI/AO 通道管理
//!
//! - AI：6 路 12-bit ADC，可配置采样周期、滑动平均
//! - AO：4 路 LEDC PWM 输出，经 RC 滤波成模拟量
//!
//! 启动入口：[`start`]

use std::sync::Arc;

use esp_idf_svc::timer::EspTaskTimerService;

use crate::error::AppResult;
use crate::hal::Hal;

pub mod ai;
pub mod ao;
pub mod calib;

/// 启动 AI 采样 + AO 输出任务
pub fn start(_hal: Arc<Hal>, _timer_svc: EspTaskTimerService) -> AppResult<()> {
    ao::start_output_task(_hal.clone())?;
    ai::start_sample_task(_hal, _timer_svc)?;
    Ok(())
}
