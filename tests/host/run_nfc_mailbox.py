#!/usr/bin/env python3
"""Execute the production NFC mailbox codec and golden vectors on the host."""

from __future__ import annotations

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


def main() -> None:
    source = (ROOT / "src/nfc/mod.rs").read_text()
    constants = "\n".join(
        line
        for line in source.splitlines()
        if line.startswith(
            (
                "const MAILBOX_",
                "const MB_CTRL_HOST_PUT_MSG",
                "const MB_CTRL_RF_PUT_MSG",
                "const ST25DV16KC_IC_REF",
                "const ST25DV16K_IC_REF",
                "const ST25DV_DEVICE_CODE",
                "const ST25DV_SYSTEM_ADDR_CANDIDATES",
                "const FTM_MB_MODE",
                "const FTM_MB_WDG_MASK",
                "const REG_FTM:",
                "const REG_K_MB_WDG:",
                "const K_MB_WDG_MASK:",
                "const REG_EH_CTRL_DYN:",
                "const REG_MB_CTRL_DYN:",
                "const REG_MB_LEN_DYN:",
                "const REG_MB_DATA_DYN:",
                "const EH_CTRL_VCC_ON:",
                "const MB_CTRL_MB_EN:",
            )
        )
    )
    assembler_struct = item(source, "struct MailboxAssembler ")
    assembler_impl = item(source, "impl MailboxAssembler ")
    fragmenter = item(source, "fn mailbox_fragment(")
    identity_helper = item(source, "const fn is_st25dv16kc_identity(")
    password_helper = item(source, "fn i2c_password_presentation(")
    address_helper = item(source, "const fn st25dv_system_addr_candidates(")
    generation = "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n" + item(source, "enum St25dvGeneration ")
    generation_helper = item(source, "const fn st25dv16k_generation(")
    ftm_helper = item(source, "fn ftm_config_value(")
    status_helpers = "\n".join(
        item(source, marker)
        for marker in (
            "const fn mailbox_is_free(",
            "const fn mailbox_has_rf_message(",
            "const fn mailbox_response_released(",
            "fn take_deferred_mailbox_reset(",
        )
    )
    golden_tests = "\n".join(
        "#[test]\n" + item(source, f"fn {name}(")
        for name in (
            "test_mailbox_envelope_round_trip_direct_and_fragmented",
            "test_mailbox_fragments_match_flutter_golden_wire_vector",
            "test_mailbox_rejects_corruption_and_recovers_after_out_of_order_data",
            "test_mailbox_maximum_frame_round_trip_and_length_rejection",
            "test_mailbox_status_helpers_cover_busy_message_and_release_states",
            "test_direct_mailbox_request_cancels_interrupted_fragment_assembly",
            "test_st25dv16kc_identity_matches_official_datasheet",
            "test_st25dv_system_address_candidates_cover_i2c_cfg_e0",
            "test_ftm_enable_disables_mailbox_watchdog_and_preserves_reserved_bits",
            "test_st25dv16k_generation_rejects_unknown_or_wrong_capacity",
            "test_k_mailbox_mode_preserves_separate_watchdog_register_layout",
            "test_i2c_password_presentation_matches_st25dv_wire_order",
        )
    )

    driver_methods = "\n".join(
        item(source, marker)
        for marker in (
            "fn set_ftm_enabled(",
            "fn enable_mailbox(",
            "fn set_mailbox_enabled(",
            "fn read_rf_mailbox(",
            "fn write_host_mailbox(",
        )
    )
    driver_harness = r"""
extern crate self as log;
#[macro_export]
macro_rules! warn { ($($argument:tt)*) => { { let _ = format_args!($($argument)*); } }; }
mod driver_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::time::Duration;
    struct St25dv {
        generation: St25dvGeneration,
        data_addr_7bit: u8,
        system_addr_7bit: u8,
        registers: RefCell<[u8; 0x2010]>,
        writes: RefCell<std::vec::Vec<(u16, u8)>>,
        ignored_write: Option<u16>,
        fail_read: Option<u16>,
        mailbox: RefCell<std::vec::Vec<u8>>,
        read_order: RefCell<std::vec::Vec<u16>>,
        polls: Cell<usize>,
        nack_polls: usize,
    }
    impl St25dv {
        fn new(generation: St25dvGeneration) -> Self {
            let mut registers = [0; 0x2010];
            registers[REG_EH_CTRL_DYN as usize] = EH_CTRL_VCC_ON;
            registers[REG_FTM as usize] = 0xBE;
            registers[REG_K_MB_WDG as usize] = 0xA7;
            Self { generation, data_addr_7bit: 0x53, system_addr_7bit: 0x57,
                registers: RefCell::new(registers), writes: RefCell::new(std::vec::Vec::new()),
                ignored_write: None, fail_read: None,
                mailbox: RefCell::new(std::vec::Vec::new()), read_order: RefCell::new(std::vec::Vec::new()),
                polls: Cell::new(0), nack_polls: 0 }
        }
        fn read_reg16_at(&self, address: u8, register: u16) -> Result<u8, ()> {
            assert_eq!(address, if register >= 0x2000 { self.data_addr_7bit } else { self.system_addr_7bit });
            self.read_order.borrow_mut().push(register);
            if self.fail_read == Some(register) { return Err(()); }
            let value = self.registers.borrow()[register as usize];
            // ES0616 section 6.1: a 256-byte RF message may release
            // RF_PUT_MSG early when I2C reads MB_LEN_Dyn (0xFF).
            if register == REG_MB_LEN_DYN && value == 255 {
                self.registers.borrow_mut()[REG_MB_CTRL_DYN as usize] &= !MB_CTRL_RF_PUT_MSG;
            }
            Ok(value)
        }
        fn write_bytes_at(&self, address: u8, register: u16, data: &[u8], wait_write: bool) -> Result<(), ()> {
            assert_eq!(address, self.system_addr_7bit);
            assert!(wait_write);
            assert_eq!(data.len(), 1);
            self.writes.borrow_mut().push((register, data[0]));
            if self.ignored_write != Some(register) { self.registers.borrow_mut()[register as usize] = data[0]; }
            Ok(())
        }
        fn write_dynamic_at(&self, address: u8, register: u16, data: &[u8]) -> Result<(), ()> {
            assert_eq!(address, self.data_addr_7bit);
            self.writes.borrow_mut().push((register, data[0]));
            if register == REG_MB_DATA_DYN {
                *self.mailbox.borrow_mut() = data.to_vec();
            } else {
                assert_eq!(register, REG_MB_CTRL_DYN);
                assert_eq!(data.len(), 1);
                if self.ignored_write != Some(register) { self.registers.borrow_mut()[register as usize] = data[0]; }
            }
            Ok(())
        }
        fn read_bytes_at(&self, address: u8, register: u16, data: &mut [u8]) -> Result<(), ()> {
            assert_eq!(address, self.data_addr_7bit);
            assert_eq!(register, REG_MB_DATA_DYN);
            self.read_order.borrow_mut().push(register);
            data.copy_from_slice(&self.mailbox.borrow()[..data.len()]);
            Ok(())
        }
        fn probe_address(&self, address: u8) -> bool {
            assert_eq!(address, self.data_addr_7bit);
            self.polls.set(self.polls.get() + 1);
            self.polls.get() > self.nack_polls
        }
""" + driver_methods + r"""
    }
    #[test]
    fn test_k_errata_full_rf_message_is_read_after_length_releases_flag() {
        let device = St25dv::new(St25dvGeneration::K);
        let message: std::vec::Vec<u8> = (0..256).map(|byte| byte as u8).collect();
        *device.mailbox.borrow_mut() = message.clone();
        device.registers.borrow_mut()[REG_MB_CTRL_DYN as usize] = MB_CTRL_MB_EN | MB_CTRL_RF_PUT_MSG;
        device.registers.borrow_mut()[REG_MB_LEN_DYN as usize] = 255;
        let mut output = [0; MAILBOX_MAX_BYTES];
        assert_eq!(device.read_rf_mailbox(&mut output).unwrap(), Some(256));
        assert_eq!(output.as_slice(), message.as_slice());
        assert_eq!(*device.read_order.borrow(), vec![REG_MB_CTRL_DYN, REG_MB_LEN_DYN, REG_MB_DATA_DYN]);
        assert_eq!(device.registers.borrow()[REG_MB_CTRL_DYN as usize] & MB_CTRL_RF_PUT_MSG, 0);
    }
    #[test]
    fn test_k_errata_host_write_polls_ack_without_republishing_message() {
        let mut device = St25dv::new(St25dvGeneration::K);
        device.nack_polls = 3;
        device.write_host_mailbox(&[1,2,3]).unwrap();
        assert_eq!(device.polls.get(), 4);
        assert_eq!(*device.writes.borrow(), vec![(REG_MB_DATA_DYN,1)]);
        assert_eq!(*device.mailbox.borrow(), vec![1,2,3]);
        let mut failed = St25dv::new(St25dvGeneration::K);
        failed.nack_polls = 10;
        assert!(failed.write_host_mailbox(&[4,5]).is_err());
        assert_eq!(failed.polls.get(), 10);
        assert_eq!(failed.writes.borrow().len(), 1, "a failed ACK poll must not replay a possibly delivered command");
    }

    #[test]
    fn test_production_driver_k_and_kc_register_writes_and_idle_wear() {
        for generation in [St25dvGeneration::K, St25dvGeneration::Kc] {
            let device = St25dv::new(generation);
            device.enable_mailbox().unwrap();
            assert_eq!(device.registers.borrow()[REG_MB_CTRL_DYN as usize], 1);
            match generation {
                St25dvGeneration::K => {
                    assert_eq!(device.registers.borrow()[REG_FTM as usize], 0xBF);
                    assert_eq!(device.registers.borrow()[REG_K_MB_WDG as usize], 0xA0);
                }
                St25dvGeneration::Kc => {
                    assert_eq!(device.registers.borrow()[REG_FTM as usize], 0xB1);
                    assert_eq!(device.registers.borrow()[REG_K_MB_WDG as usize], 0xA7,
                        "KC I2C_CFG must not be written as K MB_WDG");
                }
            }
            let writes_after_init = device.writes.borrow().len();
            for _ in 0..1000 { device.enable_mailbox().unwrap(); }
            assert_eq!(device.writes.borrow().len(), writes_after_init, "idle polling must not wear EEPROM");
        }
    }
    #[test]
    fn test_production_driver_requires_vcc_and_propagates_read_errors() {
        let mut device = St25dv::new(St25dvGeneration::K);
        device.registers.borrow_mut()[REG_EH_CTRL_DYN as usize] = 0;
        assert!(device.enable_mailbox().is_err());
        assert!(device.writes.borrow().is_empty());
        device.registers.borrow_mut()[REG_EH_CTRL_DYN as usize] = EH_CTRL_VCC_ON;
        device.fail_read = Some(REG_K_MB_WDG);
        assert!(device.enable_mailbox().is_err());
    }
    #[test]
    fn test_production_driver_rejects_static_and_dynamic_readback_mismatch() {
        for (generation, register) in [
            (St25dvGeneration::Kc, REG_FTM),
            (St25dvGeneration::K, REG_K_MB_WDG),
            (St25dvGeneration::K, REG_MB_CTRL_DYN),
        ] {
            let mut device = St25dv::new(generation);
            device.ignored_write = Some(register);
            assert!(device.enable_mailbox().is_err(), "generation={generation:?} register={register:#06x}");
        }
    }
}
"""

    harness = f"""
#![allow(dead_code)]
mod heapless {{
    #[derive(Debug)]
    pub struct Vec<T, const N: usize>(std::vec::Vec<T>);
    impl<T, const N: usize> Vec<T, N> {{
        pub fn new() -> Self {{ Self(std::vec::Vec::new()) }}
        pub fn len(&self) -> usize {{ self.0.len() }}
        pub fn clear(&mut self) {{ self.0.clear(); }}
        pub fn as_slice(&self) -> &[T] {{ self.0.as_slice() }}
        pub fn push(&mut self, value: T) -> Result<(), ()> {{
            if self.0.len() == N {{ return Err(()); }}
            self.0.push(value);
            Ok(())
        }}
        pub fn extend_from_slice(&mut self, values: &[T]) -> Result<(), ()>
        where T: Copy {{
            if self.0.len() + values.len() > N {{ return Err(()); }}
            self.0.extend_from_slice(values);
            Ok(())
        }}
    }}
    impl<T, const N: usize> std::ops::Deref for Vec<T, N> {{
        type Target = [T];
        fn deref(&self) -> &[T] {{ &self.0 }}
    }}
    impl<T, const N: usize> std::ops::DerefMut for Vec<T, N> {{
        fn deref_mut(&mut self) -> &mut [T] {{ &mut self.0 }}
    }}
    impl<T, const N: usize> IntoIterator for Vec<T, N> {{
        type Item = T;
        type IntoIter = std::vec::IntoIter<T>;
        fn into_iter(self) -> Self::IntoIter {{ self.0.into_iter() }}
    }}
}}
mod modbus {{
    pub mod shared {{
        pub fn modbus_crc16(data: &[u8]) -> u16 {{
            let mut crc = 0xffffu16;
            for &byte in data {{
                crc ^= byte as u16;
                for _ in 0..8 {{
                    crc = if crc & 1 != 0 {{ (crc >> 1) ^ 0xa001 }} else {{ crc >> 1 }};
                }}
            }}
            crc
        }}
    }}
}}
{constants}
{generation}
{generation_helper}
{password_helper}
{assembler_struct}
{assembler_impl}
{fragmenter}
{identity_helper}
{address_helper}
{ftm_helper}
{status_helpers}
{golden_tests}
{driver_harness}
"""
    with tempfile.TemporaryDirectory(prefix="imc-nfc-mailbox-") as temporary:
        source_file = Path(temporary) / "mailbox_tests.rs"
        binary = Path(temporary) / "mailbox_tests"
        source_file.write_text(harness)
        subprocess.run(
            ["rustc", "+stable", "--edition=2024", "--test", str(source_file), "-o", str(binary)],
            check=True,
        )
        subprocess.run([str(binary), "--nocapture"], check=True)


if __name__ == "__main__":
    main()
