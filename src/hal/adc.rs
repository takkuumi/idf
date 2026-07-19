//! ADC1 采样 (ESP32-S3)
//!
//! 支持两种模式 (通过 feature flag 切换):
//!
//! - **Continuous + DMA 模式** (`feature = "adc-continuous"`, 默认):
//!   ADC1 6 通道在后台 DMA 连续采样, CPU 仅读环形缓冲区, 零 CPU 占用。
//!   用 `esp_idf_sys` 直接调用 `adc_continuous_*` API。
//!   每 100ms 调用 `sample_all()` 时, 从 DMA 缓冲区读取 N 帧并取平均。
//!
//! - **OneShot + Mutex 模式** (兼容性 fallback):
//!   用 `esp_idf_hal::adc` 的 OneShot API, 每次采样需 CPU 触发 ADC 转换。
//!   多线程并发采样需 `Spin` 短临界区串行化.
//!
//! # ESP32-S3 ADC1 通道映射
//!
//! | ADC 通道 | GPIO | 用途 |
//! |---------|------|------|
//! | CH0 | GPIO1 | AI0 |
//! | CH1 | GPIO2 | AI1 |
//! | CH2 | GPIO3 | AI2 |
//! | CH3 | GPIO4 | AI3 |
//! | CH4 | GPIO5 | AI4 |
//! | CH5 | GPIO6 | AI5 |

use crate::error::{AppError, AppResult};
use crate::hal::pins::Adc1Cfg;

// ============================================================================
// 统一对外句柄 (根据 feature 切换内部实现)
// ============================================================================

/// ADC1 句柄 (统一 API, 内部实现根据 feature = "adc-continuous" 切换)
///
/// - `feature = "adc-continuous"` (默认): Continuous + DMA 模式
/// - 否则: OneShot + Mutex 模式
pub struct AdcHandle {
    #[cfg(feature = "adc-continuous")]
    inner: AdcContinuous,
    #[cfg(not(feature = "adc-continuous"))]
    inner: AdcOneShot,
}

impl AdcHandle {
    /// 初始化 ADC1 + 6 个通道
    pub fn init(cfg: Adc1Cfg) -> AppResult<Self> {
        Ok(Self {
            #[cfg(feature = "adc-continuous")]
            inner: AdcContinuous::init(cfg)?,
            #[cfg(not(feature = "adc-continuous"))]
            inner: AdcOneShot::init(cfg)?,
        })
    }

    /// 批量采样全部 6 通道
    ///
    /// - Continuous 模式: 从 DMA 环形缓冲区读取并取平均 (零 CPU 占用)
    /// - OneShot 模式: 一次 Mutex 锁采样 6 通道
    #[inline]
    pub fn sample_all(&self) -> [u16; 6] {
        self.inner.sample_all()
    }

    /// 采样单通道 idx (0..6)
    #[inline]
    pub fn sample(&self, idx: usize) -> u16 {
        self.inner.sample(idx)
    }
}

// ============================================================================
// Continuous + DMA 模式 (ESP32-S3 硬件加速)
// ============================================================================

#[cfg(feature = "adc-continuous")]
mod adc_continuous {
    use super::*;

    /// ADC Continuous + DMA 句柄
    ///
    /// ADC1 6 通道在后台 DMA 连续采样 (10kHz/通道), CPU 仅读缓冲区。
    /// `sample_all()` 从 DMA 环形缓冲区读取所有可读帧, 按通道分组取平均,
    /// 返回 6 个 12-bit 平均值。
    pub struct AdcContinuous {
        handle: esp_idf_sys::adc_continuous_handle_t,
    }

    // ADC handle is only used from the AI sampling task; safe to share.
    unsafe impl Send for AdcContinuous {}
    unsafe impl Sync for AdcContinuous {}

    impl AdcContinuous {
        pub fn init(cfg: Adc1Cfg) -> AppResult<Self> {
            // 1. 创建 Continuous handle
            let frame_cfg = esp_idf_sys::adc_continuous_handle_cfg_t {
                max_store_buf_size: 1024,  // 内部环形缓冲区 1KB
                conv_frame_size: 256,     // 单帧 256 字节 (≈ 128 样本)
                flags: Default::default(),
            };
            let mut handle: esp_idf_sys::adc_continuous_handle_t = std::ptr::null_mut();
            let r = unsafe {
                esp_idf_sys::adc_continuous_new_handle(&frame_cfg, &mut handle)
            };
            if r != 0 {
                return Err(AppError::Hal(format!(
                    "adc_continuous_new_handle failed: 0x{:08X}", r
                )));
            }

            // 2. 配置 6 个通道 (ADC1_CH0-5, 12-bit, 11dB 衰减 = 0-3.1V)
            let atten = esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11 as u8;
            let bit_width = esp_idf_sys::SOC_ADC_DIGI_MAX_BITWIDTH as u8;
            let pattern: [esp_idf_sys::adc_digi_pattern_config_t; 6] = [
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten, channel: cfg.channels[0], unit: 0, bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten, channel: cfg.channels[1], unit: 0, bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten, channel: cfg.channels[2], unit: 0, bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten, channel: cfg.channels[3], unit: 0, bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten, channel: cfg.channels[4], unit: 0, bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten, channel: cfg.channels[5], unit: 0, bit_width,
                },
            ];

            let dig_cfg = esp_idf_sys::adc_continuous_config_t {
                conv_mode: esp_idf_sys::adc_digi_convert_mode_t_ADC_CONV_SINGLE_UNIT_1,
                format: esp_idf_sys::adc_digi_output_format_t_ADC_DIGI_OUTPUT_FORMAT_TYPE1,
                sample_freq_hz: 10_000,  // 10kHz/通道 (6 通道共 60kHz)
                adc_pattern: pattern.as_ptr() as *mut _,
                pattern_num: 6,
            };
            let r = unsafe { esp_idf_sys::adc_continuous_config(handle, &dig_cfg) };
            if r != 0 {
                return Err(AppError::Hal(format!(
                    "adc_continuous_config failed: 0x{:08X}", r
                )));
            }

            // 3. 启动连续采样 (DMA 后台运行)
            let r = unsafe { esp_idf_sys::adc_continuous_start(handle) };
            if r != 0 {
                return Err(AppError::Hal(format!(
                    "adc_continuous_start failed: 0x{:08X}", r
                )));
            }

            log::info!(
                "[adc] Continuous+DMA mode started: 6 ch, 10kHz/ch, 12-bit, 0-3.1V"
            );

            Ok(Self { handle })
        }

        /// 从 DMA 缓冲区读取并按通道平均
        ///
        /// ESP32-S3 ADC Continuous 输出格式 (12-bit mode):
        /// 每样本 2 字节: [data:12 | channel:4] (小端序)
        ///   bit 0-11: 12-bit ADC 原始值
        ///   bit 12-15: 通道号 (0-9 for ADC1)
        pub fn sample_all(&self) -> [u16; 6] {
            let mut sum = [0u32; 6];
            let mut count = [0u32; 6];

            // DMA 读取缓冲区 (栈分配, 单帧容纳 6 通道 × N 次采样)
            let mut buf = [0u8; 512];

            // 循环读取直到 DMA 缓冲区为空 (timeout=0 非阻塞)
            loop {
                let mut ret_num: u32 = 0;
                let r = unsafe {
                    esp_idf_sys::adc_continuous_read(
                        self.handle,
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        &mut ret_num,
                        0,  // 非阻塞
                    )
                };
                if r != 0 || ret_num == 0 {
                    break;
                }

                // 解析样本 (每 2 字节一个样本)
                let n_samples = ret_num as usize / 2;
                for i in 0..n_samples {
                    let raw = u16::from_ne_bytes([
                        buf[i * 2],
                        buf[i * 2 + 1],
                    ]);
                    let channel = ((raw >> 12) & 0xF) as usize;
                    let data = raw & 0xFFF;  // 12-bit
                    if channel < 6 {
                        sum[channel] += data as u32;
                        count[channel] += 1;
                    }
                }
            }

            // 计算每通道平均值
            let mut out = [0u16; 6];
            for i in 0..6 {
                if count[i] > 0 {
                    out[i] = (sum[i] / count[i]) as u16;
                }
            }
            out
        }

        /// 单通道采样 (从 sample_all 结果取一个)
        pub fn sample(&self, idx: usize) -> u16 {
            if idx >= 6 {
                return 0;
            }
            self.sample_all()[idx]
        }
    }

    impl Drop for AdcContinuous {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe {
                    let _ = esp_idf_sys::adc_continuous_stop(self.handle);
                    let _ = esp_idf_sys::adc_continuous_deinit(self.handle);
                }
            }
        }
    }
}

#[cfg(feature = "adc-continuous")]
use adc_continuous::AdcContinuous;

// ============================================================================
// OneShot + Mutex 模式 (兼容性 fallback)
// ============================================================================

#[cfg(not(feature = "adc-continuous"))]
mod adc_oneshot {
    use super::*;
    use crate::sync::Spin;

    use esp_idf_hal::adc::{config::Config, AdcChannelDriver, AdcDriver};
    use esp_idf_hal::gpio::AnyInputPin;
    use esp_idf_hal::peripherals::ADC1;

    /// ADC OneShot 句柄 (fallback, 不启用 feature = "adc-continuous" 时使用)
    pub struct AdcOneShot {
        driver: Spin<AdcDriver<'static>>,
        channels: [AdcChannelDriver<'static, AnyInputPin>; 6],
    }

    impl AdcOneShot {
        pub fn init(cfg: Adc1Cfg) -> AppResult<Self> {
            let adc1 = unsafe { ADC1::new() };
            let driver = AdcDriver::new(adc1, &Config::default())
                .map_err(|e| AppError::Hal(format!("adc driver: {e:?}")))?;

            let channels: [AdcChannelDriver<'static, AnyInputPin>; 6] = [
                AdcChannelDriver::new(unsafe { AnyInputPin::new(cfg.channels[0] as i32) })
                    .map_err(|e| AppError::Hal(format!("adc chan 0: {e:?}")))?,
                AdcChannelDriver::new(unsafe { AnyInputPin::new(cfg.channels[1] as i32) })
                    .map_err(|e| AppError::Hal(format!("adc chan 1: {e:?}")))?,
                AdcChannelDriver::new(unsafe { AnyInputPin::new(cfg.channels[2] as i32) })
                    .map_err(|e| AppError::Hal(format!("adc chan 2: {e:?}")))?,
                AdcChannelDriver::new(unsafe { AnyInputPin::new(cfg.channels[3] as i32) })
                    .map_err(|e| AppError::Hal(format!("adc chan 3: {e:?}")))?,
                AdcChannelDriver::new(unsafe { AnyInputPin::new(cfg.channels[4] as i32) })
                    .map_err(|e| AppError::Hal(format!("adc chan 4: {e:?}")))?,
                AdcChannelDriver::new(unsafe { AnyInputPin::new(cfg.channels[5] as i32) })
                    .map_err(|e| AppError::Hal(format!("adc chan 5: {e:?}")))?,
            ];

            log::info!("[adc] OneShot+Spin mode (fallback)");

            Ok(Self {
                driver: Spin::new(driver),
                channels,
            })
        }

        pub fn sample_all(&self) -> [u16; 6] {
            let mut out = [0u16; 6];
            let mut driver = self.driver.lock();
            for i in 0..6 {
                out[i] = match driver.read(&self.channels[i]) {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("[adc] read ch{i} failed: {e:?}");
                        0
                    }
                };
            }
            out
        }

        pub fn sample(&self, idx: usize) -> u16 {
            if idx >= 6 {
                return 0;
            }
            let mut driver = self.driver.lock();
            match driver.read(&self.channels[idx]) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("[adc] read ch{idx} failed: {e:?}");
                    0
                }
            }
        }
    }
}

#[cfg(not(feature = "adc-continuous"))]
use adc_oneshot::AdcOneShot;
