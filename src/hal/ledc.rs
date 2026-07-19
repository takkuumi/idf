//! LEDC PWM 输出 (4 路 AO)
//!
//! 使用 `esp_idf_hal::ledc` 配置 1 个低速定时器 + 4 个通道，
//! 分辨率 12-bit (与 ADC 对齐)，频率由 `TimerConfig::frequency` 决定。
//!
//! # 自引用处理
//!
//! `LedcDriver` 借用 `LedcTimerDriver`。为在同一结构体中持有二者，
//! 将 `LedcTimerDriver` 通过 `Box::leak` 固定在堆上获取 `&'static` 引用，
//! 通道以 `'static` 生命周期存入 `Spin` 数组.
//! 由于 `Hal` 在程序生命周期内只创建一次且永不清除，此内存泄漏可接受。
//!
//! # 线程安全
//!
//! `LedcDriver::set_duty` 要求 `&mut self`，多线程 AO 输出任务
//! 并发写入时通过 `Spin` 短临界区串行化 (不阻塞内核).

use crate::sync::Spin;

use esp_idf_hal::gpio::AnyOutputPin;
use esp_idf_hal::ledc::{
    config::TimerConfig, LedcDriver, LedcTimerDriver, Resolution, LowSpeed,
    TIMER0, TIMER1, TIMER2, TIMER3, CHANNEL0, CHANNEL1, CHANNEL2, CHANNEL3,
};
use esp_idf_hal::units::Hertz;

use crate::error::{AppError, AppResult};
use crate::hal::pins::LedcTimerCfg;

/// AO 通道数
const AO_CHANNEL_COUNT: usize = 4;

/// LEDC 句柄
///
/// 持有 4 路 PWM 输出通道，每路独立 Spin.
/// timer 已 leak 到堆上为 `'static`，被各 channel 借用。
pub struct LedcHandle {
    channels: [Spin<LedcDriver<'static>>; AO_CHANNEL_COUNT],
}

/// 根据 u8 编号解析 Resolution 枚举
fn resolution_from_bits(bits: u8) -> AppResult<Resolution> {
    Ok(match bits {
        1 => Resolution::Bits1,
        2 => Resolution::Bits2,
        3 => Resolution::Bits3,
        4 => Resolution::Bits4,
        5 => Resolution::Bits5,
        6 => Resolution::Bits6,
        7 => Resolution::Bits7,
        8 => Resolution::Bits8,
        9 => Resolution::Bits9,
        10 => Resolution::Bits10,
        11 => Resolution::Bits11,
        12 => Resolution::Bits12,
        13 => Resolution::Bits13,
        14 => Resolution::Bits14,
        _ => return Err(AppError::Hal(format!("invalid ledc resolution bits: {bits}"))),
    })
}

impl LedcHandle {
    /// 初始化 LEDC 定时器 + 4 个通道。
    ///
    /// 参数:
    /// - `timer_cfg`: 定时器编号/频率/分辨率
    /// - `channels`: `[(ledc_channel, gpio_num); 4]`
    pub fn init(timer_cfg: LedcTimerCfg, channels: [(u8, u8); AO_CHANNEL_COUNT]) -> AppResult<Self> {
        let resolution = resolution_from_bits(timer_cfg.resolution_bits)?;
        let config = TimerConfig::default()
            .frequency(Hertz(timer_cfg.freq_hz))
            .resolution(resolution);

        // 1. 创建 LedcTimerDriver 并 leak 到堆上获取 'static
        //    根据 timer 编号选择对应的 typed peripheral
        //    SAFETY: timer 配置来自 config，确保未被其它驱动占用
        macro_rules! make_timer {
            ($timer:expr, $cfg:expr) => {{
                Box::leak(Box::new(
                    LedcTimerDriver::new($timer, $cfg)
                        .map_err(|e| AppError::Hal(format!("ledc timer: {e:?}")))?,
                ))
            }};
        }
        let timer_driver: &'static mut LedcTimerDriver<'static, LowSpeed> = match timer_cfg.timer {
            0 => make_timer!(unsafe { TIMER0::steal() }, &config),
            1 => make_timer!(unsafe { TIMER1::steal() }, &config),
            2 => make_timer!(unsafe { TIMER2::steal() }, &config),
            3 => make_timer!(unsafe { TIMER3::steal() }, &config),
            _ => {
                return Err(AppError::Hal(format!(
                    "invalid ledc timer: {}",
                    timer_cfg.timer
                )))
            }
        };

        // 2. 创建 4 个 LedcDriver
        //    每个 LedcDriver 借用 timer_driver (通过 &* reborrow, 不移动所有权)
        //    SAFETY: GPIO 编号来自 config，确保未被其它驱动占用
        let mut chs: Vec<Spin<LedcDriver<'static>>> = Vec::with_capacity(AO_CHANNEL_COUNT);
        for (i, (ch_num, gpio)) in channels.iter().enumerate() {
            let pin = unsafe { AnyOutputPin::steal(*gpio) };
            let driver = match *ch_num {
                0 => LedcDriver::new(unsafe { CHANNEL0::steal() }, &*timer_driver, pin),
                1 => LedcDriver::new(unsafe { CHANNEL1::steal() }, &*timer_driver, pin),
                2 => LedcDriver::new(unsafe { CHANNEL2::steal() }, &*timer_driver, pin),
                3 => LedcDriver::new(unsafe { CHANNEL3::steal() }, &*timer_driver, pin),
                _ => {
                    return Err(AppError::Hal(format!(
                        "invalid ledc channel: {ch_num}"
                    )))
                }
            }
            .map_err(|e| AppError::Hal(format!("ledc chan {i}: {e:?}")))?;
            chs.push(Spin::new(driver));
        }

        // Vec -> array (长度已知为 4)
        let channels: [Spin<LedcDriver<'static>>; AO_CHANNEL_COUNT] = [
            chs.remove(0),
            chs.remove(0),
            chs.remove(0),
            chs.remove(0),
        ];

        Ok(Self { channels })
    }

    /// 设置 AO 通道 idx (0..4) 的占空比。
    ///
    /// `duty` 范围 0..=2^resolution_bits-1 (12-bit 即 0..=4095)。
    /// 越界或硬件错误时记日志并忽略 (不向上传播)。
    pub fn set_duty(&self, idx: usize, duty: u32) {
        if idx >= AO_CHANNEL_COUNT {
            log::warn!("[ledc] set_duty idx out of range: {idx}");
            return;
        }
        let mut ch = self.channels[idx].lock();
        if let Err(e) = ch.set_duty(duty) {
            log::warn!("[ledc] set_duty ch{idx} failed: {e:?}");
        }
    }
}
