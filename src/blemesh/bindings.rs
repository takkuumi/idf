//! ESP-IDF BLE Mesh C API 绑定封装
//!
//! 字段布局严格对齐 ESP-IDF v5.5.4 源码:
//!   /Users/ling/workspace/esp-idf/components/bt/esp_ble_mesh/api/esp_ble_mesh_defs.h
//!   /Users/ling/workspace/esp-idf/components/bt/esp_ble_mesh/api/core/include/esp_ble_mesh_networking_api.h
//!
//! 关键事实 (已对照源码核对):
//! - esp_ble_mesh_cb_t = uint32_t (非函数指针, 见 defs.h L220)
//! - esp_ble_mesh_model_op_t: opcode(u32) + min_len(usize) + param_cb(u32), 共 12 bytes
//! - esp_ble_mesh_msg_ctx_t: 17 个字段, model 是指针, 末尾有 enh 结构体
//! - model_operation 事件参数: opcode + model + ctx(指针) + length + msg (无 errcode!)
//! - esp_ble_mesh_model_publish: 5 参数 (model, opcode, length, data, role), 无 ctx
//! - esp_ble_mesh_server_model_send_msg / client_model_send_msg: 带 ctx 的发送 API
//! - esp_ble_mesh_prov_t: uuid 是指针 (非数组!), 含大量回调字段 (NODE + PROVISIONER)
//! - esp_ble_mesh_init(prov, comp): 双参数, 无 cfg 包装

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};

use once_cell::sync::OnceCell;

use crate::config::ble_mesh as cfg;
use crate::error::{AppError, AppResult};

use super::models;
use super::provisioning;

// ============================================================================
// unsafe extern "C" 声明 (BLE Mesh 专有)
// ============================================================================
type MeshCb = extern "C" fn(event: c_int, param: *mut c_void);

unsafe extern "C" {
    /// esp_ble_mesh_init(esp_ble_mesh_prov_t *prov, esp_ble_mesh_comp_t *comp)
    /// 双参数: prov 可写 (栈回填 dev_key), comp 只读
    fn esp_ble_mesh_init(prov: *mut EspBleMeshProv, comp: *const EspBleMeshComp) -> c_int;
    /// 注册配网事件回调 (NODE_PROV_COMPLETE / PROVISIONER_PROV_COMPLETE 等)
    fn esp_ble_mesh_register_prov_callback(cb: MeshCb) -> c_int;
    /// 注册模型事件回调 (MODEL_OPERATION 等), v5.5.4 拆分自 register_callback
    fn esp_ble_mesh_register_custom_model_callback(cb: MeshCb) -> c_int;
    /// 注册 Generic Client 回调 (事件空间与 model_cb 不同)
    fn esp_ble_mesh_register_generic_client_callback(cb: MeshCb) -> c_int;
    fn esp_ble_mesh_proxy_identity_enable() -> c_int;
    fn esp_ble_mesh_proxy_gatt_enable() -> c_int;
    fn esp_ble_mesh_node_prov_enable(bearers: u16) -> c_int;
    fn esp_ble_mesh_provisioner_prov_enable(bearers: u16) -> c_int;

    /// 发布消息 (5 参数, 无 ctx; 需预先配置 model publish 地址)
    /// esp_ble_mesh_model_publish(model, opcode, length, data, device_role)
    fn esp_ble_mesh_model_publish(
        model: *mut EspBleMeshModel,
        opcode: u32,
        length: u16,
        data: *mut u8,
        device_role: u8,
    ) -> c_int;

    /// Server 模型发送消息 (含 ctx, 用于回复 Status 等)
    /// esp_ble_mesh_server_model_send_msg(model, ctx, opcode, length, data)
    pub fn esp_ble_mesh_server_model_send_msg(
        model: *mut EspBleMeshModel,
        ctx: *const EspBleMeshMsgCtx,
        opcode: u32,
        length: u16,
        data: *mut u8,
    ) -> c_int;

    /// Client 模型发送消息 (含 ctx + 超时 + 是否需要响应)
    /// esp_ble_mesh_client_model_send_msg(model, ctx, opcode, length, data, msg_timeout, need_rsp, device_role)
    pub fn esp_ble_mesh_client_model_send_msg(
        model: *mut EspBleMeshModel,
        ctx: *const EspBleMeshMsgCtx,
        opcode: u32,
        length: u16,
        data: *mut u8,
        msg_timeout: i32,
        need_rsp: bool,
        device_role: u8,
    ) -> c_int;
}

// ============================================================================
// 关键数据结构 (严格对齐 ESP-IDF v5.5.4 esp_ble_mesh_defs.h)
// ============================================================================

/// esp_ble_mesh_model_op_t (defs.h L552-556)
///
/// ```c
/// typedef struct {
///     const uint32_t    opcode;   // Message opcode
///     const size_t      min_len;  // Message minimum length (size_t = 4 bytes on 32-bit)
///     esp_ble_mesh_cb_t param_cb; // = uint32_t (defs.h L220)
/// } esp_ble_mesh_model_op_t;
/// ```
/// 总大小: 12 bytes (32 位系统)
#[repr(C)]
pub struct EspBleMeshOp {
    pub opcode: u32,
    pub min_len: usize,
    pub param_cb: u32, // esp_ble_mesh_cb_t = uint32_t
}

/// esp_ble_mesh_model_t (defs.h L580-615)
///
/// ```c
/// struct esp_ble_mesh_model {
///     union { const uint16_t model_id; struct { uint16_t company_id, model_id; } vnd; };
///     uint8_t element_idx, model_idx;
///     uint16_t flags;
///     esp_ble_mesh_elem_t *element;
///     esp_ble_mesh_model_pub_t *const pub;
///     uint16_t keys[CONFIG_BLE_MESH_MODEL_KEY_COUNT];
///     uint16_t groups[CONFIG_BLE_MESH_MODEL_GROUP_COUNT];
///     esp_ble_mesh_model_op_t *op;       // 注意: 非 const
///     esp_ble_mesh_model_cbs_t *cb;
///     void *user_data;
/// };
/// ```
#[repr(C)]
pub struct EspBleMeshModel {
    pub model_id: u16,
    pub company_id: u16,
    pub element_idx: u8,
    pub model_idx: u8,
    pub flags: u16,
    pub element: *mut EspBleMeshElem,
    pub pub_: *mut c_void, // esp_ble_mesh_model_pub_t*
    pub keys: [u16; CONFIG_BLE_MESH_MODEL_KEY_COUNT],
    pub groups: [u16; CONFIG_BLE_MESH_MODEL_GROUP_COUNT],
    pub op: *mut EspBleMeshOp,
    pub cb: *mut c_void, // esp_ble_mesh_model_cbs_t*
    pub user_data: *mut c_void,
}

/// esp_ble_mesh_elem_t (defs.h L459-471)
///
/// ```c
/// typedef struct {
///     uint16_t element_addr;
///     const uint16_t location;
///     const uint8_t sig_model_count;
///     const uint8_t vnd_model_count;
///     esp_ble_mesh_model_t *sig_models;  // 非 const
///     esp_ble_mesh_model_t *vnd_models;  // 非 const
/// } esp_ble_mesh_elem_t;
/// ```
#[repr(C)]
pub struct EspBleMeshElem {
    pub element_addr: u16,
    pub location: u16,
    pub sig_model_count: u8,
    pub vnd_model_count: u8,
    pub sig_models: *mut EspBleMeshModel,
    pub vnd_models: *mut EspBleMeshModel,
}

/// esp_ble_mesh_comp_t (defs.h L975-982)
///
/// ```c
/// typedef struct {
///     uint16_t cid, pid, vid;
///     size_t element_count;  // size_t = 4 bytes on 32-bit
///     esp_ble_mesh_elem_t *elements;
/// } esp_ble_mesh_comp_t;
/// ```
#[repr(C)]
pub struct EspBleMeshComp {
    pub cid: u16,
    pub pid: u16,
    pub vid: u16,
    pub element_count: usize,
    pub elements: *mut EspBleMeshElem,
}

/// esp_ble_mesh_msg_ctx_t (defs.h L785-836)
///
/// 完整字段 (17 个有效字段 + enh 结构体):
/// ```c
/// typedef struct {
///     uint16_t net_idx, app_idx, addr;
///     uint16_t recv_dst;
///     int8_t   recv_rssi;
///     uint32_t recv_op;
///     uint8_t  recv_ttl, recv_cred, recv_tag;
///     uint8_t  send_rel:1, send_szmic:1;  // bitfield (1 byte)
///     uint8_t  send_ttl, send_cred, send_tag;
///     esp_ble_mesh_model_t *model;        // 指针 (4 bytes)
///     bool srv_send;
///     esp_ble_mesh_msg_enh_params_t enh;  // 增强消息参数
/// } esp_ble_mesh_msg_ctx_t;
/// ```
///
/// 发送时只需设置 net_idx/app_idx/addr, 其余置 0;
/// 接收时通过指针解引用读取前几个字段。
///
/// esp_ble_mesh_msg_enh_params_t (defs.h L710-750) 当前配置下:
///   1 byte bitfield (adv_cfg_used:1) + 3 bytes padding + 8 bytes adv_cfg
///   总大小 12 bytes (未启用 EXT_ADV / LONG_PACKET)
#[repr(C)]
pub struct EspBleMeshMsgCtx {
    pub net_idx: u16,           // offset 0
    pub app_idx: u16,           // offset 2
    pub addr: u16,              // offset 4
    pub recv_dst: u16,          // offset 6
    pub recv_rssi: i8,          // offset 8
    pub _pad0: [u8; 3],         // offset 9-11 (对齐 recv_op: u32)
    pub recv_op: u32,           // offset 12
    pub recv_ttl: u8,           // offset 16
    pub recv_cred: u8,          // offset 17
    pub recv_tag: u8,           // offset 18
    pub send_rel_szmic: u8,     // offset 19 (bitfield: send_rel:1 + send_szmic:1)
    pub send_ttl: u8,           // offset 20
    pub send_cred: u8,          // offset 21
    pub send_tag: u8,           // offset 22
    pub _pad1: [u8; 1],         // offset 23 (对齐 model 指针)
    pub model: *mut EspBleMeshModel, // offset 24 (4 bytes on 32-bit)
    pub srv_send: u8,           // offset 28 (bool)
    pub _pad2: [u8; 3],         // offset 29-31 (对齐 enh)
    pub _enh_reserved: [u8; 12],// offset 32-43 (esp_ble_mesh_msg_enh_params_t, 12 bytes)
}
// 总大小: 44 bytes (32 位系统, 4 字节对齐)

impl EspBleMeshMsgCtx {
    /// 构造发送用 ctx (仅填充发送字段, 其余置 0)
    ///
    /// send_ttl=0 表示用默认 TTL (CONFIG_BLE_MESH_DEFAULT_TTL)
    pub fn for_send(net_idx: u16, app_idx: u16, addr: u16) -> Self {
        EspBleMeshMsgCtx {
            net_idx,
            app_idx,
            addr,
            recv_dst: 0,
            recv_rssi: 0,
            _pad0: [0; 3],
            recv_op: 0,
            recv_ttl: 0,
            recv_cred: 0,
            recv_tag: 0,
            send_rel_szmic: 0,
            send_ttl: 0,
            send_cred: 0,
            send_tag: 0,
            _pad1: [0; 1],
            model: std::ptr::null_mut(),
            srv_send: 0,
            _pad2: [0; 3],
            _enh_reserved: [0; 12],
        }
    }
}

/// esp_ble_mesh_model_operation 事件参数 (defs.h L2638-2644)
///
/// ```c
/// struct ble_mesh_model_operation_evt_param {
///     uint32_t opcode;
///     esp_ble_mesh_model_t *model;
///     esp_ble_mesh_msg_ctx_t *ctx;  // 指针
///     uint16_t length;
///     uint8_t *msg;                 // 非 const
/// } model_operation;
/// ```
/// 注意: 没有 errcode 字段!
#[repr(C)]
pub struct ModelOpParam {
    pub opcode: u32,            // offset 0
    pub model: *mut EspBleMeshModel, // offset 4
    pub ctx: *mut EspBleMeshMsgCtx, // offset 8
    pub length: u16,            // offset 12
    pub _pad0: [u8; 2],         // offset 14 (对齐 msg 指针)
    pub msg: *mut u8,           // offset 16
}
// 总大小: 20 bytes (32 位系统, 含尾部对齐 padding 到 4 字节 = 20 bytes)

/// esp_ble_mesh_prov_t (defs.h L841-970)
///
/// 结构复杂: uuid 是指针 (非数组!), 包含大量回调字段 (esp_ble_mesh_cb_t = uint32_t)
/// 受 CONFIG_BT_BLE_MESH_NODE / CONFIG_BT_BLE_MESH_PROVISIONER 条件编译控制
/// (sdkconfig 已启用两者, 故包含全部字段)
///
/// NODE 部分 (L842-900):
///   uuid, uri, oob_info(enum=int), oob_pub_key(bool), oob_pub_key_cb,
///   oob_type, static_val, static_val_len, output_size, output_actions,
///   input_size, input_actions, 7 个回调 (output_num_cb ... reset_cb)
///
/// PROVISIONER 部分 (L902-969):
///   prov_uuid, prov_unicast_addr, prov_start_address, prov_attention,
///   prov_algorithm, prov_pub_key_oob, provisioner_prov_read_oob_pub_key,
///   prov_static_oob_val, prov_static_oob_len, provisioner_prov_input/output,
///   flags, iv_index, 7 个回调 (provisioner_link_open ... prov_record_recv_comp)
#[repr(C)]
pub struct EspBleMeshProv {
    // ---- NODE 部分 (CONFIG_BT_BLE_MESH_NODE) ----
    pub uuid: *const u8,
    pub uri: *const c_char,
    pub oob_info: u32, // esp_ble_mesh_prov_oob_info_t (enum = int = 4 bytes)
    pub oob_pub_key: u8, // bool
    pub _pad0: [u8; 3], // 对齐 oob_pub_key_cb: u32
    pub oob_pub_key_cb: u32,
    pub oob_type: u8,
    pub _pad1: [u8; 3], // 对齐 static_val: *const u8
    pub static_val: *const u8,
    pub static_val_len: u8,
    pub output_size: u8,
    pub output_actions: u16,
    pub input_size: u8,
    pub _pad2: [u8; 1], // 对齐 input_actions: u16
    pub input_actions: u16,
    // input_actions 结束于 offset 36, output_num_cb 需 4 字节对齐, 36 已满足, 无需 padding
    pub output_num_cb: u32,
    pub output_str_cb: u32,
    pub input_cb: u32,
    pub link_open_cb: u32,
    pub link_close_cb: u32,
    pub complete_cb: u32,
    pub reset_cb: u32,
    // ---- PROVISIONER 部分 (CONFIG_BT_BLE_MESH_PROVISIONER) ----
    pub prov_uuid: *const u8,
    pub prov_unicast_addr: u16,
    // prov_unicast_addr 结束于 offset 70, prov_start_address 需 2 字节对齐, 70 已满足, 无需 padding
    pub prov_start_address: u16,
    pub prov_attention: u8,
    pub prov_algorithm: u8,
    pub prov_pub_key_oob: u8,
    pub _pad5: [u8; 1], // 对齐 provisioner_prov_read_oob_pub_key: u32
    pub provisioner_prov_read_oob_pub_key: u32,
    pub prov_static_oob_val: *mut u8,
    pub prov_static_oob_len: u8,
    pub _pad6: [u8; 3], // 对齐 provisioner_prov_input: u32
    pub provisioner_prov_input: u32,
    pub provisioner_prov_output: u32,
    pub flags: u8,
    pub _pad7: [u8; 3], // 对齐 iv_index: u32
    pub iv_index: u32,
    pub provisioner_link_open: u32,
    pub provisioner_link_close: u32,
    pub provisioner_prov_comp: u32,
    pub cert_based_prov_start: u32,
    pub records_list_get: u32,
    pub prov_record_recv_comp: u32,
}

/// BLE Mesh 角色 (esp_ble_mesh_dev_role_t)
/// 0=NODE 1=PROVISIONER 2=FAST_PROV
pub const ROLE_NODE: u8 = 0;

// 原始指针型静态量需手动声明 Sync
unsafe impl Sync for EspBleMeshOp {}
unsafe impl Sync for EspBleMeshModel {}
unsafe impl Sync for EspBleMeshElem {}
unsafe impl Sync for EspBleMeshComp {}
unsafe impl Sync for EspBleMeshProv {}

// ============================================================================
// 常量
// ============================================================================
// Generic OnOff opcodes (esp_ble_mesh_defs.h L2161-2164)
// ESP_BLE_MESH_MODEL_OP_2(b0, b1) = (b0 << 8) | b1
//   GET         = OP_2(0x82, 0x01) = 0x8201
//   SET         = OP_2(0x82, 0x02) = 0x8202
//   SET_UNACK   = OP_2(0x82, 0x03) = 0x8203
//   STATUS      = OP_2(0x82, 0x04) = 0x8204
pub const OP_GEN_ONOFF_GET: u32 = 0x8201;
pub const OP_GEN_ONOFF_SET: u32 = 0x8202;
pub const OP_GEN_ONOFF_SET_UNACK: u32 = 0x8203;
pub const OP_GEN_ONOFF_STATUS: u32 = 0x8204;

// 配网承载 (esp_ble_mesh_prov.h)
const PROV_BEARER_ADV: u16 = 0x0001; // PB-ADV
const PROV_BEARER_GATT: u16 = 0x0002; // PB-GATT

// mesh 模型回调事件 (esp_ble_mesh_model_cb_event_t, defs.h L2620-2629)
// ESP_BLE_MESH_MODEL_OPERATION_EVT = 0 (枚举首项)
const EVT_MODEL_OPERATION: c_int = 0;

// Generic Client 回调事件 (esp_ble_mesh_generic_client_cb_event_t, generic_model_api.h L460-465)
// 注意: 事件值空间与 model_cb 不同! 不可共用同一回调函数
const EVT_GENERIC_CLIENT_GET_STATE: c_int = 0;  // 收到 Status 响应 Get
const EVT_GENERIC_CLIENT_SET_STATE: c_int = 1;  // 收到 Status 响应 Set
const EVT_GENERIC_CLIENT_PUBLISH: c_int = 2;    // 收到 publish 消息
const EVT_GENERIC_CLIENT_TIMEOUT: c_int = 3;    // 等待响应超时

// 保存 nvs 分区句柄 (mesh 栈生命周期内需常驻)
static NVS: OnceCell<esp_idf_svc::nvs::EspDefaultNvsPartition> = OnceCell::new();

// CONFIG_BLE_MESH_MODEL_KEY_COUNT / GROUP_COUNT 默认值 (Kconfig.in L755/L763)
// sdkconfig.defaults 未覆盖, 使用 ESP-IDF 默认值 3
pub const CONFIG_BLE_MESH_MODEL_KEY_COUNT: usize = 3;
pub const CONFIG_BLE_MESH_MODEL_GROUP_COUNT: usize = 3;

// ============================================================================
// 错误转换
// ============================================================================
fn check(ret: c_int, ctx: &str) -> AppResult<()> {
    if ret == 0 {
        Ok(())
    } else {
        Err(AppError::BleMesh(format!("{ctx} failed: esp_err=0x{ret:x}")))
    }
}

// ============================================================================
// 初始化各阶段
// ============================================================================
/// 1. 初始化 BT 控制器 (BLE only)
pub fn init_ble_controller() -> AppResult<()> {
    let mode_classic = esp_idf_sys::esp_bt_mode_t_ESP_BT_MODE_CLASSIC_BT;
    let _ = unsafe { esp_idf_sys::esp_bt_controller_mem_release(mode_classic) };

    let mut bt_cfg = esp_idf_sys::esp_bt_controller_config_t {
        controller_task_stack_size: 4096,
        ..Default::default()
    };
    check(
        unsafe { esp_idf_sys::esp_bt_controller_init(&mut bt_cfg) },
        "bt_controller_init",
    )?;
    check(
        unsafe { esp_idf_sys::esp_bt_controller_enable(esp_idf_sys::esp_bt_mode_t_ESP_BT_MODE_BLE) },
        "bt_controller_enable",
    )?;
    log::info!("[blemesh] bt controller ready");
    Ok(())
}

/// 2. 初始化 Bluedroid 协议栈 + 设置设备名
pub fn init_bluedroid() -> AppResult<()> {
    check(unsafe { esp_idf_sys::esp_bluedroid_init() }, "bluedroid_init")?;
    check(unsafe { esp_idf_sys::esp_bluedroid_enable() }, "bluedroid_enable")?;
    let name = CString::new(cfg::DEVICE_NAME).map_err(|e| {
        AppError::BleMesh(format!("device name has nul: {e}"))
    })?;
    check(
        unsafe { esp_idf_sys::esp_bt_dev_set_device_name(name.as_ptr()) },
        "set_device_name",
    )?;
    log::info!("[blemesh] bluedroid ready (name={})", cfg::DEVICE_NAME);
    Ok(())
}

/// 3. 初始化 mesh 协议栈 (持有 nvs)
///
/// v5.5.4: esp_ble_mesh_init(prov, comp) 双参数
pub fn init_mesh_stack(nvs: esp_idf_svc::nvs::EspDefaultNvsPartition) -> AppResult<()> {
    let _ = NVS.set(nvs);

    let prov_mut = provisioning::provisioning_info_mut();
    let comp = models::composition();

    check(unsafe { esp_ble_mesh_init(prov_mut, comp) }, "esp_ble_mesh_init")?;

    // v5.5.4: 三类回调事件空间互不相同, 必须使用独立回调函数
    // - prov_cb:            NODE_PROV_COMPLETE(10) / PROVISIONER_PROV_COMPLETE(31) 等
    // - custom_model_cb:    MODEL_OPERATION(0) / MODEL_SEND_COMP(1) 等 (esp_ble_mesh_model_cb_event_t)
    // - generic_client_cb:  GET_STATE(0) / SET_STATE(1) / PUBLISH(2) / TIMEOUT(3)
    //                       (esp_ble_mesh_generic_client_cb_event_t, 与 model_cb 事件值空间不同!)
    check(
        unsafe { esp_ble_mesh_register_prov_callback(prov_event_cb) },
        "esp_ble_mesh_register_prov_callback",
    )?;
    check(
        unsafe { esp_ble_mesh_register_custom_model_callback(custom_model_event_cb) },
        "esp_ble_mesh_register_custom_model_callback",
    )?;
    check(
        unsafe { esp_ble_mesh_register_generic_client_callback(generic_client_event_cb) },
        "esp_ble_mesh_register_generic_client_callback",
    )?;
    log::info!("[blemesh] mesh stack initialized");
    Ok(())
}

/// 4. 注册模型 (Generic OnOff Server/Client) 与客户端回调
///
/// v5.5.4: 回调已在 init_mesh_stack 中注册, 此函数保留为占位/扩展点
pub fn register_models() -> AppResult<()> {
    log::info!("[blemesh] models registered (callbacks in init_mesh_stack)");
    Ok(())
}

/// 5. 启用 Proxy 服务 (GATT 接入)
///
/// v5.5.4: esp_ble_mesh_proxy_gatt_enable (非 proxy_proxy_enable!)
pub fn enable_proxy() -> AppResult<()> {
    check(unsafe { esp_ble_mesh_proxy_identity_enable() }, "proxy_identity_enable")?;
    check(unsafe { esp_ble_mesh_proxy_gatt_enable() }, "proxy_gatt_enable")?;
    log::info!("[blemesh] proxy enabled");
    Ok(())
}

/// 6. 启动配网广播 (Node + Provisioner 双角色)
pub fn start_advertising() -> AppResult<()> {
    check(
        unsafe { esp_ble_mesh_node_prov_enable(PROV_BEARER_ADV | PROV_BEARER_GATT) },
        "node_prov_enable",
    )?;
    check(
        unsafe { esp_ble_mesh_provisioner_prov_enable(PROV_BEARER_ADV | PROV_BEARER_GATT) },
        "provisioner_prov_enable",
    )?;
    log::info!("[blemesh] provisioning advertising started");
    Ok(())
}

/// 7. 心跳 / 状态周期上报
///
/// 使用 esp_ble_mesh_model_publish (5 参数, 无 ctx)
/// 注: publish 需预先配置 model 的 publish 地址 (通过配网时设置)
pub fn heartbeat_loop() -> AppResult<()> {
    let period = std::time::Duration::from_secs(cfg::HEARTBEAT_PERIOD_S as u64);
    loop {
        std::thread::sleep(period);

        let on = crate::bus::lock_timeout()
            .map(|b| b.do_.bits & 0x01u64 != 0)
            .unwrap_or(false);

        let mut msg: [u8; 1] = [on as u8];

        // 使用 Server 模型 (SIG_MODELS[0]) 发布
        // esp_ble_mesh_model_publish: 5 参数 (model, opcode, length, data, role)
        let server_model = models::SIG_MODELS.as_ptr() as *mut EspBleMeshModel;
        let ret = unsafe {
            esp_ble_mesh_model_publish(
                server_model,
                OP_GEN_ONOFF_STATUS,
                msg.len() as u16,
                msg.as_mut_ptr(),
                ROLE_NODE,
            )
        };

        if ret == 0 {
            log::info!("[blemesh] heartbeat: onoff[0]={} (published)", on);
        } else {
            log::warn!("[blemesh] heartbeat publish failed: esp_err=0x{:x} (on={})", ret, on);
        }
    }
}

// ============================================================================
// mesh 事件回调 (由 C 栈调用)
// ============================================================================
/// 配网事件回调 (prov_cb): NODE_PROV_COMPLETE / PROVISIONER_PROV_COMPLETE 等
extern "C" fn prov_event_cb(event: c_int, param: *mut c_void) {
    unsafe { provisioning::handle_provisioning_event(event, param) };
}

/// 自定义模型事件回调 (custom_model_cb): MODEL_OPERATION / MODEL_SEND_COMP 等
/// 事件值空间: esp_ble_mesh_model_cb_event_t (0=MODEL_OPERATION, 1=SEND_COMP, ...)
extern "C" fn custom_model_event_cb(event: c_int, param: *mut c_void) {
    if event == EVT_MODEL_OPERATION {
        unsafe { models::handle_model_operation(param) };
        return;
    }
    log::debug!("[blemesh] custom_model_cb: unhandled event={}", event);
}

/// Generic Client 事件回调 (generic_client_cb)
/// 事件值空间: esp_ble_mesh_generic_client_cb_event_t
///   0=GET_STATE, 1=SET_STATE, 2=PUBLISH, 3=TIMEOUT
/// 注意: 与 custom_model_cb 事件值空间不同, 不可共用!
extern "C" fn generic_client_event_cb(event: c_int, param: *mut c_void) {
    match event {
        EVT_GENERIC_CLIENT_GET_STATE => {
            log::info!("[blemesh] generic_client: GET_STATE (recv status)");
        }
        EVT_GENERIC_CLIENT_SET_STATE => {
            log::info!("[blemesh] generic_client: SET_STATE (recv status)");
        }
        EVT_GENERIC_CLIENT_PUBLISH => {
            log::debug!("[blemesh] generic_client: PUBLISH");
        }
        EVT_GENERIC_CLIENT_TIMEOUT => {
            log::warn!("[blemesh] generic_client: TIMEOUT (no response)");
        }
        _ => {
            log::debug!("[blemesh] generic_client_cb: unhandled event={}", event);
        }
    }
    // param 暂未使用 (TODO: 解析 onoff_status 等参数)
    let _ = param;
}
