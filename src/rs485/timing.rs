//! UART timing in explicit units. Pure production code exercised on the host.
const MAX_CONFIGURED_WAIT_MS: u64 = 2_000;
const MAX_VALID_FRAME_WAIT_MS: u64 = 3_000;

/// Modbus Serial Line v1.02: 3.5 * 11 bits below/equal 19200 baud;
/// above 19200 the recommended fixed t3.5 is 1750 microseconds.
pub fn silence_us(baud: u32) -> u32 {
    if baud > 19_200 {
        1_750
    } else {
        38_500_000u32.div_ceil(baud.max(1))
    }
}

pub fn silence_timeout_ms(baud: u32, tick_ms: u64) -> u64 {
    let tick_ms = tick_ms.max(1);
    u64::from(silence_us(baud)).div_ceil(tick_ms.saturating_mul(1_000)) * tick_ms
}

/// One blocking receive cannot exceed the remaining frame budget rounded to
/// one scheduler tick. Zero remaining budget means no further blocking read.
pub fn inter_byte_wait_ticks(baud: u32, remaining_us: u64, tick_ms: u64) -> u32 {
    if remaining_us == 0 {
        return 0;
    }
    let tick_us = tick_ms.max(1).saturating_mul(1_000);
    // A FreeRTOS N-tick block can start just before a tick and return after
    // N-1 whole ticks. One extra tick guarantees the complete silence window.
    let silence_ticks = u64::from(silence_us(baud))
        .div_ceil(tick_us)
        .saturating_add(1);
    let remaining_ticks = remaining_us.div_ceil(tick_us);
    silence_ticks.min(remaining_ticks).min(u64::from(u32::MAX)) as u32
}

/// Deliver hardware FIFO batches well before the software silence timer can
/// expire. Default 120-byte batching hides an in-progress 9600-baud frame for
/// 125ms. Adaptive batching stays below half t3.5 (at most 1ms), avoiding both
/// false gaps and one-interrupt-per-byte overhead at high baud.
pub fn rx_fifo_threshold(baud: u32, bits_per_char: u32) -> u32 {
    let budget_us = (u64::from(silence_us(baud)) / 2).clamp(1, 1_000);
    (u64::from(baud).saturating_mul(budget_us) / (u64::from(bits_per_char.max(1)) * 1_000_000))
        .clamp(1, 120) as u32
}

/// Keep idle/first-byte policy separate. Once data has started, a legal slow
/// frame needs its actual wire time; 256-byte 8E2 at 1200 exceeds two seconds.
pub fn frame_timeout_ms(
    baud: u32,
    bits_per_char: u32,
    request: usize,
    response: usize,
    configured_ms: u64,
    tick_ms: u64,
) -> u64 {
    let bits = (request as u64)
        .saturating_add(response as u64)
        .saturating_mul(u64::from(bits_per_char.max(1)));
    let wire_ms = bits.saturating_mul(1_000).div_ceil(u64::from(baud.max(1)));
    let minimum = wire_ms
        .saturating_add(silence_timeout_ms(baud, tick_ms))
        .saturating_add(30)
        .min(MAX_VALID_FRAME_WAIT_MS);
    configured_ms.clamp(20, MAX_CONFIGURED_WAIT_MS).max(minimum)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_microseconds_are_converted_before_tick_rounding() {
        assert_eq!(silence_timeout_ms(9600, 1), 5);
        assert_eq!(silence_timeout_ms(1200, 1), 33);
        assert_eq!(silence_timeout_ms(19200, 1), 3);
        assert_eq!(silence_timeout_ms(115200, 1), 2);
        assert_eq!(silence_timeout_ms(9600, 10), 10);
        assert_eq!(silence_timeout_ms(1200, 10), 40);
    }
    #[test]
    fn test_every_supported_baud_and_scheduler_tick_respects_units() {
        for baud in [
            1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200, 128000, 153600, 230400,
            256000, 460800, 921600,
        ] {
            for tick in [1, 2, 10] {
                let ms = silence_timeout_ms(baud, tick);
                assert!(ms * 1000 >= u64::from(silence_us(baud)));
                assert!(ms * 1000 < u64::from(silence_us(baud)) + tick * 1000);
                assert_eq!(ms % tick, 0);
                assert!(ms <= 40, "baud={baud}, tick={tick}, ms={ms}");
            }
        }
    }
    #[test]
    fn test_high_baud_uses_fixed_modbus_gap() {
        for baud in [19201, 38400, 115200, 921600] {
            assert_eq!(silence_us(baud), 1750);
        }
    }
    #[test]
    fn test_blocking_read_is_capped_to_remaining_frame_budget() {
        assert_eq!(inter_byte_wait_ticks(9600, 0, 1), 0);
        assert_eq!(inter_byte_wait_ticks(9600, 100, 1), 1);
        assert_eq!(inter_byte_wait_ticks(9600, 2000, 1), 2);
        assert_eq!(inter_byte_wait_ticks(9600, 250000, 1), 6);
        assert_eq!(inter_byte_wait_ticks(1200, 5000, 1), 5);
        assert_eq!(inter_byte_wait_ticks(1200, 250000, 1), 34);
    }
    #[test]
    fn test_low_baud_long_frame_is_not_truncated_at_250ms_or_two_seconds() {
        let budget = frame_timeout_ms(1200, 12, 8, 255, 250, 1);
        assert!(budget >= 2693 && budget <= 3000, "budget={budget}");
        assert_eq!(frame_timeout_ms(9600, 10, 8, 5, 60000, 1), 2000);
        assert!(frame_timeout_ms(9600, 10, 0, 256, 250, 1) >= 302);
    }
    #[test]
    fn test_fifo_batches_never_hide_in_progress_bytes_past_silence() {
        for baud in [1200, 2400, 4800, 9600, 19200, 38400, 115200, 921600] {
            for bits in [9, 10, 11, 12] {
                let threshold = rx_fifo_threshold(baud, bits);
                assert!((1..=120).contains(&threshold));
                if threshold > 1 {
                    assert!(
                        u64::from(threshold) * u64::from(bits) * 1_000_000
                            <= u64::from(baud) * 1_000
                    );
                    assert!(
                        u64::from(threshold) * u64::from(bits) * 1_000_000
                            < u64::from(baud) * u64::from(silence_us(baud))
                    );
                }
            }
        }
        assert_eq!(rx_fifo_threshold(9600, 10), 1);
        assert_eq!(rx_fifo_threshold(921600, 10), 80);
    }
    #[test]
    fn test_tick_phase_margin_does_not_exceed_remaining_budget() {
        assert_eq!(inter_byte_wait_ticks(115200, 50000, 1), 3);
        assert_eq!(inter_byte_wait_ticks(9600, 50000, 1), 6);
        assert_eq!(inter_byte_wait_ticks(1200, 5000, 1), 5);
    }
    #[test]
    fn test_invalid_inputs_saturate_without_overflow_or_infinite_wait() {
        assert_eq!(silence_us(0), 38500000);
        assert_eq!(silence_timeout_ms(9600, 0), 5);
        assert!(frame_timeout_ms(0, u32::MAX, usize::MAX, usize::MAX, u64::MAX, 1) <= 3000);
    }
}
