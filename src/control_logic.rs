//! MCA legacy IO control-word executor.
//!
//! The original firmware executes an IO control word from the variable-length
//! 2300+ configuration table. This module preserves that behavior without
//! blocking Modbus: Q points are interlocked, delay/pulse are scheduled, and
//! feedback words are derived from configured I/Q points.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::time::Instant;

use crate::bus::IO;
use crate::bus::storage_state::storage_read;
use crate::sync::{MainLoopCell, MpscRing};

const CONFIG_BASE: u16 = 2300;
const HOLDING_BASE: u16 = crate::config::regs::HOLD_PXX_BASE;
const MAX_LOGICS: usize = 32;
const MAX_ANALOG_CHANNELS: usize = 64;
const MAX_SCHEDULED: usize = 64;
const MONITOR_RESULT_COUNT: usize = crate::config::regs::MONITOR_WORD_COUNT as usize;
const USER_RESULT_BASE: u16 = crate::config::regs::HOLD_USER_BASE;
const USER_RESULT_COUNT: usize = crate::config::regs::HOLD_USER_COUNT as usize;
const ANALOG_RESULT_COUNT: usize = MONITOR_RESULT_COUNT + USER_RESULT_COUNT;
const RESULT_MASK_BITS: usize = 32;
const ANALOG_MASK_WORDS: usize = ANALOG_RESULT_COUNT.div_ceil(RESULT_MASK_BITS);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlSpec {
    value: u16,
    feedback: u16,
    q_bits: u64,
    i_bits: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IoLogic {
    logic_id: u16,
    controllable: bool,
    control_addr: u16,
    feedback_addr: u16,
    q_points: heapless::Vec<u8, 64>,
    i_points: heapless::Vec<u8, 64>,
    delay_ms: u32,
    pulse_ms: u32,
    controls: Vec<ControlSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnalogChannel {
    source: u8,
    destination: u16,
    minimum: u16,
    maximum: u16,
    unit_minimum: u16,
    unit_maximum: u16,
    reference: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicTable {
    entries: Vec<IoLogic>,
    analog_channels: Vec<AnalogChannel>,
}

impl Default for LogicTable {
    fn default() -> Self {
        // A full table is larger than CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL, so one
        // deterministic allocation lands in PSRAM instead of several small growth
        // allocations consuming scarce internal SRAM.
        Self {
            entries: Vec::with_capacity(MAX_LOGICS),
            analog_channels: Vec::with_capacity(MAX_ANALOG_CHANNELS),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Command {
    logic_id: u16,
    q_scope: u64,
    q_mask: u64,
    delay_ms: u32,
    pulse_ms: u32,
}

#[derive(Clone, Copy, Debug)]
struct Scheduled {
    logic_id: u16,
    due_ms: u64,
    mask: u64,
    value: u64,
    transient: bool,
}

static TABLE: LazyLock<crate::bus::rcu::Rcu<LogicTable>> =
    LazyLock::new(|| crate::bus::rcu::Rcu::new(LogicTable::default()));
static COMMANDS: LazyLock<MpscRing<Command, 64>> = LazyLock::new(MpscRing::new);
static SCHEDULED: MainLoopCell<Vec<Scheduled>> = MainLoopCell::new();
static CONFIG_GENERATION: AtomicU32 = AtomicU32::new(1);
static TABLE_GENERATION: AtomicU32 = AtomicU32::new(0);
static START: LazyLock<Instant> = LazyLock::new(Instant::now);
static TRANSIENT_MASK: crate::sync::AtomicBits64 = crate::sync::AtomicBits64::new(0);
static ANALOG_VALUES: [AtomicU16; ANALOG_RESULT_COUNT] =
    [const { AtomicU16::new(0) }; ANALOG_RESULT_COUNT];
static ANALOG_VALID: [AtomicU32; ANALOG_MASK_WORDS] =
    [const { AtomicU32::new(0) }; ANALOG_MASK_WORDS];
static ANALOG_VERSION: AtomicU32 = AtomicU32::new(0);
static LAST_ANALOG_UPDATE_MS: AtomicU32 = AtomicU32::new(0);

#[inline]
fn now_ms() -> u64 {
    START.elapsed().as_millis() as u64
}

pub fn init() {
    let _ = SCHEDULED.init(Vec::with_capacity(MAX_SCHEDULED));
    refresh_table();
    let (count, analog_count) = TABLE
        .read_with(|table| (table.entries.len(), table.analog_channels.len()))
        .unwrap_or((0, 0));
    log::info!(
        "[control] MCA table loaded: {} IO records, {} analog channels",
        count,
        analog_count
    );
}
pub fn mark_config_changed() {
    CONFIG_GENERATION.fetch_add(1, Ordering::AcqRel);
}
pub fn transient_mask() -> u64 {
    TRANSIENT_MASK.load_bits()
}

fn holding_word(words: &[u16], address: u16) -> Option<u16> {
    words
        .get(address.checked_sub(HOLDING_BASE)? as usize)
        .copied()
}

fn bit_words(words: &[u16], start: usize, count: usize) -> u64 {
    let mut result = 0u64;
    for logical_word in 0..count.min(4) {
        let Some(word) = words.get(start + count - 1 - logical_word).copied() else {
            break;
        };
        result |= (word as u64) << (logical_word * 16);
    }
    result
}

fn result_index(address: u16) -> Option<usize> {
    if (address as usize) < MONITOR_RESULT_COUNT {
        return Some(address as usize);
    }
    let index = address.checked_sub(USER_RESULT_BASE)? as usize;
    (index < USER_RESULT_COUNT).then_some(MONITOR_RESULT_COUNT + index)
}

fn analog_destination_is_valid(channel: AnalogChannel) -> bool {
    let words = if channel.has_engineering_range() {
        2
    } else {
        1
    };
    let Some(last) = channel.destination.checked_add(words - 1) else {
        return false;
    };
    matches!(
        (result_index(channel.destination), result_index(last)),
        (Some(first), Some(last)) if last - first + 1 == words as usize
    )
}

impl AnalogChannel {
    fn has_engineering_range(self) -> bool {
        self.maximum > self.minimum && self.unit_maximum > self.unit_minimum
    }

    fn encode(self, raw: u16) -> ([u16; 2], usize) {
        if !self.has_engineering_range() {
            return ([raw, 0], 1);
        }

        // Arduino `map()` in MCA works with signed integers. It maps min/max
        // multiplied by 100 and only then converts to float, preserving the
        // legacy two-decimal quantisation exactly.
        let input_min = 4096i64 * i64::from(self.unit_minimum) / i64::from(self.reference);
        let input_max = 4096i64 * i64::from(self.unit_maximum) / i64::from(self.reference) - 1;
        if input_max <= input_min {
            return ([raw, 0], 1);
        }
        let input = i64::from(raw).clamp(input_min, input_max);
        let output_min = i64::from(self.minimum) * 100;
        let output_max = i64::from(self.maximum) * 100;
        let scaled =
            (input - input_min) * (output_max - output_min) / (input_max - input_min) + output_min;
        let bits = ((scaled as f32) / 100.0).to_bits();
        ([(bits >> 16) as u16, bits as u16], 2)
    }
}

fn parse_channels(record_words: &[u16], header: usize, target: &mut Vec<AnalogChannel>) {
    let Some(channel_header) = record_words.get(header).copied() else {
        return;
    };
    let [channel_bytes, channel_count] = channel_header.to_be_bytes();
    let channel_words = usize::from(channel_bytes) / 2;
    if channel_words < 6 || channel_count == 0 {
        return;
    }
    let channels_start = header + 1;
    for channel_index in 0..usize::from(channel_count) {
        if target.len() >= MAX_ANALOG_CHANNELS {
            break;
        }
        let start = channels_start + channel_index * channel_words;
        let Some(channel) = record_words.get(start).copied() else {
            break;
        };
        if channel == 0 || channel > 16 {
            continue;
        }
        let Some(&minimum) = record_words.get(start + 2) else {
            continue;
        };
        let Some(&maximum) = record_words.get(start + 3) else {
            continue;
        };
        let Some(&destination) = record_words.get(start + 4) else {
            continue;
        };
        let unit_minimum = record_words.get(start + 6).copied().unwrap_or(0);
        let unit_maximum = record_words.get(start + 7).copied().unwrap_or(20);
        let parsed = AnalogChannel {
            source: ((channel - 1) / 2) as u8,
            destination,
            minimum,
            maximum,
            unit_minimum,
            unit_maximum,
            reference: if channel.is_multiple_of(2) { 10 } else { 20 },
        };
        if analog_destination_is_valid(parsed) {
            target.push(parsed);
        }
    }
}

fn rs485_channel_header(record_words: &[u16], start: usize) -> Option<usize> {
    let function_header = *record_words.get(start + 5)?;
    let [function_bytes, function_count] = function_header.to_be_bytes();
    if function_bytes != 6 {
        return None;
    }
    let mut cursor = start + 6;
    for _ in 0..function_count {
        let register_header = *record_words.get(cursor + 3)?;
        let [register_bytes, register_count] = register_header.to_be_bytes();
        if register_bytes == 0 || !register_bytes.is_multiple_of(2) {
            return None;
        }
        cursor = cursor
            .checked_add(4 + usize::from(register_bytes / 2) * usize::from(register_count))?;
        if cursor > record_words.len() {
            return None;
        }
    }
    Some(cursor)
}

fn parse_table(words: &[u16]) -> LogicTable {
    let mut table = LogicTable::default();
    let Some(count) = holding_word(words, CONFIG_BASE).map(usize::from) else {
        return table;
    };
    let count = count.min(MAX_LOGICS);
    for index in 0..count {
        let Some(start_offset) = holding_word(words, CONFIG_BASE + 1 + index as u16) else {
            break;
        };
        let end_offset = if index + 1 < count {
            holding_word(words, CONFIG_BASE + 2 + index as u16)
        } else {
            holding_word(words, CONFIG_BASE + 1 + count as u16)
        };
        let Some(end_offset) = end_offset else { break };
        if start_offset >= end_offset {
            continue;
        }
        let Some(start_addr) = CONFIG_BASE.checked_add(start_offset) else {
            continue;
        };
        let Some(end_addr) = CONFIG_BASE.checked_add(end_offset) else {
            continue;
        };
        let Some(end_index) = end_addr
            .checked_sub(HOLDING_BASE)
            .map(usize::from)
            .filter(|&end| end <= words.len())
        else {
            continue;
        };
        let record_words = &words[..end_index];
        let base_index = usize::from(start_addr - HOLDING_BASE);
        let Some(attribute) = holding_word(record_words, start_addr) else {
            continue;
        };
        let attribute = attribute.to_be_bytes()[1];
        if attribute == 3 {
            parse_channels(record_words, base_index + 4, &mut table.analog_channels);
            continue;
        }
        if attribute == 4 {
            if let Some(header) = rs485_channel_header(record_words, base_index) {
                parse_channels(record_words, header, &mut table.analog_channels);
            }
            continue;
        }
        if attribute != 1 && attribute != 5 {
            continue;
        }

        // IO records begin with icon/label/name followed by Q and I point tables.
        let Some(q_header) = holding_word(record_words, start_addr + 4) else {
            continue;
        };
        let [q_bytes, q_count] = q_header.to_be_bytes();
        let q_words = (usize::from(q_bytes).saturating_add(1)) / 2;
        let q_start = base_index + 5;
        let mut q_points = heapless::Vec::<u8, 64>::new();
        for point in 0..usize::from(q_count) {
            let pos = q_start + point * q_words;
            if q_words >= 2 {
                let addr = record_words.get(pos + q_words - 1).copied().unwrap_or(0);
                if (1..=64).contains(&addr) {
                    let _ = q_points.push(addr as u8);
                }
            }
        }
        let i_header_addr = q_start + q_words * usize::from(q_count);
        let Some(i_header) = record_words.get(i_header_addr).copied() else {
            continue;
        };
        let [i_bytes, i_count] = i_header.to_be_bytes();
        let i_words = (usize::from(i_bytes).saturating_add(1)) / 2;
        let i_start = i_header_addr + 1;
        let mut i_points = heapless::Vec::<u8, 64>::new();
        for point in 0..usize::from(i_count) {
            let pos = i_start + point * i_words;
            if i_words >= 2 {
                let addr = record_words.get(pos + i_words - 1).copied().unwrap_or(0);
                if (1..=64).contains(&addr) {
                    let _ = i_points.push(addr as u8);
                }
            }
        }
        let cw_header_addr = i_header_addr + 1 + i_words * usize::from(i_count);
        let Some(cw_header) = record_words.get(cw_header_addr).copied() else {
            continue;
        };
        let [cw_bytes, cw_count] = cw_header.to_be_bytes();
        let cw_words = (usize::from(cw_bytes).saturating_add(1)) / 2;
        if cw_words == 0 || cw_count < 2 {
            continue;
        }
        let control_words_start = cw_header_addr + 1;
        let control_addr = record_words.get(control_words_start).copied().unwrap_or(0);
        let feedback_addr = record_words
            .get(control_words_start + cw_words)
            .copied()
            .unwrap_or(0);
        let control_header_addr = control_words_start + cw_words * usize::from(cw_count);
        let Some(control_header) = record_words.get(control_header_addr).copied() else {
            continue;
        };
        let [control_bytes, control_count] = control_header.to_be_bytes();
        let control_words = (usize::from(control_bytes).saturating_add(1)) / 2;
        if control_words < 6 || control_count == 0 {
            continue;
        }
        let controls_start = control_header_addr + 1;
        let mut controls = Vec::new();
        for control_index in 0..usize::from(control_count) {
            let pos = controls_start + control_index * control_words;
            let Some(&value) = record_words.get(pos) else {
                break;
            };
            let feedback = record_words.get(pos + 1).copied().unwrap_or(0);
            let bit_words_count = (control_words - 4) / 2;
            let q_bits = bit_words(record_words, pos + 3, bit_words_count);
            let i_bits = bit_words(record_words, pos + 3 + bit_words_count, bit_words_count);
            controls.push(ControlSpec {
                value,
                feedback,
                q_bits,
                i_bits,
            });
        }
        if controls.is_empty() {
            continue;
        }
        let tail = controls_start + control_words * usize::from(control_count);
        let Some(delay) = record_words.get(tail).copied() else {
            continue;
        };
        let Some(pulse) = record_words.get(tail + 1).copied() else {
            continue;
        };
        let delay_ms = delay.saturating_mul(1000) as u32;
        let pulse_ms = pulse.saturating_mul(1000) as u32;
        table.entries.push(IoLogic {
            logic_id: index as u16,
            controllable: attribute == 1,
            control_addr,
            feedback_addr,
            q_points,
            i_points,
            delay_ms,
            pulse_ms,
            controls,
        });
    }
    table
}

fn refresh_table() {
    let generation = CONFIG_GENERATION.load(Ordering::Acquire);
    if TABLE_GENERATION.load(Ordering::Acquire) == generation {
        return;
    }
    let table = storage_read()
        .map(|s| parse_table(&s.holding_buf))
        .unwrap_or_default();
    publish_analog_ranges(&table.analog_channels);
    TABLE.write(table);
    TABLE_GENERATION.store(generation, Ordering::Release);
}

fn publish_analog_ranges(channels: &[AnalogChannel]) {
    ANALOG_VERSION.fetch_add(1, Ordering::AcqRel);
    for value in &ANALOG_VALUES {
        value.store(0, Ordering::Release);
    }
    let mut masks = [0u32; ANALOG_MASK_WORDS];
    for channel in channels {
        let start = result_index(channel.destination).expect("validated analog destination");
        let count = if channel.has_engineering_range() {
            2
        } else {
            1
        };
        for index in start..start + count {
            masks[index / RESULT_MASK_BITS] |= 1u32 << (index % RESULT_MASK_BITS);
        }
    }
    for (target, mask) in ANALOG_VALID.iter().zip(masks) {
        target.store(mask, Ordering::Release);
    }
    ANALOG_VERSION.fetch_add(1, Ordering::Release);
}

fn update_analog_results(table: &LogicTable) {
    if table.analog_channels.is_empty() {
        return;
    }
    ANALOG_VERSION.fetch_add(1, Ordering::AcqRel);
    for channel in &table.analog_channels {
        let raw = IO.ai.get_scaled(channel.source as usize);
        let (words, count) = channel.encode(raw);
        let start = result_index(channel.destination).expect("validated analog destination");
        for (offset, value) in words[..count].iter().copied().enumerate() {
            ANALOG_VALUES[start + offset].store(value, Ordering::Release);
        }
    }
    ANALOG_VERSION.fetch_add(1, Ordering::Release);
}

pub fn analog_result_word(address: u16) -> Option<u16> {
    let index = result_index(address)?;
    let mask = ANALOG_VALID[index / RESULT_MASK_BITS].load(Ordering::Acquire);
    if mask & (1u32 << (index % RESULT_MASK_BITS)) == 0 {
        return None;
    }
    Some(ANALOG_VALUES[index].load(Ordering::Acquire))
}

#[inline]
pub fn analog_result_version() -> u32 {
    ANALOG_VERSION.load(Ordering::Acquire)
}

fn point_scope(points: &[u8]) -> u64 {
    points.iter().fold(0u64, |mask, &point| {
        if point == 0 || point > 64 {
            mask
        } else {
            mask | (1u64 << (point - 1))
        }
    })
}

fn command_for_logic(logic: &IoLogic, address: u16, value: u16) -> Option<Command> {
    if !logic.controllable || logic.control_addr != address {
        return None;
    }
    let control = logic
        .controls
        .iter()
        .find(|control| control.value == value)?;
    let q_scope = point_scope(&logic.q_points);
    Some(Command {
        logic_id: logic.logic_id,
        q_scope,
        // MCA stores Q/I bits by physical point number, not by point-list order.
        q_mask: control.q_bits & q_scope,
        delay_ms: logic.delay_ms,
        pulse_ms: logic.pulse_ms,
    })
}

pub fn on_control_word_written(address: u16, value: u16) {
    refresh_table();
    let _ = TABLE.read_with(|table| {
        for logic in &table.entries {
            if let Some(command) = command_for_logic(logic, address, value)
                && !COMMANDS.try_enqueue(command)
            {
                log::warn!(
                    "[control] command queue full; dropped logic={} address={} value={}",
                    logic.logic_id,
                    address,
                    value
                );
            }
        }
    });
}

pub fn feedback_word(address: u16) -> Option<u16> {
    refresh_table();
    let di_bits = IO.di.load_bits();
    let do_bits = IO.do_.load_bits();
    TABLE
        .read_with(|table| feedback_from_table(table, address, di_bits, do_bits))
        .flatten()
}

fn feedback_from_table(
    table: &LogicTable,
    address: u16,
    di_bits: u64,
    do_bits: u64,
) -> Option<u16> {
    for logic in table
        .entries
        .iter()
        .filter(|logic| logic.feedback_addr == address)
    {
        let i_scope = point_scope(&logic.i_points);
        let q_scope = point_scope(&logic.q_points);
        for control in &logic.controls {
            let matched = if control.i_bits != 0 {
                di_bits & i_scope == control.i_bits
            } else {
                do_bits & q_scope == control.q_bits
            };
            if matched {
                return Some(control.feedback);
            }
        }
    }
    None
}

fn apply_output(mask: u64, value: u64, transient: bool) {
    IO.do_.mask_replace(mask, value);
    #[cfg(any(feature = "io-di-do", feature = "f3", feature = "f4"))]
    crate::io::do_::notify();
    if transient {
        TRANSIENT_MASK.mask_replace(mask, mask);
    } else {
        TRANSIENT_MASK.mask_replace(mask, 0);
    }
}

pub fn tick() {
    refresh_table();
    let now = now_ms();
    let last = LAST_ANALOG_UPDATE_MS.load(Ordering::Relaxed);
    if now.saturating_sub(u64::from(last)) >= 100 {
        let _ = TABLE.read_with(update_analog_results);
        LAST_ANALOG_UPDATE_MS.store(now as u32, Ordering::Relaxed);
    }
    while let Some(command) = COMMANDS.dequeue() {
        let due = now_ms().saturating_add(u64::from(command.delay_ms));
        let _ = SCHEDULED.with_mut(|scheduled| {
            scheduled.retain(|item| item.logic_id != command.logic_id);
            if scheduled.len() < MAX_SCHEDULED {
                scheduled.push(Scheduled {
                    logic_id: command.logic_id,
                    due_ms: due,
                    mask: command.q_scope,
                    value: command.q_mask,
                    transient: command.pulse_ms > 0,
                });
            }
            if command.pulse_ms > 0 && scheduled.len() < MAX_SCHEDULED {
                scheduled.push(Scheduled {
                    logic_id: command.logic_id,
                    due_ms: due.saturating_add(u64::from(command.pulse_ms)),
                    mask: command.q_scope,
                    value: 0,
                    transient: false,
                });
            }
        });
    }
    let now = now_ms();
    let _ = SCHEDULED.with_mut(|scheduled| {
        let mut index = 0;
        while index < scheduled.len() {
            if scheduled[index].due_ms <= now {
                let action = scheduled.swap_remove(index);
                apply_output(action.mask, action.value, action.transient);
            } else {
                index += 1;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_bit_words_are_decoded_little_bit_big_word() {
        assert_eq!(bit_words(&[0x0001, 0x0000, 0x8000], 0, 3), 0x8000_0000_0001);
    }
    #[test]
    fn empty_table_is_safe() {
        assert!(parse_table(&[]).entries.is_empty());
    }

    fn logic(logic_id: u16, control_addr: u16, feedback_addr: u16, q_bits: u64) -> IoLogic {
        IoLogic {
            logic_id,
            controllable: true,
            control_addr,
            feedback_addr,
            q_points: heapless::Vec::from_slice(&[3, 6]).unwrap(),
            i_points: heapless::Vec::new(),
            delay_ms: 0,
            pulse_ms: 0,
            controls: vec![ControlSpec {
                value: 7,
                feedback: 9,
                q_bits,
                i_bits: 0,
            }],
        }
    }

    #[test]
    fn control_bits_use_physical_point_numbers() {
        let logic = logic(0, 17, 18, (1u64 << 2) | (1u64 << 5));
        let command = command_for_logic(&logic, 17, 7).unwrap();
        assert_eq!(command.q_scope, (1u64 << 2) | (1u64 << 5));
        assert_eq!(command.q_mask, (1u64 << 2) | (1u64 << 5));
    }

    #[test]
    fn duplicate_control_and_feedback_addresses_do_not_hide_later_logic() {
        let first = logic(0, 17, 18, 1u64 << 2);
        let second = logic(1, 17, 18, 1u64 << 5);
        let table = LogicTable {
            entries: vec![first, second],
            analog_channels: Vec::new(),
        };
        assert_eq!(
            table
                .entries
                .iter()
                .filter_map(|logic| command_for_logic(logic, 17, 7))
                .count(),
            2
        );
        assert_eq!(feedback_from_table(&table, 18, 0, 1u64 << 5), Some(9));
    }

    #[test]
    fn parses_legacy_io_record_and_maps_non_contiguous_points() {
        let mut words = vec![0u16; 256];
        let base = (CONFIG_BASE - HOLDING_BASE) as usize;
        words[base] = 1;
        words[base + 1] = 21;
        words[base + 2] = 100;
        let start = base + 21;
        words[start] = 1;
        words[start + 4] = 0x0402;
        words[start + 5] = 0;
        words[start + 6] = 3;
        words[start + 7] = 0;
        words[start + 8] = 6;
        words[start + 9] = 0x0401;
        words[start + 10] = 0;
        words[start + 11] = 1;
        words[start + 12] = 0x0202;
        words[start + 13] = 17;
        words[start + 14] = 18;
        words[start + 15] = 0x1402;
        words[start + 16] = 0x0101;
        words[start + 17] = 0x0202;
        words[start + 18] = 0x0303;
        words[start + 19] = 0x0404;
        words[start + 20] = 0x0505;
        words[start + 21] = 0x0606;
        words[start + 22] = 0x0707;
        words[start + 23] = 0x0808;
        words[start + 24] = 0x0909;
        words[start + 25] = 0x0A0A;
        words[start + 26] = 0;
        words[start + 27] = 0;
        let table = parse_table(&words);
        assert_eq!(table.entries.len(), 1);
        assert_eq!(table.entries[0].control_addr, 17);
        assert_eq!(table.entries[0].feedback_addr, 18);
        assert_eq!(table.entries[0].q_points.as_slice(), &[3, 6]);
    }

    #[test]
    fn truncated_record_does_not_read_into_following_storage() {
        let mut words = vec![0u16; 256];
        let base = (CONFIG_BASE - HOLDING_BASE) as usize;
        words[base] = 1;
        words[base + 1] = 21;
        words[base + 2] = 30;
        let start = base + 21;
        words[start] = 1;
        words[start + 4] = 0x0401;
        assert!(parse_table(&words).entries.is_empty());
    }

    fn analog_words(attribute: u8, channel: u16, destination: u16) -> Vec<u16> {
        let mut words = vec![0u16; 256];
        let base = (CONFIG_BASE - HOLDING_BASE) as usize;
        words[base] = 1;
        words[base + 1] = 3;
        words[base + 2] = 16;
        let start = base + 3;
        words[start] = u16::from(attribute);
        words[start + 4] = 0x1001;
        words[start + 5] = channel;
        words[start + 6] = 0;
        words[start + 7] = 0;
        words[start + 8] = 100;
        words[start + 9] = destination;
        words[start + 10] = 0;
        words[start + 11] = 4;
        words[start + 12] = 20;
        words
    }

    #[test]
    fn parses_legacy_analog_channel_and_odd_channel_uses_20ma_reference() {
        let table = parse_table(&analog_words(3, 1, 4001));
        assert_eq!(table.analog_channels.len(), 1);
        let channel = table.analog_channels[0];
        assert_eq!(channel.source, 0);
        assert_eq!(channel.reference, 20);
        assert_eq!(channel.destination, 4001);
    }

    #[test]
    fn even_channel_uses_10v_reference_and_maps_to_big_endian_f32_words() {
        let channel = AnalogChannel {
            source: 0,
            destination: 2,
            minimum: 0,
            maximum: 100,
            unit_minimum: 0,
            unit_maximum: 10,
            reference: 10,
        };
        let (encoded, count) = channel.encode(2048);
        assert_eq!(count, 2);
        let value = f32::from_bits((u32::from(encoded[0]) << 16) | u32::from(encoded[1]));
        assert!(
            (value - 50.01).abs() < 0.02,
            "legacy integer map value={value}"
        );
    }

    #[test]
    fn invalid_or_absent_range_publishes_raw_value_in_one_word() {
        let channel = AnalogChannel {
            source: 0,
            destination: 127,
            minimum: 0,
            maximum: 0,
            unit_minimum: 0,
            unit_maximum: 20,
            reference: 20,
        };
        assert_eq!(channel.encode(3065), ([3065, 0], 1));
        assert!(analog_destination_is_valid(channel));
    }

    #[test]
    fn two_word_float_cannot_cross_monitor_and_user_result_areas() {
        let channel = AnalogChannel {
            source: 0,
            destination: 127,
            minimum: 0,
            maximum: 100,
            unit_minimum: 4,
            unit_maximum: 20,
            reference: 20,
        };
        assert!(!analog_destination_is_valid(channel));
    }
}
