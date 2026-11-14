//! Length-aware Modbus RTU framing for transparent radio packet gaps.
//! Wired RS485 keeps its t3.5 framing; this policy is only used by LoRa UART0.
/// E22-T supports air rates down to 2.4 kbps and packet sizes as small as
/// 32 bytes; transparent UART output can therefore contain multi-second gaps.
pub const RADIO_PACKET_GAP_TIMEOUT_MS: u64 = 6_000;
/// Absolute frame cap keeps incomplete RF traffic from holding the UART worker
/// indefinitely; expected short frames still return as soon as their length arrives.
pub const RADIO_FRAME_TIMEOUT_MS: u64 = 12_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Request,
    Response,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    Header,
    Known(usize),
    Unknown,
    Invalid,
}

/// Next bounded read step for a length-aware RTU receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadPlan {
    NeedBytes(usize),
    Complete,
    GapDelimited,
}

pub fn read_plan(bytes: &[u8], direction: Direction, capacity: usize) -> ReadPlan {
    match expected(bytes, direction, capacity) {
        Expected::Header => ReadPlan::NeedBytes(1),
        Expected::Known(length) if bytes.len() < length => {
            ReadPlan::NeedBytes(length - bytes.len())
        }
        Expected::Known(_) => ReadPlan::Complete,
        Expected::Unknown | Expected::Invalid => ReadPlan::GapDelimited,
    }
}

pub fn expected(bytes: &[u8], direction: Direction, capacity: usize) -> Expected {
    if capacity < 2 {
        return Expected::Invalid;
    }
    if bytes.len() < 2 {
        return Expected::Header;
    }
    let function = bytes[1];
    let length = match direction {
        Direction::Request => match function {
            1..=6 => 8,
            15 | 16 => {
                if bytes.len() < 7 {
                    return Expected::Header;
                }
                9 + usize::from(bytes[6])
            }
            _ => return Expected::Unknown,
        },
        Direction::Response => match function {
            f if f & 0x80 != 0 => 5,
            1..=4 => {
                if bytes.len() < 3 {
                    return Expected::Header;
                }
                5 + usize::from(bytes[2])
            }
            5 | 6 | 15 | 16 => 8,
            _ => return Expected::Unknown,
        },
    };
    if length > capacity || length > 256 {
        Expected::Invalid
    } else {
        Expected::Known(length)
    }
}

pub fn incomplete(expected: Expected, received: usize) -> bool {
    match expected {
        Expected::Header => true,
        Expected::Known(n) => received < n,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_radio_packet_and_frame_waits_are_bounded_and_ordered() {
        assert!(RADIO_PACKET_GAP_TIMEOUT_MS >= 5_000);
        assert!(RADIO_FRAME_TIMEOUT_MS >= RADIO_PACKET_GAP_TIMEOUT_MS);
        assert!(RADIO_FRAME_TIMEOUT_MS < 30_000);
    }

    #[test]
    fn test_fixed_requests_and_write_responses() {
        for f in 1..=6 {
            assert_eq!(
                expected(&[1, f], Direction::Request, 256),
                Expected::Known(8)
            );
        }
        for f in [5, 6, 15, 16] {
            assert_eq!(
                expected(&[1, f], Direction::Response, 256),
                Expected::Known(8)
            );
        }
    }
    #[test]
    fn test_fc16_can_span_radio_packets_without_early_termination() {
        let mut frame = [0u8; 255];
        frame[..7].copy_from_slice(&[1, 16, 0x10, 0, 0, 123, 246]);
        for n in 0..7 {
            assert_eq!(
                expected(&frame[..n], Direction::Request, 256),
                Expected::Header
            );
        }
        for n in 7..255 {
            let e = expected(&frame[..n], Direction::Request, 256);
            assert_eq!(e, Expected::Known(255));
            assert!(incomplete(e, n));
        }
        assert!(!incomplete(expected(&frame, Direction::Request, 256), 255));
    }
    #[test]
    fn test_fc03_max_response_and_exceptions() {
        assert_eq!(
            expected(&[1, 3], Direction::Response, 256),
            Expected::Header
        );
        assert_eq!(
            expected(&[1, 3, 250], Direction::Response, 256),
            Expected::Known(255)
        );
        assert_eq!(
            expected(&[1, 4, 250], Direction::Response, 256),
            Expected::Known(255)
        );
        assert_eq!(
            expected(&[1, 0x83], Direction::Response, 256),
            Expected::Known(5)
        );
    }
    #[test]
    fn test_read_plan_stops_at_exact_fc16_and_fc03_lengths() {
        let mut request = [0u8; 255];
        request[..7].copy_from_slice(&[1, 16, 0x10, 0, 0, 123, 246]);
        for received in 0..7 {
            assert_eq!(
                read_plan(&request[..received], Direction::Request, request.len()),
                ReadPlan::NeedBytes(1)
            );
        }
        assert_eq!(
            read_plan(&request[..7], Direction::Request, request.len()),
            ReadPlan::NeedBytes(248)
        );
        assert_eq!(
            read_plan(&request[..240], Direction::Request, request.len()),
            ReadPlan::NeedBytes(15)
        );
        assert_eq!(
            read_plan(&request, Direction::Request, request.len()),
            ReadPlan::Complete
        );

        let response = [1, 3, 250];
        assert_eq!(
            read_plan(&response, Direction::Response, 256),
            ReadPlan::NeedBytes(252)
        );
    }

    #[test]
    fn test_read_plan_uses_gap_fallback_for_unknown_or_invalid_frames() {
        assert_eq!(
            read_plan(&[1, 0x7f], Direction::Request, 256),
            ReadPlan::GapDelimited
        );
        assert_eq!(
            read_plan(&[1, 16, 0, 0, 0, 0, 255], Direction::Request, 256),
            ReadPlan::GapDelimited
        );
    }

    #[test]
    fn test_unknown_functions_keep_gap_framing_and_invalid_lengths_are_bounded() {
        assert_eq!(
            expected(&[1, 0x7f], Direction::Request, 256),
            Expected::Unknown
        );
        assert_eq!(
            expected(&[1, 16, 0, 0, 0, 0, 255], Direction::Request, 256),
            Expected::Invalid
        );
        assert_eq!(
            expected(&[1, 4, 255], Direction::Response, 256),
            Expected::Invalid
        );
        assert_eq!(expected(&[1, 3], Direction::Request, 4), Expected::Invalid);
        assert_eq!(expected(&[], Direction::Request, 0), Expected::Invalid);
    }
}
