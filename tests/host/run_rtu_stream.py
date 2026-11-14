#!/usr/bin/env python3
"""Run the production RTU echo/concatenated-frame scanner on the host."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
slave = (ROOT / "src/modbus/rtu_slave.rs").read_text()
shared = (ROOT / "src/modbus/shared.rs").read_text()


def item(source: str, marker: str) -> str:
    start = source.index(marker)
    opening = source.index("{", start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


source = """#![allow(dead_code)]
""" + "\n".join(
    item(shared, marker)
    for marker in (
        "const fn build_crc16_table(",
        "fn modbus_crc16(",
    )
) + "\nconst MODBUS_CRC16_TABLE: [u16; 256] = build_crc16_table();\n" + item(
    slave, "fn find_valid_request("
) + r'''
fn append_crc(frame: &mut Vec<u8>) {
    let crc = modbus_crc16(frame);
    frame.push(crc as u8);
    frame.push((crc >> 8) as u8);
}
#[test]
fn test_long_fc03_response_echo_then_request_is_found() {
    let mut response = vec![1, 3, 200];
    for value in 0..100u16 { response.extend_from_slice(&value.to_be_bytes()); }
    append_crc(&mut response);
    assert_eq!(response.len(), 205);
    let request = [1, 3, 0x08, 0xFC, 0, 100, 0x86, 0x71];
    let mut batch = response;
    batch.extend_from_slice(&request);
    assert_eq!(find_valid_request(&batch, 0, 1), Some((205, 8)));
}
#[test]
fn test_exception_echo_is_skipped_without_response_loop() {
    let mut exception = vec![1, 0x83, 1];
    append_crc(&mut exception);
    assert_eq!(find_valid_request(&exception, 0, 1), None);
}
#[test]
fn test_misaligned_request_is_found_and_bad_crc_rejected() {
    let mut batch = vec![0xAA, 0x55, 0xCC];
    batch.extend_from_slice(&[1, 3, 0x08, 0xFC, 0, 100, 0x86, 0x71]);
    assert_eq!(find_valid_request(&batch, 0, 1), Some((3, 8)));
    batch[10] ^= 1;
    assert_eq!(find_valid_request(&batch, 0, 1), None);
}
'''
with tempfile.TemporaryDirectory(prefix="imc-rtu-stream-") as name:
    path = Path(name)
    (path / "tests.rs").write_text(source)
    binary = path / "tests"
    subprocess.run(
        ["rustc", "+stable", "--edition=2024", "--test", str(path / "tests.rs"), "-o", str(binary)],
        check=True,
    )
    subprocess.run([str(binary)], check=True, timeout=30)
