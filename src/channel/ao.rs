//! AO 模拟输出任务
//!
//! - 周期 100ms 读取 `BUS.ao.scaled`，转换为 duty 写入 `BUS.ao.duty`
//! - 调用 `hal.ledc.set_duty(idx, duty)` 输出 PWM
//! - 0-10000 (0-10V) 对应 duty 0-4095 (12-bit)
//!
//! 工程量尺度：scaled = 工程量 * 1000 (0..=10000 表示 0.000-10.000 V)

use std::sync::Arc;
use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// 输出周期 (ms)
const OUTPUT_PERIOD_MS: u64 = 100;
/// AO 通道数
const CHANNEL_COUNT: usize = 4;

/// scaled 上限 (0-10V 对应 0-10000)
const SCALED_MAX: u32 = 10000;
/// LEDC duty 上限 (12-bit, 由 config::pins::AO_RESOLUTION_BITS 推导)
const DUTY_MAX: u32 = (1u32 << (crate::config::pins::AO_RESOLUTION_BITS as u32)) - 1;

/// 任务心跳记录 (静态分配, main_loop 监控)
static TASK_HB: TaskHb = TaskHb::new("ao-output");

/// 启动 AO 输出任务
///
/// TODO: 假设 `hal.ledc.set_duty(idx: usize, duty: u32) -> ()`，由 hal/ledc 模块实现后接入。
pub fn start_output_task(hal: Arc<Hal>) -> AppResult<()> {
    health::register(&TASK_HB);
    health::set_next_thread_core(health::CORE_RT);
    let result = std::thread::Builder::new()
        .name("ao-output".into())
        .spawn(move || {
            log::info!("[ao] output task started, period={}ms", OUTPUT_PERIOD_MS);

            // 上一次输出的 duty，初值全 1 使首次必然全量刷新
            let mut last_duty = [u32::MAX; CHANNEL_COUNT];

            loop {
                // 心跳: 每次循环 (100ms) 上报一次
                TASK_HB.tick();
                // 1. 读取 scaled, 计算 duty, 写回 duty 到总线
                //    阶段 A: 读 scaled + 写 duty 全过 bus::IO.ao (原子), 无锁
                let mut duties = [0u32; CHANNEL_COUNT];
                for ch in 0..CHANNEL_COUNT {
                    let scaled = crate::bus::IO.ao.get_scaled(ch);
                    duties[ch] = (scaled as u32) * DUTY_MAX / SCALED_MAX;
                    crate::bus::IO.ao.set_duty(ch, duties[ch]);
                }
                crate::bus::send_event(crate::bus::IoEvent::AoUpdated);

                // 2. 仅在 duty 变化时调用 LEDC (硬件 IO 在总线锁外执行)
                for ch in 0..CHANNEL_COUNT {
                    if duties[ch] != last_duty[ch] {
                        hal.ledc.set_duty(ch, duties[ch]);
                        last_duty[ch] = duties[ch];
                    }
                }

                std::thread::sleep(Duration::from_millis(OUTPUT_PERIOD_MS));
            }
        });
    health::reset_thread_core();
    result.map_err(|e| AppError::Channel(format!("spawn ao-output: {e}")))?;

    Ok(())
}
