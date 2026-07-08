//! 配网流程 (Node 被配网 + Provisioner 主动配网)
//!
//! - 静态 OOB / 输出 OOB 配置
//! - 配网广播承载参数 (在 `bindings::start_advertising` 中通过 bearer 启动)
//! - 处理 ESP_BLE_MESH_PROVISIONING_EVT 等子事件
//! - Provisioner 角色: 添加 AppKey / Network Key 后扫描配网其它节点
//!
//! 注意: OOB 实际值 / dev key / 静态 OOB 应从 nvs 读出或生成; 此处仅占位 + TODO。

use std::os::raw::{c_int, c_void};

use parking_lot::Mutex;
use once_cell::sync::Lazy;

use crate::config::ble_mesh as cfg;
use crate::error::AppResult;

use super::bindings::EspBleMeshProv;

// OOB 输出动作 (esp_ble_mesh_prov.h)
// TODO: 用 esp_idf_sys 中已导出的常量
const PROV_OOB_OUTPUT_BLINK: u16 = 0x0001;
const PROV_OOB_OUTPUT_BEEP: u16 = 0x0002;
/// OOB info 标志 (esp_ble_mesh_prov_oob_info_t = enum = int = 4 bytes)
/// ESP_BLE_MESH_PROV_OOB_OTHER = BIT(0) = 0x0001
const PROV_OOB_INFO_OTHER: u32 = 0x0001;

// mesh 回调事件 (esp_ble_mesh_prov_cb_event_t 子集, 数值取自 ESP-IDF v5.5.4)
const EVT_NODE_PROV_COMPLETE: c_int = 10; // ESP_BLE_MESH_NODE_PROV_COMPLETE_EVT
const EVT_PROVISIONER_PROV_COMPLETE: c_int = 31; // ESP_BLE_MESH_PROVISIONER_PROV_COMPLETE_EVT

// ----------------------------------------------------------------------------
// 当前 mesh 密钥索引 (运行时状态, 配网完成后更新, 启动时从 NVS 加载)
// ----------------------------------------------------------------------------
/// 当前 mesh 密钥索引 (net_idx, app_idx)
/// - 启动时从 NVS 加载 (Lazy 首次访问时执行)
/// - 配网完成后通过 `set_mesh_keys()` 更新并持久化到 NVS
/// - 供 `bindings::heartbeat_loop` / `models::send_onoff_set` 等读取
static MESH_KEYS: Lazy<Mutex<(u16, u16)>> = Lazy::new(|| {
    let (net_idx, app_idx) = crate::device::load_mesh_keys();
    if net_idx != 0 || app_idx != 0 {
        log::info!(
            "[mesh-prov] mesh keys loaded from NVS: net_idx={}, app_idx={}",
            net_idx,
            app_idx
        );
    } else {
        log::info!("[mesh-prov] no mesh keys in NVS (not provisioned yet)");
    }
    Mutex::new((net_idx, app_idx))
});

/// 当前 net_idx / app_idx (供 bindings::heartbeat_loop 等使用)
///
/// 未配网时返回 (0, 0), 配网完成后返回最新值
pub fn current_mesh_keys() -> (u16, u16) {
    *MESH_KEYS.lock()
}

/// 更新 mesh 密钥索引 (配网完成后调用, 同时持久化到 NVS)
///
/// - 更新内存中的 `MESH_KEYS`
/// - 持久化到 NVS (`mesh_nidx` / `mesh_aidx`), 重启后可恢复
pub fn set_mesh_keys(net_idx: u16, app_idx: u16) {
    {
        let mut keys = MESH_KEYS.lock();
        *keys = (net_idx, app_idx);
    }
    if let Err(e) = crate::device::save_mesh_keys(net_idx, app_idx) {
        log::warn!("[mesh-prov] save mesh keys to NVS failed: {}", e);
    } else {
        log::info!(
            "[mesh-prov] mesh keys updated: net_idx={}, app_idx={}",
            net_idx,
            app_idx
        );
    }
}

// ----------------------------------------------------------------------------
// 设备 UUID (test, TODO: 从 nvs 读 / 生成)
// ----------------------------------------------------------------------------
/// 未配网设备 UUID (前缀 0x02 表示 Mesh unprovisioned device)
const DEVICE_UUID: [u8; 16] = [
    0x02, 0x00, // Mesh UUID prefix
    0x00, 0x00, 0x00, 0x00, // TODO: company / product / version
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, // TODO: CRC placeholder
];

// ----------------------------------------------------------------------------
// 配网参数 (static mut: esp_ble_mesh_init 可能写入 dev_key)
//
// EspBleMeshProv 字段布局严格对齐 ESP-IDF v5.5.4 esp_ble_mesh_defs.h L841-970
// uuid 是指针 (*const u8), 非 [u8; 16] 数组
// 所有回调字段 (esp_ble_mesh_cb_t = uint32_t) 初始化为 0, 由 stack 回填
// ----------------------------------------------------------------------------
static mut PROV: EspBleMeshProv = EspBleMeshProv {
    // ---- NODE 部分 (CONFIG_BT_BLE_MESH_NODE) ----
    uuid: DEVICE_UUID.as_ptr(),
    uri: std::ptr::null(),
    oob_info: PROV_OOB_INFO_OTHER, // esp_ble_mesh_prov_oob_info_t (enum = int)
    oob_pub_key: 0,
    _pad0: [0; 3],
    oob_pub_key_cb: 0,
    oob_type: 0,
    _pad1: [0; 3],
    static_val: std::ptr::null(),
    static_val_len: 0,
    output_size: cfg::OOB_SIZE,
    output_actions: PROV_OOB_OUTPUT_BLINK | PROV_OOB_OUTPUT_BEEP,
    input_size: 0,
    _pad2: [0; 1],
    input_actions: 0,
    output_num_cb: 0,
    output_str_cb: 0,
    input_cb: 0,
    link_open_cb: 0,
    link_close_cb: 0,
    complete_cb: 0,
    reset_cb: 0,
    // ---- PROVISIONER 部分 (CONFIG_BT_BLE_MESH_PROVISIONER) ----
    prov_uuid: std::ptr::null(),
    prov_unicast_addr: 0,
    prov_start_address: 0,
    prov_attention: 0,
    prov_algorithm: 0,
    prov_pub_key_oob: 0,
    _pad5: [0; 1],
    provisioner_prov_read_oob_pub_key: 0,
    prov_static_oob_val: std::ptr::null_mut(),
    prov_static_oob_len: 0,
    _pad6: [0; 3],
    provisioner_prov_input: 0,
    provisioner_prov_output: 0,
    flags: 0,
    _pad7: [0; 3],
    iv_index: 0,
    provisioner_link_open: 0,
    provisioner_link_close: 0,
    provisioner_prov_comp: 0,
    cert_based_prov_start: 0,
    records_list_get: 0,
    prov_record_recv_comp: 0,
};

/// 返回配网参数指针 (供 esp_ble_mesh_init; 注意 mesh 栈可能写入 dev_key)
pub fn provisioning_info_mut() -> *mut EspBleMeshProv {
    std::ptr::addr_of_mut!(PROV)
}

/// 返回 const 指针 (供 EspBleMeshConfig 引用)
pub fn provisioning_info() -> *const EspBleMeshProv {
    provisioning_info_mut() as *const _
}

// ----------------------------------------------------------------------------
// 配网事件处理 (由 bindings::mesh_event_cb 转发)
// ----------------------------------------------------------------------------

/// `esp_ble_mesh_prov_cb_param_t.node_prov_complete` (ESP-IDF v5.5.4 defs.h L1264-1270)
///
/// ```c
/// struct ble_mesh_provision_complete_evt_param {
///     uint16_t net_idx;       // offset 0
///     uint8_t  net_key[16];   // offset 2 (u8 数组, 无需 padding)
///     uint16_t addr;          // offset 18
///     uint8_t  flags;         // offset 20
///     uint32_t iv_index;     // offset 24 (需 4 字节对齐, flags 后 pad 3 bytes)
/// } node_prov_complete;
/// ```
#[repr(C)]
struct NodeProvCompleteParam {
    net_idx: u16,       // offset 0
    net_key: [u8; 16],  // offset 2 (u8 数组, 无需 padding)
    addr: u16,          // offset 18
    flags: u8,          // offset 20
    _pad0: [u8; 3],     // offset 21-23 (对齐 iv_index: u32)
    iv_index: u32,      // offset 24
}

/// `esp_ble_mesh_prov_cb_param_t.provisioner_prov_complete` (ESP-IDF v5.5.4 defs.h L1419-1425)
///
/// ```c
/// struct ble_mesh_provisioner_prov_comp_param {
///     uint16_t node_idx;              // offset 0
///     esp_ble_mesh_octet16_t device_uuid; // offset 2 (uint8_t[16], 无需 padding)
///     uint16_t unicast_addr;          // offset 18
///     uint8_t element_num;            // offset 20
///     uint16_t netkey_idx;            // offset 22 (需 2 字节对齐, element_num 后 pad 1 byte)
/// } provisioner_prov_complete;
/// ```
#[repr(C)]
struct ProvisionerProvCompleteParam {
    node_idx: u16,          // offset 0
    device_uuid: [u8; 16],  // offset 2 (u8 数组, 无需 padding)
    unicast_addr: u16,      // offset 18
    element_num: u8,        // offset 20
    _pad0: [u8; 1],         // offset 21 (对齐 netkey_idx: u16)
    netkey_idx: u16,        // offset 22
}

/// 处理 ESP_BLE_MESH_PROVISIONING_EVT 等事件
///
/// `param` 实为 `esp_ble_mesh_prov_cb_param_t*`; 按 event 解析联合体对应分支。
/// 常见 sub-event:
/// - NODE_PROV_COMPLETE (10): 本机配网完成 → 提取 net_idx → set_mesh_keys 持久化
/// - PROVISIONER_PROV_COMPLETE (31): 远端节点配网完成 → add_app_key / bind model
/// - NODE_PROV_OUTPUT_NUMBER (7): stack 请求输出 OOB 数字 → output_oob
pub unsafe fn handle_provisioning_event(event: c_int, param: *mut c_void) {
    match event {
        EVT_NODE_PROV_COMPLETE => {
            // 本机配网完成, 提取 net_idx 并持久化
            if param.is_null() {
                log::warn!("[mesh-prov] NODE_PROV_COMPLETE: null param");
                return;
            }
            // SAFETY: param 类型对齐 NodeProvCompleteParam (esp_ble_mesh_defs.h v5.5.4 L1264-1270)
            let p = unsafe { &*(param as *const NodeProvCompleteParam) };
            log::info!(
                "[mesh-prov] NODE_PROV_COMPLETE: net_idx={}, addr=0x{:04x}, flags={}, iv_index={}",
                p.net_idx,
                p.addr,
                p.flags,
                p.iv_index
            );
            // app_idx 在配网完成事件中未提供, 默认用 cfg::APP_KEY_IDX (通常 0)
            // 后续 PROVISIONER_ADD_APP_KEY_COMP 事件可补充实际 app_idx
            set_mesh_keys(p.net_idx, cfg::APP_KEY_IDX);
        }
        EVT_PROVISIONER_PROV_COMPLETE => {
            // 远端节点配网完成 (本机作为 provisioner)
            if param.is_null() {
                log::warn!("[mesh-prov] PROVISIONER_PROV_COMPLETE: null param");
                return;
            }
            // SAFETY: param 类型对齐 ProvisionerProvCompleteParam
            let p = unsafe { &*(param as *const ProvisionerProvCompleteParam) };
            log::info!(
                "[mesh-prov] PROVISIONER_PROV_COMPLETE: node_idx={}, addr=0x{:04x}, netkey_idx={}",
                p.node_idx,
                p.unicast_addr,
                p.netkey_idx
            );
            // 远端节点配网完成, 触发 app_key 绑定
            if let Err(e) = add_app_key() {
                log::warn!("[mesh-prov] add_app_key after provisioner prov complete: {}", e);
            }
        }
        _ => {
            log::debug!("[mesh-prov] unhandled event={}", event);
        }
    }
}

// ----------------------------------------------------------------------------
// Provisioner 角色
// ----------------------------------------------------------------------------
/// 启动 provisioner (添加 AppKey / Network Key)
pub fn start_provisioner() -> AppResult<()> {
    // TODO: 1. esp_ble_mesh_provisioner_add_unprov_dev(已知未配网设备)
    // TODO: 2. provisioner_prov_enable 已在 bindings::start_advertising 调用
    // TODO: 3. bind app key 到 onoff server model
    add_app_key()?;
    log::info!("[mesh-prov] provisioner started");
    Ok(())
}

/// 添加 AppKey 到网络 (供 provisioner 调用)
pub fn add_app_key() -> AppResult<()> {
    let (net_idx, app_idx) = current_mesh_keys();
    // TODO: esp_ble_mesh_provisioner_add_app_key(net_idx, app_idx, app_key)
    // TODO: esp_ble_mesh_model_bind_app_key(model, app_idx)
    log::info!(
        "[mesh-prov] add_app_key (TODO): net_idx={}, app_idx={}",
        net_idx,
        app_idx
    );
    Ok(())
}

/// 输出 OOB (供应用层在配网时展示, TODO: 驱动 LED)
pub fn output_oob(number: u32) {
    // TODO: 通过 hal.gpio 闪烁 LED 展示 number
    log::info!("[mesh-prov] output OOB: {} (TODO: LED blink)", number);
}

/// 静态 OOB 取值 (TODO: 从 nvs 读出 16 字节)
pub fn static_oob() -> Option<[u8; 16]> {
    // TODO: 读取持久化的 static OOB; 返回 None 表示不使用 static OOB
    None
}
