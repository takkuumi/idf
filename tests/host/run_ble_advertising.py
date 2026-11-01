#!/usr/bin/env python3
"""Exercise production BLE renewal/rename code with a simulated GAP controller.

Includes the real state module and extracts the unchanged maintenance functions
and GAP callback. Only ESP-IDF calls, clock, config and synchronization are shims.
No device is connected or reset by this test.
"""
from pathlib import Path
import re
import subprocess
import tempfile

from run_production_core import item

ROOT = Path(__file__).resolve().parents[2]

SHIMS = r'''
extern crate self as log;
#[macro_export] macro_rules! info { ($($arg:tt)*) => { let _ = format_args!($($arg)*); }; }
#[macro_export] macro_rules! warn { ($($arg:tt)*) => { let _ = format_args!($($arg)*); }; }
#[macro_export] macro_rules! error { ($($arg:tt)*) => { let _ = format_args!($($arg)*); }; }
#[macro_export] macro_rules! debug { ($($arg:tt)*) => { let _ = format_args!($($arg)*); }; }
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};
use std::ffi::CString;
struct Spin<T>(std::sync::Mutex<T>);
impl<T> Spin<T> {
    const fn new(value: T) -> Self { Self(std::sync::Mutex::new(value)) }
    fn lock(&self) -> std::sync::MutexGuard<'_, T> { self.0.lock().unwrap() }
    fn try_lock(&self) -> Option<std::sync::MutexGuard<'_, T>> { self.0.try_lock().ok() }
}
mod heapless {
    pub struct String<const N: usize>(std::string::String);
    impl<const N: usize> String<N> {
        pub fn new() -> Self { Self(std::string::String::new()) }
        pub fn push_str(&mut self, value: &str) -> Result<(), ()> {
            if self.0.len() + value.len() > N { return Err(()); }
            self.0.push_str(value); Ok(())
        }
        pub fn push(&mut self, value: char) -> Result<(), ()> { self.push_str(&value.to_string()) }
        pub fn as_str(&self) -> &str { &self.0 }
    }
    impl<const N: usize> std::fmt::Display for String<N> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
    }
}
mod bus { pub mod config_state {
    pub struct Config; pub struct State { pub cfg: Config }
    impl Config { pub fn ble_name_str(&self) -> String { super::super::ID.lock().clone() } }
    pub fn config_read_with<T>(read: impl FnOnce(&State) -> T) -> Option<T> { Some(read(&State { cfg: Config })) }
} }
static NOW_MS: AtomicU32 = AtomicU32::new(0);
static STARTS: AtomicU32 = AtomicU32::new(0);
static STOPS: AtomicU32 = AtomicU32::new(0);
static CONFIGS: AtomicU32 = AtomicU32::new(0);
static NAMES: AtomicU32 = AtomicU32::new(0);
static FAIL_NEXT: AtomicU8 = AtomicU8::new(0);
static CONTROLLER_ACTIVE: AtomicBool = AtomicBool::new(false);
static ID: Spin<String> = Spin::new(String::new());
static AIR_NAME: Spin<String> = Spin::new(String::new());
// Names and structures intentionally mirror generated ESP-IDF C bindings.
#[allow(non_camel_case_types, non_upper_case_globals)]
mod esp_idf_sys {
    use super::*;
    pub const ESP_OK: i32 = 0;
    pub const esp_bt_status_t_ESP_BT_STATUS_SUCCESS: i32 = 0;
    pub const esp_ble_adv_type_t_ADV_TYPE_IND: i32 = 0;
    pub const esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC: i32 = 0;
    pub const esp_ble_adv_channel_t_ADV_CHNL_ALL: i32 = 7;
    pub const esp_ble_adv_filter_t_ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY: i32 = 0;
    pub const ESP_BLE_ADV_FLAG_GEN_DISC: u8 = 2;
    pub const ESP_BLE_ADV_FLAG_BREDR_NOT_SPT: u8 = 4;
    pub type esp_gap_ble_cb_event_t = i32;
    pub const esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT: i32 = 1;
    pub const esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_START_COMPLETE_EVT: i32 = 2;
    pub const esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_STOP_COMPLETE_EVT: i32 = 3;
    pub struct esp_ble_adv_params_t {
        pub adv_int_min: u16, pub adv_int_max: u16, pub adv_type: i32,
        pub own_addr_type: i32, pub peer_addr: [u8;6], pub peer_addr_type: i32,
        pub channel_map: i32, pub adv_filter_policy: i32,
    }
    pub struct esp_ble_adv_data_t {
        pub set_scan_rsp: bool, pub include_name: bool, pub include_txpower: bool,
        pub min_interval: i32, pub max_interval: i32, pub appearance: i32,
        pub manufacturer_len: u16, pub p_manufacturer_data: *mut u8,
        pub service_data_len: u16, pub p_service_data: *mut u8,
        pub service_uuid_len: u16, pub p_service_uuid: *mut u8, pub flag: u8,
    }
    #[derive(Clone, Copy)] pub struct Completion { pub status: i32 }
    pub struct esp_ble_gap_cb_param_t {
        pub adv_data_cmpl: Completion, pub adv_start_cmpl: Completion, pub adv_stop_cmpl: Completion,
    }
    fn result(kind: u8) -> i32 {
        assert!(ADV_HEALTH.try_lock().is_some(), "SDK called while holding health lock");
        if FAIL_NEXT.compare_exchange(kind, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() { 0x103 } else { 0 }
    }
    pub unsafe fn esp_timer_get_time() -> i64 { NOW_MS.load(Ordering::Relaxed) as i64 * 1_000 }
    pub unsafe fn esp_ble_gap_set_device_name(name: *const std::ffi::c_char) -> i32 {
        NAMES.fetch_add(1, Ordering::Relaxed);
        let ret = result(1);
        if ret == 0 { *AIR_NAME.lock() = unsafe { std::ffi::CStr::from_ptr(name) }.to_str().unwrap().to_owned(); }
        ret
    }
    pub unsafe fn esp_ble_gap_config_adv_data(data: *mut esp_ble_adv_data_t) -> i32 {
        let data = unsafe { &*data };
        assert!(data.include_name && !data.set_scan_rsp);
        assert!(!data.include_txpower);
        assert_eq!((data.min_interval, data.max_interval, data.appearance), (0,0,0));
        assert_eq!((data.manufacturer_len, data.service_data_len), (0,0));
        assert!(data.p_manufacturer_data.is_null() && data.p_service_data.is_null());
        assert!(!data.p_service_uuid.is_null());
        assert_eq!(data.flag, ESP_BLE_ADV_FLAG_GEN_DISC | ESP_BLE_ADV_FLAG_BREDR_NOT_SPT);
        assert_eq!(data.service_uuid_len, 16);
        CONFIGS.fetch_add(1, Ordering::Relaxed); result(2)
    }
    pub unsafe fn esp_ble_gap_start_advertising(params: *mut esp_ble_adv_params_t) -> i32 {
        let params = unsafe { &*params };
        assert_eq!((params.adv_int_min, params.adv_int_max, params.channel_map), (0x100,0x100,7));
        assert_eq!((params.adv_type,params.own_addr_type,params.peer_addr_type,params.adv_filter_policy), (0,0,0,0));
        assert_eq!(params.peer_addr,[0;6]);
        STARTS.fetch_add(1, Ordering::Relaxed);
        let ret = result(3);
        if ret == 0 { CONTROLLER_ACTIVE.store(true, Ordering::Release); }
        ret
    }
    pub unsafe fn esp_ble_gap_stop_advertising() -> i32 {
        STOPS.fetch_add(1, Ordering::Relaxed);
        let ret = result(4);
        if ret == 0 { CONTROLLER_ACTIVE.store(false, Ordering::Release); }
        ret
    }
}
'''

TESTS = r'''
fn at(now: u32) { NOW_MS.store(now, Ordering::SeqCst); }
fn reset() {
    *ADV_HEALTH.lock() = AdvertisingHealth::new();
    for count in [&STARTS,&STOPS,&CONFIGS,&NAMES] { count.store(0,Ordering::SeqCst); }
    FAIL_NEXT.store(0,Ordering::SeqCst); CONN_ID.store(0xFFFF,Ordering::SeqCst);
    SVC_STARTED.store(true,Ordering::SeqCst); ADV_RECONFIGURE_PENDING.store(false,Ordering::SeqCst);
    PENDING_GAP_NAME_UPDATE.store(false,Ordering::SeqCst); CONTROLLER_ACTIVE.store(false,Ordering::SeqCst);
    *ID.lock() = "Mesh".into(); *AIR_NAME.lock() = "Mesh".into(); at(100);
}
fn event(kind: i32, status: i32) {
    let c=esp_idf_sys::Completion{status};
    let mut p=esp_idf_sys::esp_ble_gap_cb_param_t{adv_data_cmpl:c,adv_start_cmpl:c,adv_stop_cmpl:c};
    unsafe { gap_event_cb(kind,&mut p); }
}
fn running() { reset(); start_advertising(); event(2,0); }
fn poll(now: u32) { at(now); maintain_advertising(); }
#[test] fn test_real_paths_recover_silent_stop_despite_active_flag() {
    running(); CONTROLLER_ACTIVE.store(false,Ordering::SeqCst);
    poll(60_099); assert_eq!(STARTS.load(Ordering::SeqCst),1);
    poll(60_100); assert_eq!(STARTS.load(Ordering::SeqCst),2);
    assert!(CONTROLLER_ACTIVE.load(Ordering::SeqCst));
    event(2,0); poll(60_110); assert_eq!(STARTS.load(Ordering::SeqCst),2);
}
#[test] fn test_real_paths_connected_and_rename_deferred_until_disconnect() {
    running(); CONN_ID.store(0,Ordering::SeqCst); advertising_connected();
    *ID.lock()="12345678".into(); PENDING_GAP_NAME_UPDATE.store(true,Ordering::SeqCst);
    poll(120_100); assert_eq!(STARTS.load(Ordering::SeqCst),1);
    assert_eq!(CONFIGS.load(Ordering::SeqCst),0); assert_eq!(NAMES.load(Ordering::SeqCst),0);
    CONN_ID.store(0xFFFF,Ordering::SeqCst); advertising_disconnected();
    poll(120_110); assert_eq!(&*AIR_NAME.lock(),"M12345678");
    assert_eq!(CONFIGS.load(Ordering::SeqCst),1); event(1,0); event(2,0);
    assert_eq!(STARTS.load(Ordering::SeqCst),2);
}
#[test] fn test_real_paths_configure_callback_lost_no_overlap_then_retry() {
    reset(); config_adv_data(); poll(5_099); assert_eq!(CONFIGS.load(Ordering::SeqCst),1);
    poll(5_100); assert_eq!(CONFIGS.load(Ordering::SeqCst),2);
    event(1,0); event(2,0); assert!(ADV_HEALTH.lock().active());
}
#[test] fn test_real_paths_start_callback_lost_recovers_in_five_seconds() {
    reset(); start_advertising(); poll(5_099); assert_eq!(STARTS.load(Ordering::SeqCst),1);
    poll(5_100); assert_eq!(CONFIGS.load(Ordering::SeqCst),1);
    event(1,0); event(2,0); assert_eq!(STARTS.load(Ordering::SeqCst),2);
}
#[test] fn test_real_paths_failed_renewal_uses_backoff() {
    running(); FAIL_NEXT.store(3,Ordering::SeqCst); poll(60_100);
    assert!(!ADV_HEALTH.lock().active()); poll(65_099); assert_eq!(CONFIGS.load(Ordering::SeqCst),0);
    poll(65_100); assert_eq!(CONFIGS.load(Ordering::SeqCst),1);
    event(1,0); event(2,0); assert!(ADV_HEALTH.lock().active());
}
#[test] fn test_real_paths_rename_waits_for_stop_callback_and_latest_name() {
    running(); *ID.lock()="Node".into(); PENDING_GAP_NAME_UPDATE.store(true,Ordering::SeqCst);
    poll(200); assert_eq!(STOPS.load(Ordering::SeqCst),1); assert_eq!(CONFIGS.load(Ordering::SeqCst),0);
    assert_eq!(&*AIR_NAME.lock(),"MNode"); event(3,0);
    assert_eq!(CONFIGS.load(Ordering::SeqCst),1); event(1,0); event(2,0);
    assert!(ADV_HEALTH.lock().active());
}
#[test] fn test_real_paths_stop_callback_lost_cannot_freeze_rename() {
    running(); PENDING_GAP_NAME_UPDATE.store(true,Ordering::SeqCst); poll(200);
    poll(5_199); assert_eq!(CONFIGS.load(Ordering::SeqCst),0);
    poll(5_200); assert_eq!(CONFIGS.load(Ordering::SeqCst),1);
    assert!(!ADV_RECONFIGURE_PENDING.load(Ordering::SeqCst));
    event(3,0); assert_eq!(CONFIGS.load(Ordering::SeqCst),1);
    event(1,0); event(2,0); assert!(ADV_HEALTH.lock().active());
}
#[test] fn test_real_paths_set_name_failure_does_not_flood_sdk() {
    running(); FAIL_NEXT.store(1,Ordering::SeqCst); PENDING_GAP_NAME_UPDATE.store(true,Ordering::SeqCst);
    poll(200); for now in (210..5_200).step_by(10) { poll(now); }
    assert_eq!(NAMES.load(Ordering::SeqCst),1); assert_eq!(CONFIGS.load(Ordering::SeqCst),0);
    poll(5_200); assert_eq!(NAMES.load(Ordering::SeqCst),2);
    assert_eq!(CONFIGS.load(Ordering::SeqCst),1);
}
#[test] fn test_real_paths_no_maintenance_before_service_ready() {
    reset(); SVC_STARTED.store(false,Ordering::SeqCst); poll(120_100);
    assert_eq!(STARTS.load(Ordering::SeqCst),0); assert_eq!(CONFIGS.load(Ordering::SeqCst),0);
}
#[test] fn test_real_paths_ordinary_disconnect_resumes_without_rename() {
    running(); CONN_ID.store(0,Ordering::SeqCst); advertising_connected();
    poll(120_000); assert_eq!(STARTS.load(Ordering::SeqCst),1);
    CONN_ID.store(0xFFFF,Ordering::SeqCst); advertising_disconnected(); event(2,0);
    assert_eq!(STARTS.load(Ordering::SeqCst),2);
    assert_eq!(STOPS.load(Ordering::SeqCst),0);
    assert_eq!(CONFIGS.load(Ordering::SeqCst),0);
    assert_eq!(NAMES.load(Ordering::SeqCst),0);
}
#[test] fn test_real_paths_null_and_failure_callbacks_remain_recoverable() {
    reset(); config_adv_data(); unsafe { gap_event_cb(1,std::ptr::null_mut()); }
    poll(5_100); assert_eq!(CONFIGS.load(Ordering::SeqCst),2); event(1,1);
    poll(10_099); assert_eq!(CONFIGS.load(Ordering::SeqCst),2);
    poll(10_100); assert_eq!(CONFIGS.load(Ordering::SeqCst),3);
    event(1,0); event(2,0); assert!(ADV_HEALTH.lock().active());
}
'''


def main():
    original = (ROOT / "src/ble_at/mod.rs").read_text()
    functions = "\n".join(item(original, anchor) for anchor in (
        "fn advertising_now_ms(", "fn begin_advertising_operation(",
        "fn start_advertising(", "fn stop_advertising(", "fn gap_name_from_ble_id(",
        "fn advertising_connected(", "fn advertising_disconnected(",
        "fn config_adv_data(", 'unsafe extern "C" fn gap_event_cb(',
        "pub fn update_gap_device_name(", "fn maintain_advertising(",
    ))
    uuid = re.search(r"static SERVICE_UUID_128:\s*\[u8;\s*16\]\s*=\s*\[.*?\];", original, re.S).group(0)
    source = SHIMS + f'\n#[path="{ROOT / "src/ble_at/advertising_health.rs"}"] mod advertising_health;\n'
    source += '''
use advertising_health::{AdvertisingHealth, Operation as AdvOperation, Recovery as AdvRecovery};
static ADV_HEALTH: Spin<AdvertisingHealth> = Spin::new(AdvertisingHealth::new());
static CONN_ID: AtomicU16 = AtomicU16::new(0xFFFF);
static SVC_STARTED: AtomicBool = AtomicBool::new(false);
static ADV_RECONFIGURE_PENDING: AtomicBool = AtomicBool::new(false);
static PENDING_GAP_NAME_UPDATE: AtomicBool = AtomicBool::new(false);
const SERVICE_UUID: &str = "4fafc201-1fb5-459e-8fcc-c5c9c331914b";
'''
    source += uuid + "\n" + functions + "\n" + TESTS
    with tempfile.TemporaryDirectory(prefix="imc-ble-advertising-") as temporary:
        root = Path(temporary)
        (root / "tests.rs").write_text(source)
        subprocess.run(["rustc", "+stable", "--edition=2024", "--test", str(root / "tests.rs"), "-o", str(root / "tests")], check=True)
        subprocess.run([str(root / "tests"), "--test-threads=1"], check=True, timeout=30)


if __name__ == "__main__":
    main()
