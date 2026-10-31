#!/usr/bin/env python3
"""Run the real NVS adapter with instrumented IDF calls on the host.

This verifies adapter ordering and failure propagation, not physical Flash or
the IDF storage engine. Those require the separate device acceptance run.
"""

import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def main():
    source = r'''
extern crate self as esp_idf_sys;
extern crate self as esp_idf_svc;

pub const ESP_ERR_NVS_INVALID_NAME: i32 = 0x1106;
pub const ESP_ERR_NVS_KEY_TOO_LONG: i32 = 0x1109;
#[derive(Debug, PartialEq, Eq)]
pub struct EspError(i32);
impl EspError {
    pub const fn from_infallible<const E: i32>() -> Self { Self(E) }
    pub fn convert(code: i32) -> Result<(), Self> {
        if code == 0 { Ok(()) } else { Err(Self(code)) }
    }
    pub fn code(&self) -> i32 { self.0 }
}
pub mod handle {
    pub trait RawHandle { fn handle(&self) -> u32; }
}
pub mod nvs {
    pub struct EspDefaultNvs;
    impl crate::handle::RawHandle for EspDefaultNvs {
        fn handle(&self) -> u32 { 42 }
    }
}
#[derive(Default)]
struct Trace {
    calls: Vec<&'static str>,
    key: Vec<u8>,
    payload: Vec<u8>,
    set_error: i32,
    commit_error: i32,
}
static TRACE: std::sync::Mutex<Trace> = std::sync::Mutex::new(Trace {
    calls: Vec::new(), key: Vec::new(), payload: Vec::new(),
    set_error: 0, commit_error: 0,
});
pub unsafe fn nvs_set_blob(handle: u32, key: *const std::ffi::c_char,
    value: *const std::ffi::c_void, len: usize) -> i32 {
    assert_eq!(handle, 42);
    let mut trace = TRACE.lock().unwrap();
    trace.calls.push("set_blob");
    trace.key = unsafe { std::ffi::CStr::from_ptr(key) }.to_bytes().to_vec();
    trace.payload = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) }.to_vec();
    trace.set_error
}
pub unsafe fn nvs_commit(handle: u32) -> i32 {
    assert_eq!(handle, 42);
    let mut trace = TRACE.lock().unwrap();
    trace.calls.push("commit");
    trace.commit_error
}
#[path = "__NVS_STORE__"] mod nvs_store;
#[path = "__PENDING_SAVE__"] mod pending_save;

fn reset() { *TRACE.lock().unwrap() = Trace::default(); }

#[test]
fn test_actual_adapter_commits_after_set_for_all_existing_blob_sizes() {
    for len in [0, 8, 152, 1024, 3010, 4000] {
        reset();
        let bytes = vec![0xA5; len];
        nvs_store::set_blob(&nvs::EspDefaultNvs, "sys_cfg", &bytes).unwrap();
        let trace = TRACE.lock().unwrap();
        assert_eq!(trace.calls, ["set_blob", "commit"]);
        assert_eq!(trace.key, b"sys_cfg");
        assert_eq!(trace.payload, bytes);
    }
}
#[test]
fn test_failed_set_does_not_commit_or_clear_dirty() {
    reset();
    TRACE.lock().unwrap().set_error = 0x1105;
    let dirty = std::sync::atomic::AtomicBool::new(true);
    let lock = std::sync::Mutex::new(());
    let save = || nvs_store::set_blob(&nvs::EspDefaultNvs, "dev_text", &[1; 4000]);
    assert_eq!(pending_save::commit_pending(&dirty, &lock, false, save), Err(EspError(0x1105)));
    assert!(dirty.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(TRACE.lock().unwrap().calls, ["set_blob"]);
    reset();
    pending_save::commit_pending(&dirty, &lock, false, save).unwrap();
    assert!(!dirty.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(TRACE.lock().unwrap().calls, ["set_blob", "commit"]);
}
#[test]
fn test_commit_failure_is_retried_without_a_false_success_cache() {
    reset();
    TRACE.lock().unwrap().commit_error = 0x1234;
    let dirty = std::sync::atomic::AtomicBool::new(true);
    let lock = std::sync::Mutex::new(());
    let save = || nvs_store::set_blob(&nvs::EspDefaultNvs, "do_bits", &[0; 8]);
    assert_eq!(pending_save::commit_pending(&dirty, &lock, false, save), Err(EspError(0x1234)));
    assert!(dirty.load(std::sync::atomic::Ordering::Acquire));
    TRACE.lock().unwrap().commit_error = 0;
    pending_save::commit_pending(&dirty, &lock, false, save).unwrap();
    assert_eq!(TRACE.lock().unwrap().calls, ["set_blob", "commit", "set_blob", "commit"]);
    assert!(!dirty.load(std::sync::atomic::Ordering::Acquire));
}
#[test]
fn test_invalid_key_never_calls_the_driver() {
    reset();
    for key in ["", "bad\0key", "0123456789abcdef"] {
        assert!(nvs_store::set_blob(&nvs::EspDefaultNvs, key, &[1]).is_err());
    }
    assert!(TRACE.lock().unwrap().calls.is_empty());
}
#[test]
fn test_new_mutation_during_save_remains_dirty() {
    reset();
    let dirty = std::sync::atomic::AtomicBool::new(true);
    pending_save::commit_pending(&dirty, &std::sync::Mutex::new(()), false, || {
        nvs_store::set_blob(&nvs::EspDefaultNvs, "sys_cfg", &[1; 152])?;
        dirty.store(true, std::sync::atomic::Ordering::Release);
        Ok::<(), EspError>(())
    }).unwrap();
    assert!(dirty.load(std::sync::atomic::Ordering::Acquire));
}
'''
    source = source.replace("__NVS_STORE__", str(ROOT / "src/device/nvs_store.rs"))
    source = source.replace("__PENDING_SAVE__", str(ROOT / "src/device/pending_save.rs"))
    with tempfile.TemporaryDirectory(prefix="gateway-nvs-host-") as temporary:
        root = Path(temporary)
        (root / "test.rs").write_text(source)
        subprocess.run(["rustc", "+stable", "--edition=2024", "--test",
                        str(root / "test.rs"), "-o", str(root / "tests")], check=True)
        subprocess.run([str(root / "tests"), "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
