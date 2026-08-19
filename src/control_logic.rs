//! MCA legacy IO control-word executor.
//!
//! The original firmware executes an IO control word from the variable-length
//! 2300+ configuration table. This module preserves that behavior without
//! blocking Modbus: Q points are interlocked, delay/pulse are scheduled, and
//! feedback words are derived from configured I/Q points.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use crate::bus::IO;
use crate::bus::storage_state::storage_read;
use crate::sync::{MainLoopCell, MpscRing};

const CONFIG_BASE: u16 = 2300;
const HOLDING_BASE: u16 = crate::config::regs::HOLD_PXX_BASE;
const MAX_LOGICS: usize = 32;
const MAX_SCHEDULED: usize = 64;

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicTable {
    entries: Vec<IoLogic>,
}

impl Default for LogicTable {
    fn default() -> Self {
        // A full table is larger than CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL, so one
        // deterministic allocation lands in PSRAM instead of several small growth
        // allocations consuming scarce internal SRAM.
        Self {
            entries: Vec::with_capacity(MAX_LOGICS),
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

#[inline]
fn now_ms() -> u64 {
    START.elapsed().as_millis() as u64
}

pub fn init() {
    let _ = SCHEDULED.init(Vec::with_capacity(MAX_SCHEDULED));
    refresh_table();
    let count = TABLE.read_with(|table| table.entries.len()).unwrap_or(0);
    log::info!(
        "[control] MCA IO control table loaded: {} logic records",
        count
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
        let Some(attribute) = holding_word(record_words, start_addr) else {
            continue;
        };
        let attribute = attribute.to_be_bytes()[1];
        if attribute != 1 && attribute != 5 {
            continue;
        }

        // IO records begin with icon/label/name followed by Q and I point tables.
        let Some(q_header) = holding_word(record_words, start_addr + 4) else {
            continue;
        };
        let [q_bytes, q_count] = q_header.to_be_bytes();
        let q_words = (usize::from(q_bytes).saturating_add(1)) / 2;
        let base_index = usize::from(start_addr - HOLDING_BASE);
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
    TABLE.write(table);
    TABLE_GENERATION.store(generation, Ordering::Release);
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
            if let Some(command) = command_for_logic(logic, address, value) {
                if !COMMANDS.try_enqueue(command) {
                    log::warn!(
                        "[control] command queue full; dropped logic={} address={} value={}",
                        logic.logic_id,
                        address,
                        value
                    );
                }
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
}
