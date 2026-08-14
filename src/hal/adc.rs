//! ADC1 采样 (ESP32-S3)
//!
//! 支持两种模式 (通过 feature flag 切换):
//!
//! - **Continuous + DMA 模式** (`feature = "adc-continuous"`, 默认):
//!   ADC1 6 通道在后台 DMA 连续采样, CPU 仅读环形缓冲区, 零 CPU 占用。
//!   用 `esp_idf_sys` 直接调用 `adc_continuous_*` API。
//!   每 100ms 调用 `sample_all()` 时, 从 DMA 缓冲区读取 N 帧并取平均。
//!
//! - **OneShot + Spin 模式** (兼容性 fallback):
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
    /// ADC1 6 通道以 1kHz 总频率在后台 DMA 连续采样，CPU 仅读缓冲区。
    /// `sample_all()` 从 DMA 环形缓冲区读取所有可读帧, 按通道分组取平均,
    /// 返回 6 个 12-bit 平均值。
    pub struct AdcContinuous {
        handle: esp_idf_sys::adc_continuous_handle_t,
        last: [core::sync::atomic::AtomicU16; 6],
        read_errors: core::sync::atomic::AtomicU32,
        read_lock: crate::sync::Spin<()>,
    }

    // SAFETY: DMA reads are serialized by read_lock and Drop requires exclusive ownership.
    unsafe impl Send for AdcContinuous {}
    unsafe impl Sync for AdcContinuous {}

    impl AdcContinuous {
        pub fn init(cfg: Adc1Cfg) -> AppResult<Self> {
            // 1. 创建 Continuous handle
            let frame_cfg = esp_idf_sys::adc_continuous_handle_cfg_t {
                max_store_buf_size: 1024, // 内部环形缓冲区 1KB
                conv_frame_size: 256,     // 单帧 256 字节 (≈ 128 样本)
                flags: Default::default(),
            };
            let mut handle: esp_idf_sys::adc_continuous_handle_t = std::ptr::null_mut();
            // SAFETY: frame_cfg 是栈上局部 &, handle 是 &mut null 初始化.
            // adc_continuous_new_handle 是 ESP-IDF 提供的 handle 分配 API,
            // 仅读写调用方提供的指针, 无全局副作用.
            let r = unsafe { esp_idf_sys::adc_continuous_new_handle(&frame_cfg, &mut handle) };
            if r != 0 {
                return Err(AppError::Hal(format!(
                    "adc_continuous_new_handle failed: 0x{:08X}",
                    r
                )));
            }

            // 2. 配置 6 个通道 (ADC1_CH0-5, 12-bit, 11dB 衰减 = 0-3.1V)
            let atten = esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11 as u8;
            let bit_width = esp_idf_sys::SOC_ADC_DIGI_MAX_BITWIDTH as u8;
            let pattern: [esp_idf_sys::adc_digi_pattern_config_t; 6] = [
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten,
                    channel: cfg.channels[0],
                    unit: 0,
                    bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten,
                    channel: cfg.channels[1],
                    unit: 0,
                    bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten,
                    channel: cfg.channels[2],
                    unit: 0,
                    bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten,
                    channel: cfg.channels[3],
                    unit: 0,
                    bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten,
                    channel: cfg.channels[4],
                    unit: 0,
                    bit_width,
                },
                esp_idf_sys::adc_digi_pattern_config_t {
                    atten,
                    channel: cfg.channels[5],
                    unit: 0,
                    bit_width,
                },
            ];

            let dig_cfg = esp_idf_sys::adc_continuous_config_t {
                conv_mode: esp_idf_sys::adc_digi_convert_mode_t_ADC_CONV_SINGLE_UNIT_1,
                // ESP32-S3 只产生 4-byte Type2 DMA 结果，IDF 内部也会强制 Type2。
                format: esp_idf_sys::adc_digi_output_format_t_ADC_DIGI_OUTPUT_FORMAT_TYPE2,
                // 4-20mA 是慢变量。1kHz 总采样率约为每通道 167Hz；1KB ring
                // 可容纳约 256ms 数据，覆盖 100ms 主循环周期及合理调度抖动。
                sample_freq_hz: 1_000,
                adc_pattern: pattern.as_ptr() as *mut _,
                pattern_num: 6,
            };
            let r = unsafe { esp_idf_sys::adc_continuous_config(handle, &dig_cfg) };
            if r != 0 {
                // SAFETY: handle 由 new_handle 成功创建，尚未 start，可直接释放。
                unsafe { esp_idf_sys::adc_continuous_deinit(handle) };
                return Err(AppError::Hal(format!(
                    "adc_continuous_config failed: 0x{:08X}",
                    r
                )));
            }

            // 3. 启动连续采样 (DMA 后台运行)
            let r = unsafe { esp_idf_sys::adc_continuous_start(handle) };
            if r != 0 {
                // SAFETY: start 失败后句柄仍处于可 deinit 状态。
                unsafe { esp_idf_sys::adc_continuous_deinit(handle) };
                return Err(AppError::Hal(format!(
                    "adc_continuous_start failed: 0x{:08X}",
                    r
                )));
            }

            log::info!("[adc] Continuous+DMA mode started: 6 ch, 1kHz total, Type2 12-bit");

            Ok(Self {
                handle,
                last: [const { core::sync::atomic::AtomicU16::new(0) }; 6],
                read_errors: core::sync::atomic::AtomicU32::new(0),
                read_lock: crate::sync::Spin::new(()),
            })
        }

        /// 从 DMA 缓冲区读取并按通道平均
        ///
        /// ESP32-S3 ADC Continuous Type2 输出格式 (12-bit mode):
        /// 每样本 4 字节，data=bits 0..11，channel=bits 13..16，unit=bit 17。
        pub fn sample_all(&self) -> [u16; 6] {
            let _read_guard = self.read_lock.lock();
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
                        0, // 非阻塞
                    )
                };
                if r == esp_idf_sys::ESP_ERR_TIMEOUT || ret_num == 0 {
                    break;
                }
                if r != esp_idf_sys::ESP_OK {
                    let errors = self
                        .read_errors
                        .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                        + 1;
                    if errors == 1 || errors.is_multiple_of(100) {
                        log::warn!("[adc] continuous read failed #{errors}: 0x{r:08X}");
                    }
                    break;
                }

                for sample in buf[..ret_num as usize].chunks_exact(4) {
                    let encoded = [sample[0], sample[1], sample[2], sample[3]];
                    let (unit, channel, data) = decode_type2_sample(encoded);
                    if unit == 0 && channel < 6 {
                        sum[channel] += data as u32;
                        count[channel] += 1;
                    }
                }
            }

            // 计算每通道平均值
            let mut out =
                core::array::from_fn(|i| self.last[i].load(core::sync::atomic::Ordering::Acquire));
            for i in 0..6 {
                if count[i] > 0 {
                    out[i] = (sum[i] / count[i]) as u16;
                    self.last[i].store(out[i], core::sync::atomic::Ordering::Release);
                }
            }
            if count.iter().any(|&samples| samples != 0) {
                self.read_errors
                    .store(0, core::sync::atomic::Ordering::Relaxed);
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

    #[inline]
    fn decode_type2_sample(sample: [u8; 4]) -> (u8, usize, u16) {
        let raw = u32::from_le_bytes(sample);
        let data = (raw & 0x0FFF) as u16;
        let channel = ((raw >> 13) & 0x0F) as usize;
        let unit = ((raw >> 17) & 0x01) as u8;
        (unit, channel, data)
    }

    #[cfg(test)]
    mod tests {
        use super::decode_type2_sample;

        #[test]
        fn test_decode_esp32s3_type2_dma_sample() {
            let raw = 0x0A5Au32 | (5 << 13);
            assert_eq!(decode_type2_sample(raw.to_le_bytes()), (0, 5, 0x0A5A));

            let adc2_raw = 0x0123u32 | (3 << 13) | (1 << 17);
            assert_eq!(decode_type2_sample(adc2_raw.to_le_bytes()), (1, 3, 0x0123));
        }
    }

    impl Drop for AdcContinuous {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                // SAFETY: handle 来自 init 成功时的非 null 分配, drop 时独占访问,
                // ESP-IDF 保证 stop/deinit 后 handle 不再有效. 二次 drop 会被 null 检查拦截.
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

    /// ADC OneShot 句柄 (fallback, 不启用 feature = "adc-continuous" 时使用)
    pub struct AdcOneShot {
        handle: esp_idf_sys::adc_oneshot_unit_handle_t,
        channels: [u8; 6],
        read_lock: Spin<()>,
        last: [core::sync::atomic::AtomicU16; 6],
        failures: [core::sync::atomic::AtomicU32; 6],
    }

    // SAFETY: all reads are serialized and Drop has exclusive ownership.
    unsafe impl Send for AdcOneShot {}
    unsafe impl Sync for AdcOneShot {}

    impl AdcOneShot {
        pub fn init(cfg: Adc1Cfg) -> AppResult<Self> {
            let unit_cfg = esp_idf_sys::adc_oneshot_unit_init_cfg_t {
                unit_id: esp_idf_sys::adc_unit_t_ADC_UNIT_1,
                ..Default::default()
            };
            let mut handle: esp_idf_sys::adc_oneshot_unit_handle_t = std::ptr::null_mut();
            let ret = unsafe { esp_idf_sys::adc_oneshot_new_unit(&unit_cfg, &mut handle) };
            if ret != esp_idf_sys::ESP_OK {
                return Err(AppError::Hal(format!(
                    "adc_oneshot_new_unit failed: 0x{ret:08X}"
                )));
            }

            let channel_cfg = esp_idf_sys::adc_oneshot_chan_cfg_t {
                atten: esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_11,
                bitwidth: esp_idf_sys::adc_bitwidth_t_ADC_BITWIDTH_DEFAULT,
            };
            for &channel in &cfg.channels {
                let ret = unsafe {
                    esp_idf_sys::adc_oneshot_config_channel(
                        handle,
                        channel as esp_idf_sys::adc_channel_t,
                        &channel_cfg,
                    )
                };
                if ret != esp_idf_sys::ESP_OK {
                    unsafe { esp_idf_sys::adc_oneshot_del_unit(handle) };
                    return Err(AppError::Hal(format!(
                        "adc channel {channel} config failed: 0x{ret:08X}"
                    )));
                }
            }

            log::info!("[adc] OneShot+Spin mode (fallback)");

            Ok(Self {
                handle,
                channels: cfg.channels,
                read_lock: Spin::new(()),
                last: [const { core::sync::atomic::AtomicU16::new(0) }; 6],
                failures: [const { core::sync::atomic::AtomicU32::new(0) }; 6],
            })
        }

        pub fn sample_all(&self) -> [u16; 6] {
            let mut out = [0u16; 6];
            let _guard = self.read_lock.lock();
            for (i, sample) in out.iter_mut().enumerate() {
                *sample = match self.read_raw(i) {
                    Ok(value) => {
                        self.last[i].store(value, core::sync::atomic::Ordering::Release);
                        self.failures[i].store(0, core::sync::atomic::Ordering::Relaxed);
                        value
                    }
                    Err(e) => {
                        let failures = self.failures[i]
                            .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                            .saturating_add(1);
                        if failures == 1 || failures.is_multiple_of(100) {
                            log::warn!("[adc] read ch{i} failed (attempt={failures}): {e:?}");
                        }
                        self.last[i].load(core::sync::atomic::Ordering::Acquire)
                    }
                };
            }
            out
        }

        pub fn sample(&self, idx: usize) -> u16 {
            if idx >= 6 {
                return 0;
            }
            let _guard = self.read_lock.lock();
            match self.read_raw(idx) {
                Ok(value) => {
                    self.last[idx].store(value, core::sync::atomic::Ordering::Release);
                    self.failures[idx].store(0, core::sync::atomic::Ordering::Relaxed);
                    value
                }
                Err(e) => {
                    let failures = self.failures[idx]
                        .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                        .saturating_add(1);
                    if failures == 1 || failures.is_multiple_of(100) {
                        log::warn!("[adc] read ch{idx} failed (attempt={failures}): {e:?}");
                    }
                    self.last[idx].load(core::sync::atomic::Ordering::Acquire)
                }
            }
        }

        fn read_raw(&self, idx: usize) -> Result<u16, i32> {
            let mut value = 0i32;
            let ret = unsafe {
                esp_idf_sys::adc_oneshot_read(
                    self.handle,
                    self.channels[idx] as esp_idf_sys::adc_channel_t,
                    &mut value,
                )
            };
            if ret == esp_idf_sys::ESP_OK {
                Ok(value.clamp(0, u16::MAX as i32) as u16)
            } else {
                Err(ret)
            }
        }
    }

    impl Drop for AdcOneShot {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe { esp_idf_sys::adc_oneshot_del_unit(self.handle) };
                self.handle = std::ptr::null_mut();
            }
        }
    }
}

#[cfg(not(feature = "adc-continuous"))]
use adc_oneshot::AdcOneShot;
