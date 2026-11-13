#!/usr/bin/env python3
"""Run the production IO function-table parser and command match on host."""
from __future__ import annotations

import os
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def item(text: str, marker: str) -> str:
    start = text.index(marker)
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


control = (ROOT / "src/control_logic.rs").read_text()
source = f'''#![allow(dead_code)]
mod heapless {{
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Vec<T, const N: usize>(std::vec::Vec<T>);
    impl<T, const N: usize> Vec<T, N> {{
        pub fn new() -> Self {{ Self(std::vec::Vec::new()) }}
        pub fn push(&mut self, value: T) -> Result<(), T> {{
            if self.0.len() == N {{ Err(value) }} else {{ self.0.push(value); Ok(()) }}
        }}
    }}
    impl<T, const N: usize> std::ops::Deref for Vec<T, N> {{
        type Target = std::vec::Vec<T>;
        fn deref(&self) -> &Self::Target {{ &self.0 }}
    }}
}}
const CONFIG_BASE: u16 = 2300;
const HOLDING_BASE: u16 = 2176;
const MAX_LOGICS: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlSpec {{ value: u16, feedback: u16, q_bits: u64, i_bits: u64 }}
#[derive(Clone, Debug, PartialEq, Eq)]
struct IoLogic {{
    logic_id: u16, controllable: bool, control_addr: u16, feedback_addr: u16,
    q_points: heapless::Vec<u8, 64>, i_points: heapless::Vec<u8, 64>,
    delay_ms: u32, pulse_ms: u32, controls: Vec<ControlSpec>,
}}
#[derive(Clone, Debug, PartialEq, Eq)]
struct AnalogChannel;
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicTable {{ entries: Vec<IoLogic>, analog_channels: Vec<AnalogChannel> }}
impl Default for LogicTable {{
    fn default() -> Self {{ Self {{ entries: Vec::new(), analog_channels: Vec::new() }} }}
}}
#[derive(Clone, Copy, Debug)]
struct Command {{ logic_id: u16, q_scope: u64, q_mask: u64, delay_ms: u32, pulse_ms: u32 }}
fn parse_channels(_: &[u16], _: usize, _: &mut Vec<AnalogChannel>) {{}}
fn rs485_channel_header(_: &[u16], _: usize) -> Option<usize> {{ None }}
{item(control, "fn holding_word(")}
{item(control, "fn bit_words(")}
{item(control, "fn seconds_to_millis(")}
{item(control, "fn parse_table(")}
{item(control, "fn point_scope(")}
{item(control, "fn command_for_logic(")}
{item(control, "fn feedback_from_table(")}
#[cfg(test)]
mod tests {{
    use super::*;
    #[test]
    {item(control, "fn parse_io_control_address_width_zero_compatibly_and_match_q6_q7(")}
}}
'''

with tempfile.TemporaryDirectory(prefix="imc-control-parser-") as temp:
    rust = Path(temp) / "control_parser.rs"
    binary = Path(temp) / "tests"
    rust.write_text(source)
    subprocess.run(["rustc", "+stable", "--edition=2024", "--test", str(rust), "-o", str(binary)], check=True, env=os.environ.copy())
    subprocess.run([str(binary), "--nocapture"], check=True)
