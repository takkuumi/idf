//! 高频 IO 状态 - 全无锁 (AtomicBits64 + Atomic 原子)
//!
//! ## 设计
//! - di, do_ 是 64 位位图, 用 [`crate::sync::AtomicBits64`] (xtensa 无 AtomicU64 的真无锁实现)
//! - ai, ao 是小数组, 用 std AtomicU16
//! - sys 是小结构, 用原子字段组合
//!
//! ## 优势
//! - 位图读写真无锁 (CAS + 序列锁), 不持任何锁
//! - 不阻塞 Modbus TCP 等慢任务
//! - 与之前 12KB 大锁相比, 并发度提升 1000 倍

use crate::sync::AtomicBits64;
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, Ordering};

/// DI 状态 (16/48 个数字输入, 用 64 位位图 rom AtomicBits64)
pub struct DiState {
    pub bits: AtomicBits64,
}

/// DO 状态 (16/48 个数字输出, 用 64 位位图)
pub struct DoState {
    pub bits: AtomicBits64,
}

/// AI 状态 (LOOP12: 容量扩至 8 通道, F48=F4 设备需要).
/// F16 设备只填 0..5 (6 通道), F48/F4 设备填 0..7 (8 通道, 对齐 MCA `REG_A08`).
pub struct AiState {
    pub raw: [AtomicU16; 8],
    pub scaled: [AtomicU16; 8],
}

/// AO 状态 (4 通道: 工程量 + LEDC duty)
pub struct AoState {
    pub scaled: [AtomicU16; 4],
    pub duty: [AtomicU32; 4],
}

/// Sys 状态
pub struct SysState {
    pub firmware_version: AtomicU16,
    pub uptime_s: AtomicU32,
    pub reset_count: AtomicU16,
    pub reset_reason: AtomicU8,
    pub reset_request: AtomicU8,
    pub log_level: AtomicU8,
}

/// 计划重启的来源码。使用分散的非零校验值，避免相邻状态的单字节误写被
/// 解释成有效重启请求；主循环只接受这里声明的值。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResetSource(u8);

impl ResetSource {
    pub const BLE_AT: Self = Self(0xA1);
    pub const BLE_OTA: Self = Self(0xB2);
    pub const MODBUS: Self = Self(0xC3);
    pub const WEB: Self = Self(0xD4);
    pub const WEB_CONFIG_SAVED: Self = Self(0xE5);

    pub const fn as_str(self) -> &'static str {
        match self.0 {
            0xA1 => "ble-at",
            0xB2 => "ble-ota",
            0xC3 => "modbus",
            0xD4 => "web",
            0xE5 => "web-config-saved",
            _ => "invalid",
        }
    }

    pub const fn is_valid(raw: u8) -> bool {
        matches!(raw, 0xA1 | 0xB2 | 0xC3 | 0xD4 | 0xE5)
    }

    pub const fn label(raw: u8) -> &'static str {
        match raw {
            0xA1 => "ble-at",
            0xB2 => "ble-ota",
            0xC3 => "modbus",
            0xD4 => "web",
            0xE5 => "web-config-saved",
            _ => "invalid",
        }
    }
}

impl Default for DiState {
    fn default() -> Self {
        Self {
            bits: AtomicBits64::new(0),
        }
    }
}

impl Default for DoState {
    fn default() -> Self {
        Self {
            bits: AtomicBits64::new(0),
        }
    }
}

impl Default for AiState {
    fn default() -> Self {
        Self {
            raw: [const { AtomicU16::new(0) }; 8],
            scaled: [const { AtomicU16::new(0) }; 8],
        }
    }
}

impl Default for AoState {
    fn default() -> Self {
        Self {
            scaled: [const { AtomicU16::new(0) }; 4],
            duty: [const { AtomicU32::new(0) }; 4],
        }
    }
}

impl SysState {
    pub const DEFAULT_LOG_LEVEL: u8 = 2;
}

impl Default for SysState {
    fn default() -> Self {
        Self {
            firmware_version: AtomicU16::new(0),
            uptime_s: AtomicU32::new(0),
            reset_count: AtomicU16::new(0),
            reset_reason: AtomicU8::new(0),
            reset_request: AtomicU8::new(0),
            log_level: AtomicU8::new(Self::DEFAULT_LOG_LEVEL),
        }
    }
}

// === 便利操作 ===

impl DiState {
    pub fn get_bit(&self, ch: usize) -> bool {
        self.bits.get_bit(ch)
    }

    /// CAS-loop 单位写 (多写者安全).
    pub fn set_bit(&self, ch: usize, value: bool) {
        let _ = self.bits.set_bit(ch, value);
    }

    pub fn store_bits(&self, bits: u64) {
        self.bits.store_bits(bits);
    }

    pub fn load_bits(&self) -> u64 {
        self.bits.load_bits()
    }
}

impl DoState {
    pub fn get_bit(&self, ch: usize) -> bool {
        self.bits.get_bit(ch)
    }

    /// CAS-loop 单位写 (多写者安全).
    pub fn set_bit(&self, ch: usize, value: bool) {
        let _ = self.bits.set_bit(ch, value);
    }

    pub fn store_bits(&self, bits: u64) {
        self.bits.store_bits(bits);
    }

    /// 原子替换一组 DO 位，供 Modbus FC=0F 批量写一次提交。
    pub fn mask_replace(&self, mask: u64, value: u64) {
        self.bits.mask_replace(mask, value);
    }

    pub fn load_bits(&self) -> u64 {
        self.bits.load_bits()
    }
}

impl AiState {
    pub fn set_raw(&self, ch: usize, value: u16) {
        // LOOP14: ch < self.raw.len() 替代硬编码 ch < 6 — F4 有 8 通道 AI
        if ch < self.raw.len() {
            self.raw[ch].store(value, Ordering::Release);
        }
    }

    pub fn get_raw(&self, ch: usize) -> u16 {
        if ch < self.raw.len() {
            self.raw[ch].load(Ordering::Acquire)
        } else {
            0
        }
    }

    pub fn set_scaled(&self, ch: usize, value: u16) {
        if ch < self.scaled.len() {
            self.scaled[ch].store(value, Ordering::Release);
        }
    }

    pub fn get_scaled(&self, ch: usize) -> u16 {
        if ch < self.scaled.len() {
            self.scaled[ch].load(Ordering::Acquire)
        } else {
            0
        }
    }

    pub fn read_all_raw(&self, buf: &mut [u16]) {
        for (i, dst) in buf.iter_mut().enumerate() {
            *dst = self.get_raw(i);
        }
    }

    pub fn read_all_scaled(&self, buf: &mut [u16]) {
        for (i, dst) in buf.iter_mut().enumerate() {
            *dst = self.get_scaled(i);
        }
    }
}

impl AoState {
    pub fn set_scaled(&self, ch: usize, value: u16) {
        if ch < 4 {
            self.scaled[ch].store(value, Ordering::Release);
        }
    }

    pub fn get_scaled(&self, ch: usize) -> u16 {
        if ch < 4 {
            self.scaled[ch].load(Ordering::Acquire)
        } else {
            0
        }
    }

    pub fn set_duty(&self, ch: usize, value: u32) {
        if ch < 4 {
            self.duty[ch].store(value, Ordering::Release);
        }
    }

    pub fn get_duty(&self, ch: usize) -> u32 {
        if ch < 4 {
            self.duty[ch].load(Ordering::Acquire)
        } else {
            0
        }
    }

    pub fn read_all_scaled(&self, buf: &mut [u16]) {
        for (i, dst) in buf.iter_mut().enumerate() {
            *dst = self.get_scaled(i);
        }
    }
}

impl SysState {
    pub fn get_fw_version(&self) -> u16 {
        self.firmware_version.load(Ordering::Acquire)
    }

    pub fn set_fw_version(&self, v: u16) {
        self.firmware_version.store(v, Ordering::Release);
    }

    pub fn get_uptime(&self) -> u32 {
        self.uptime_s.load(Ordering::Acquire)
    }

    pub fn set_uptime(&self, v: u32) {
        self.uptime_s.store(v, Ordering::Release);
    }

    pub fn get_reset_count(&self) -> u16 {
        self.reset_count.load(Ordering::Acquire)
    }

    pub fn set_reset_count(&self, v: u16) {
        self.reset_count.store(v, Ordering::Release);
    }

    pub fn get_reset_reason(&self) -> u8 {
        self.reset_reason.load(Ordering::Acquire)
    }

    pub fn set_reset_reason(&self, v: u8) {
        self.reset_reason.store(v, Ordering::Release);
    }

    pub fn request_reset(&self, source: ResetSource) {
        self.reset_request.store(source.0, Ordering::Release);
    }

    /// 一次性取得重启请求的原始校验码。调用方只接受 [`ResetSource::is_valid`]
    /// 能识别的值，未知值必须告警但不得重启。
    pub fn take_reset_request(&self) -> u8 {
        self.reset_request.swap(0, Ordering::AcqRel)
    }

    pub fn get_log_level(&self) -> u8 {
        self.log_level.load(Ordering::Acquire)
    }

    pub fn set_log_level(&self, v: u8) {
        self.log_level.store(v, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{ResetSource, SysState};
    use std::sync::atomic::Ordering;

    #[test]
    fn test_reset_request_is_source_tagged_and_consumed_once() {
        let state = SysState::default();
        state.request_reset(ResetSource::MODBUS);
        let raw = state.take_reset_request();
        assert!(ResetSource::is_valid(raw));
        assert_eq!(ResetSource::label(raw), "modbus");
        assert_eq!(state.take_reset_request(), 0);
    }

    #[test]
    fn test_invalid_reset_request_never_becomes_planned_reset() {
        let state = SysState::default();
        state.reset_request.store(1, Ordering::Release);
        let raw = state.take_reset_request();
        assert_eq!(raw, 1);
        assert!(!ResetSource::is_valid(raw));
        assert_eq!(state.take_reset_request(), 0);
    }
}
