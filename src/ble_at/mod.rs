//! BLE GATT AT 命令通道
//!
//! 在 BLE Mesh 之外额外提供一个 GATT 自定义服务, 用于设备配置阶段
//! (手机 APP 直连设备, 写入协议内容)。
//!
//! 服务结构:
//!   Service:  0000ff01-0000-1000-8000-00805f9b34fb
//!   RX Char:  0000ff02-... (Write, 主机 → 设备, AT 命令)
//!   TX Char:  0000ff03-... (Notify, 设备 → 主机, AT 响应)
//!
//! GATT 与 BLE Mesh 共存:
//! - BLE Mesh 使用 Bluedroid 协议栈 (CONFIG_BT_BLUEDROID_ENABLED)
//! - GATT 也在 Bluedroid 下注册, 与 Mesh 共享同一 controller
//! - sdkconfig 需启用: CONFIG_BT_GATTS_ENABLE + CONFIG_BT_BLE_42_FEATURES_SUPPORTED
//! - Mesh Proxy Service (0x1828) 由 Mesh 栈自动注册, 自定义服务 (0xFF01) 独立
//!
//! 属性表 (attribute table) 方式创建 GATT 服务:
//! - 一次 esp_ble_gatts_create_attr_tab 调用完成 service + char + CCCD 全部建表
//! - 协议栈按数组顺序分配 handle, HANDLE_TABLE[idx] 与表中索引一一对应
//! - ESP_GATTS_CREAT_ATTR_TAB_EVT 回调中拿到 handle 表 → start_service
//!
//! AT 命令格式 (一行, 以 \r\n 或 \n 结尾):
//!   AT+READ=<addr>                  读协议区指定地址
//!   AT+WRITE=<addr>,<value>          写协议区指定地址
//!   AT+BULKR=<start>,<len>          批量读
//!   AT+BULKW=<start>,<v1>,<v2>,...  批量写
//!   AT+COMMIT                       提交到 NVS 持久化
//!   AT+RELOAD                       从 NVS 重载
//!   AT+INFO                         查询存储区信息
//!   AT+STATUS                       查询系统状态
//!   AT+RESET                        触发设备复位
//!   AT+VERSION                     查询固件版本
//!
//! 响应格式:
//!   OK\r\n                 成功 (无数据)
//!   OK <data>\r\n          成功 (有数据)
//!   ERROR <code>\r\n       失败

pub mod parser;
pub mod handlers;
pub mod cfg_handlers;
pub mod ota_handlers;

use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use once_cell::sync::Lazy;

use crate::error::AppResult;
use crate::health::{self, TaskHb};

/// GATT 服务 UUID (16-bit, 自动扩展为 128-bit UUID)
pub const SERVICE_UUID: u16 = 0xFF01;
/// RX Characteristic (Write, 主机 → 设备)
pub const RX_CHAR_UUID: u16 = 0xFF02;
/// TX Characteristic (Notify, 设备 → 主机)
pub const TX_CHAR_UUID: u16 = 0xFF03;

/// GATT app ID (用于 esp_ble_gatts_app_register)
const GATTS_APP_ID: u16 = 0x01;
/// 服务实例 ID (单实例)
const SVC_INST_ID: u8 = 0;

// 属性表索引 (与 ATTRIBUTE_TABLE 数组顺序严格一致)
const IDX_SVC: usize = 0;          // Primary Service 声明
const IDX_CHAR_RX_DECL: usize = 1; // RX Characteristic 声明
const IDX_CHAR_RX_VAL: usize = 2;  // RX Characteristic 值
const IDX_CHAR_TX_DECL: usize = 3; // TX Characteristic 声明
const IDX_CHAR_TX_VAL: usize = 4;  // TX Characteristic 值
const IDX_CHAR_TX_CCCD: usize = 5; // TX CCCD 描述符
const ATTR_TABLE_LEN: usize = 6;

/// 运行时分配的 GATT handle 表 (CREAT_ATTR_TAB_EVT 中填充)
/// 0 = 尚未分配
static HANDLE_TABLE: Mutex<[u16; ATTR_TABLE_LEN]> = Mutex::new([0u16; ATTR_TABLE_LEN]);

/// Bluedord 分配的 GATT 接口号 (REG_EVT 中填充, -1 = 未注册)
static GATTS_IF: Mutex<i32> = Mutex::new(-1);

/// 当前 GATT 连接 ID (None = 无客户端连接)
static CONN_ID: Mutex<Option<u16>> = Mutex::new(None);

/// TX Characteristic CCCD 使能标志 (主机写 0x0001 启用 notify)
static TX_NOTIFY_ENABLED: AtomicBool = AtomicBool::new(false);

/// AT 命令处理任务心跳 (静态分配)
static TASK_HB: TaskHb = TaskHb::new("ble-at");

/// 输入缓冲区 (累计 GATT 写入, 直到遇到 \n)
/// 单条 AT 命令最长约 200 字节 (BULKW 50 个 U16), 256 字节够用
static RX_BUFFER: Lazy<Mutex<heapless::String<512>>> =
    Lazy::new(|| Mutex::new(heapless::String::new()));

/// 输出缓冲区 (notify 给主机)
static TX_BUFFER: Lazy<Mutex<heapless::String<512>>> =
    Lazy::new(|| Mutex::new(heapless::String::new()));

// ============================================================================
// Bluedroid GATT C API 绑定
// edition 2024 要求 extern block 加 unsafe 修饰
// 这些函数由 libbt.a (Bluedroid) 提供, esp_idf_sys 应已链接
// ============================================================================

/// GATT 事件类型 (esp_gatts_cb_event_t, 部分)
const ESP_GATTS_REG_EVT: c_int = 0;            // app 注册完成
const ESP_GATTS_WRITE_EVT: c_int = 4;          // 主机写入 characteristic
const ESP_GATTS_CONNECT_EVT: c_int = 5;       // 主机连接
const ESP_GATTS_DISCONNECT_EVT: c_int = 6;    // 主机断开
const ESP_GATTS_CREAT_ATTR_TAB_EVT: c_int = 11; // 属性表创建完成

/// GATT 属性权限 (esp_gatt_perm_t)
const ESP_GATT_PERM_READ: u16 = 0x0001;
const ESP_GATT_PERM_WRITE: u16 = 0x0010;

/// GATT characteristic 属性位 (esp_gatt_char_prop_t)
const ESP_GATT_CHAR_PROP_WRITE: u8 = 0x08;
const ESP_GATT_CHAR_PROP_NOTIFY: u8 = 0x10;

/// UUID 长度 (esp_uuid_len_t)
const ESP_UUID_LEN_16: u16 = 2;

/// GATT UUID 16-bit 常量 (esp_gatt_uuid_t)
const ESP_GATT_UUID_PRI_SERVICE: u16 = 0x2800;
const ESP_GATT_UUID_CHAR_DECLARE: u16 = 0x2803;
const ESP_GATT_UUID_CHAR_CLIENT_CONFIG: u16 = 0x2902;

/// 属性自动应答控制 (esp_attr_control_t.auto_rsp)
const ESP_GATT_AUTO_RSP: u8 = 1;     // 协议栈自动响应读写
const ESP_GATT_RSP_BY_APP: u8 = 0;   // 应用响应 (触发 ESP_GATTS_WRITE_EVT/READ_EVT)

/// CCCD 默认值 (notify/indicate disabled)
const CCCD_DEFAULT: [u8; 2] = [0x00, 0x00];

// 静态 UUID/属性值 (用 'static 引用传入 attr table 的 raw pointer)
// SAFETY: 这些常量在程序生命周期内不变化, 指针指向 'static 数据
static PRIMARY_SERVICE_UUID: u16 = ESP_GATT_UUID_PRI_SERVICE;
static CHAR_DECL_UUID: u16 = ESP_GATT_UUID_CHAR_DECLARE;
static CHAR_CLIENT_CONFIG_UUID: u16 = ESP_GATT_UUID_CHAR_CLIENT_CONFIG;
static RX_DECL_VALUE: u8 = ESP_GATT_CHAR_PROP_WRITE;
static TX_DECL_VALUE: u8 = ESP_GATT_CHAR_PROP_NOTIFY;

/// esp_attr_control_t (1 byte)
#[repr(C)]
#[derive(Copy, Clone)]
struct AttrControl {
    auto_rsp: u8,
}

/// esp_attr_desc_t (attribute 描述)
/// 字段顺序对齐 esp_gatt_defs.h
#[repr(C)]
#[derive(Copy, Clone)]
struct AttrDesc {
    uuid_length: u16,
    uuid_p: *const u8,
    perm: u16,
    max_length: u16,
    length: u16,
    value: *const u8,
}

/// esp_gatts_attr_db_t (attr table 条目)
#[repr(C)]
#[derive(Copy, Clone)]
struct GattsAttrDb {
    attr_control: AttrControl,
    att_desc: AttrDesc,
}

// SAFETY: GattsAttrDb 含 raw pointer, 默认 !Sync
// 此处声明可安全跨线程共享, 因为:
// 1. ATTRIBUTE_TABLE 为只读, 创建后不再修改
// 2. raw pointer 指向 'static u16/u8 常量, 生命周期与程序相同
unsafe impl Sync for GattsAttrDb {}

/// 静态属性表 (服务 + RX char + TX char + TX CCCD)
/// 协议栈按出现顺序分配 handle, HANDLE_TABLE[idx] 对应表中索引
static ATTRIBUTE_TABLE: [GattsAttrDb; ATTR_TABLE_LEN] = [
    // [0] Primary Service
    GattsAttrDb {
        attr_control: AttrControl { auto_rsp: ESP_GATT_AUTO_RSP },
        att_desc: AttrDesc {
            uuid_length: ESP_UUID_LEN_16,
            uuid_p: &PRIMARY_SERVICE_UUID as *const u16 as *const u8,
            perm: ESP_GATT_PERM_READ,
            max_length: 2,
            length: 2,
            value: &SERVICE_UUID as *const u16 as *const u8,
        },
    },
    // [1] RX Characteristic Declaration (属性位 = Write)
    GattsAttrDb {
        attr_control: AttrControl { auto_rsp: ESP_GATT_AUTO_RSP },
        att_desc: AttrDesc {
            uuid_length: ESP_UUID_LEN_16,
            uuid_p: &CHAR_DECL_UUID as *const u16 as *const u8,
            perm: ESP_GATT_PERM_READ,
            max_length: 1,
            length: 1,
            value: &RX_DECL_VALUE as *const u8,
        },
    },
    // [2] RX Characteristic Value (Write, 主机 → 设备)
    // 应用响应 (RSP_BY_APP) 以触发 ESP_GATTS_WRITE_EVT 接收数据
    GattsAttrDb {
        attr_control: AttrControl { auto_rsp: ESP_GATT_RSP_BY_APP },
        att_desc: AttrDesc {
            uuid_length: ESP_UUID_LEN_16,
            uuid_p: &RX_CHAR_UUID as *const u16 as *const u8,
            perm: ESP_GATT_PERM_WRITE,
            max_length: 512,
            length: 0,
            value: std::ptr::null(),
        },
    },
    // [3] TX Characteristic Declaration (属性位 = Notify)
    GattsAttrDb {
        attr_control: AttrControl { auto_rsp: ESP_GATT_AUTO_RSP },
        att_desc: AttrDesc {
            uuid_length: ESP_UUID_LEN_16,
            uuid_p: &CHAR_DECL_UUID as *const u16 as *const u8,
            perm: ESP_GATT_PERM_READ,
            max_length: 1,
            length: 1,
            value: &TX_DECL_VALUE as *const u8,
        },
    },
    // [4] TX Characteristic Value (Notify, 设备 → 主机)
    GattsAttrDb {
        attr_control: AttrControl { auto_rsp: ESP_GATT_AUTO_RSP },
        att_desc: AttrDesc {
            uuid_length: ESP_UUID_LEN_16,
            uuid_p: &TX_CHAR_UUID as *const u16 as *const u8,
            perm: ESP_GATT_PERM_READ,
            max_length: 512,
            length: 0,
            value: std::ptr::null(),
        },
    },
    // [5] TX CCCD (Client Characteristic Configuration Descriptor)
    // 主机写 0x0001 启用 notify, 0x0000 关闭
    GattsAttrDb {
        attr_control: AttrControl { auto_rsp: ESP_GATT_AUTO_RSP },
        att_desc: AttrDesc {
            uuid_length: ESP_UUID_LEN_16,
            uuid_p: &CHAR_CLIENT_CONFIG_UUID as *const u16 as *const u8,
            perm: ESP_GATT_PERM_READ | ESP_GATT_PERM_WRITE,
            max_length: 2,
            length: 2,
            value: &CCCD_DEFAULT as *const [u8; 2] as *const u8,
        },
    },
];

/// esp_ble_gatts_cb_param_t.write (字段顺序对齐 esp_gatts_api.h)
/// 注意 trans_id 是 u32, 非早期版本假定的 u16
#[repr(C)]
struct GattsWriteParam {
    conn_id: u16,
    _pad0: [u8; 2],
    trans_id: u32,
    handle: u16,
    offset: u16,
    need_rsp: u8,
    is_prep: u8,
    len: u16,
    value: *const u8,
}

/// esp_ble_gatts_cb_param_t.add_attr_tab (CREAT_ATTR_TAB_EVT)
/// svc_uuid 用 [u8; 20] 概括 esp_bt_uuid_t (实际 20 字节: len + pad + 16-byte union)
#[repr(C)]
struct GattsAddAttrTabParam {
    status: u8,
    _pad0: [u8; 3],
    svc_uuid: [u8; 20],
    svc_inst_id: u8,
    _pad1: [u8; 1],
    num_handle: u16,
    handles: *const u16,
}

/// esp_ble_gatts_cb_param_t.connect (CONNECT_EVT)
#[repr(C)]
struct GattsConnectParam {
    conn_id: u16,
    remote_bda: [u8; 6],
    is_connected: u8,
}

/// esp_ble_gatts_cb_param_t.disconnect (DISCONNECT_EVT)
#[repr(C)]
struct GattsDisconnectParam {
    conn_id: u16,
    remote_bda: [u8; 6],
    reason: u8,
}

/// GATT 回调函数类型
type GattsCb = extern "C" fn(event: c_int, gatts_if: c_int, param: *mut c_void);

unsafe extern "C" {
    fn esp_ble_gatts_register_callback(cb: GattsCb) -> c_int;
    fn esp_ble_gatts_app_register(app_id: u16) -> c_int;
    fn esp_ble_gatts_create_attr_tab(
        gatts_attr_db: *const GattsAttrDb,
        gatts_if: c_int,
        max_nb_attr: u16,
        srvc_inst_id: u8,
    ) -> c_int;
    fn esp_ble_gatts_start_service(handle: u16) -> c_int;
    fn esp_ble_gatts_send_response(
        gatts_if: c_int,
        conn_id: u16,
        trans_id: u32,
        status: u8,
        rsp: *const c_void,
    ) -> c_int;
    fn esp_ble_gatts_send_indicate(
        gatts_if: c_int,
        conn_id: u16,
        attr_handle: u16,
        value_len: u16,
        value: *const u8,
        need_confirm: u8,
    ) -> c_int;
}

// ============================================================================
// GATT 回调 (由 Bluedroid C 栈调用)
// ============================================================================
/// GATT 事件回调
///
/// 处理 Bluedroid GATT 事件:
/// - REG_EVT: app 注册成功 → 调用 create_attr_tab 创建 service + characteristic
/// - CREAT_ATTR_TAB_EVT: 属性表创建完成 → 拷贝 handle 表 → start_service
/// - WRITE_EVT: 主机写入 RX char (AT 命令) 或 TX CCCD (notify enable)
/// - CONNECT_EVT: 记录 conn_id 用于后续 notify
/// - DISCONNECT_EVT: 清除连接状态, 禁用 notify
extern "C" fn gatts_event_cb(event: c_int, gatts_if: c_int, param: *mut c_void) {
    match event {
        ESP_GATTS_REG_EVT => {
            // app 注册成功, gatts_if 是 Bluedroid 分配的接口号
            log::info!("[ble_at] GATT app registered, gatts_if={}", gatts_if);
            if gatts_if < 0 {
                log::warn!("[ble_at] invalid gatts_if, abort service creation");
                return;
            }
            *GATTS_IF.lock() = gatts_if;
            // 创建属性表 (触发 ESP_GATTS_CREAT_ATTR_TAB_EVT)
            // SAFETY: ATTRIBUTE_TABLE 是 'static 只读数组, 指针来源可靠
            let ret = unsafe {
                esp_ble_gatts_create_attr_tab(
                    ATTRIBUTE_TABLE.as_ptr(),
                    gatts_if,
                    ATTR_TABLE_LEN as u16,
                    SVC_INST_ID,
                )
            };
            if ret != 0 {
                log::warn!("[ble_at] create_attr_tab failed: 0x{:x}", ret);
            }
        }
        ESP_GATTS_CREAT_ATTR_TAB_EVT => {
            // 属性表创建完成, 拿到 handle 表 → 启动服务
            if param.is_null() {
                return;
            }
            // SAFETY: param 由 Bluedord 在回调中传入, 类型对齐 esp_ble_gatts_cb_param_t
            let p = unsafe { &*(param as *const GattsAddAttrTabParam) };
            if p.status != 0 || p.num_handle == 0 || p.handles.is_null() {
                log::warn!(
                    "[ble_at] attr_tab creation failed (status={}, num_handle={})",
                    p.status,
                    p.num_handle
                );
                return;
            }
            // 拷贝 handle 表到本地存储
            let n = (p.num_handle as usize).min(ATTR_TABLE_LEN);
            // SAFETY: p.handles 指向 Bluedord 内部 u16 数组, 长度 = num_handle
            let src = unsafe { std::slice::from_raw_parts(p.handles, n) };
            let mut table = HANDLE_TABLE.lock();
            for (i, &h) in src.iter().enumerate() {
                if i < table.len() {
                    table[i] = h;
                }
            }
            let svc_handle = table[IDX_SVC];
            drop(table);
            log::info!(
                "[ble_at] attr_tab created (handles={}, svc={})",
                n,
                svc_handle
            );
            // 启动服务
            // SAFETY: svc_handle 来自 Bluedord 分配, 有效
            let ret = unsafe { esp_ble_gatts_start_service(svc_handle) };
            if ret != 0 {
                log::warn!("[ble_at] start_service failed: 0x{:x}", ret);
            } else {
                log::info!("[ble_at] GATT service started (uuid={:#06x})", SERVICE_UUID);
            }
        }
        ESP_GATTS_WRITE_EVT => {
            // 主机写入 RX characteristic 或 TX CCCD
            if param.is_null() {
                return;
            }
            // SAFETY: param 类型对齐 GattsWriteParam
            let p = unsafe { &*(param as *const GattsWriteParam) };
            let handles = HANDLE_TABLE.lock();

            // CCCD 写入 (TX notify enable/disable)
            if p.handle == handles[IDX_CHAR_TX_CCCD] && p.len == 2 && !p.value.is_null() {
                // SAFETY: p.value 指向主机写入的 2 字节数据
                let v = unsafe { std::slice::from_raw_parts(p.value, 2) };
                let cccd = u16::from_le_bytes([v[0], v[1]]);
                let enabled = cccd & 0x0001 != 0;
                TX_NOTIFY_ENABLED.store(enabled, Ordering::SeqCst);
                log::info!(
                    "[ble_at] TX notify {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            } else if p.handle == handles[IDX_CHAR_RX_VAL] && !p.value.is_null() && p.len > 0 {
                // RX characteristic 写入 (AT 命令)
                // SAFETY: p.value 指向主机写入数据, 长度 p.len
                let data = unsafe { std::slice::from_raw_parts(p.value, p.len as usize) };
                feed_data(data);
                log::debug!("[ble_at] GATT write RX: {} bytes", data.len());
            }
            drop(handles);

            // 发送写入响应 (need_rsp=true 时, AUTO_RSP 的 CCCD 由协议栈自动响应)
            // 仅 RSP_BY_APP 的 RX char 需应用响应
            if p.need_rsp != 0 {
                let gatts_if = *GATTS_IF.lock();
                // SAFETY: gatts_if / conn_id / trans_id 来源可靠
                let _ = unsafe {
                    esp_ble_gatts_send_response(
                        gatts_if,
                        p.conn_id,
                        p.trans_id,
                        0, // ESP_GATT_OK
                        std::ptr::null(),
                    )
                };
            }
        }
        ESP_GATTS_CONNECT_EVT => {
            if param.is_null() {
                return;
            }
            // SAFETY: param 类型对齐 GattsConnectParam
            let p = unsafe { &*(param as *const GattsConnectParam) };
            *CONN_ID.lock() = Some(p.conn_id);
            *GATTS_IF.lock() = gatts_if;
            log::info!("[ble_at] GATT client connected (conn_id={})", p.conn_id);
        }
        ESP_GATTS_DISCONNECT_EVT => {
            if param.is_null() {
                return;
            }
            // SAFETY: param 类型对齐 GattsDisconnectParam
            let p = unsafe { &*(param as *const GattsDisconnectParam) };
            *CONN_ID.lock() = None;
            TX_NOTIFY_ENABLED.store(false, Ordering::SeqCst);
            log::info!(
                "[ble_at] GATT client disconnected (conn_id={}, reason={})",
                p.conn_id,
                p.reason
            );
        }
        _ => {
            log::debug!("[ble_at] GATT event={} gatts_if={}", event, gatts_if);
        }
    }
}

/// 启动 BLE AT 服务
///
/// 1. 注册 Bluedroid GATT 回调
/// 2. 创建 GATT app (触发 REG_EVT → create_attr_tab → CREAT_ATTR_TAB_EVT → start_service)
/// 3. 启动 AT 命令处理线程
///
/// 前置条件: Bluedroid 已初始化 (由 blemesh::start 完成)
/// sdkconfig 需启用: CONFIG_BT_GATTS_ENABLE=y
pub fn start() -> AppResult<()> {
    log::info!("[ble_at] service starting (uuid={:#06x})", SERVICE_UUID);
    health::register(&TASK_HB);

    // 1. 注册 GATT 回调
    // SAFETY: gatts_event_cb 是 extern "C" 函数, 满足 Bluedord 回调签名
    let ret = unsafe { esp_ble_gatts_register_callback(gatts_event_cb) };
    if ret != 0 {
        log::warn!("[ble_at] GATT register callback failed: 0x{:x}", ret);
        // 不阻断启动, AT 处理线程仍可运行 (后续可重试注册)
    }

    // 2. 创建 GATT app (触发 ESP_GATTS_REG_EVT)
    let ret = unsafe { esp_ble_gatts_app_register(GATTS_APP_ID) };
    if ret != 0 {
        log::warn!("[ble_at] GATT app create failed: 0x{:x}", ret);
    }

    // 3. 启动 AT 命令处理线程
    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("ble-at".into())
        .stack_size(4096)
        .spawn(process_loop);
    health::reset_thread_core();
    result.map_err(|e| crate::error::AppError::BleMesh(format!("spawn ble-at: {e}")))?;

    log::info!("[ble_at] service started (GATT callback registered, AT parser ready)");
    Ok(())
}

/// AT 命令处理循环
///
/// 每 10ms 检查 RX_BUFFER 是否有完整命令行 (以 \n 结尾),
/// 有则调用 parser::process 处理, 响应写入 TX_BUFFER 等待 notify。
fn process_loop() {
    loop {
        // 心跳: 每次 10ms 循环
        TASK_HB.tick();

        // 从 RX_BUFFER 取一行
        let cmd = {
            let mut rx = RX_BUFFER.lock();
            match rx.find('\n') {
                Some(pos) => {
                    let line: String = rx[..pos].trim_end_matches('\r').to_string();
                    // heapless::String has no replace_range; save rest then rebuild
                    let rest: String = rx[pos + 1..].to_string();
                    rx.clear();
                    let _ = rx.push_str(&rest);
                    Some(line)
                }
                None => None,
            }
        };

        if let Some(line) = cmd {
            log::debug!("[ble_at] recv: {}", line);
            let resp = parser::process(&line);
            log::debug!("[ble_at] resp: {}", resp.trim_end());

            // 写入 TX_BUFFER, 等待 notify 发送
            {
                let mut tx = TX_BUFFER.lock();
                let _ = tx.push_str(&resp);
            }

            // 尝试通过 GATT notify 发送响应
            try_send_notify();
        }

        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 尝试通过 GATT notify 发送 TX_BUFFER 中的响应
///
/// 当以下条件全部满足时实际发送:
/// 1. TX Characteristic CCCD 已被主机使能 (notify enabled)
/// 2. 当前有 GATT 客户端连接 (conn_id 有效)
/// 3. GATT 服务已注册完成 (HANDLE_TABLE 已填充, GATTS_IF 有效)
///
/// 任一条件不满足时静默回退 (数据保留在 TX_BUFFER, 等下次重试)。
fn try_send_notify() {
    // 检查 CCCD 是否使能
    if !TX_NOTIFY_ENABLED.load(Ordering::SeqCst) {
        return;
    }

    // 检查是否有客户端连接
    let conn_id = match *CONN_ID.lock() {
        Some(c) => c,
        None => return,
    };

    // 取出待发送数据
    let data = match take_response() {
        Some(d) => d,
        None => return,
    };
    if data.is_empty() {
        return;
    }

    // 检查 GATT 服务是否已注册
    let handles = HANDLE_TABLE.lock();
    let tx_handle = handles[IDX_CHAR_TX_VAL];
    drop(handles);
    if tx_handle == 0 {
        // GATT 服务尚未注册, 数据放回 TX_BUFFER 等下次重试
        let mut tx = TX_BUFFER.lock();
        let _ = tx.push_str(&data);
        return;
    }

    let gatts_if = *GATTS_IF.lock();
    // SAFETY: gatts_if / conn_id / tx_handle 来源可靠 (Bluedord 分配)
    let ret = unsafe {
        esp_ble_gatts_send_indicate(
            gatts_if,
            conn_id,
            tx_handle,
            data.len() as u16,
            data.as_ptr(),
            0, // false = notify (no confirm)
        )
    };
    if ret != 0 {
        log::warn!("[ble_at] send_indicate failed: 0x{:x} ({} bytes)", ret, data.len());
        // 放回缓冲区等待下次重试
        let mut tx = TX_BUFFER.lock();
        let _ = tx.push_str(&data);
    } else {
        log::debug!("[ble_at] notify sent: {} bytes", data.len());
    }
}

/// 接收来自 GATT write 的数据 (供 GATT 回调调用)
pub fn feed_data(data: &[u8]) {
    let mut rx = RX_BUFFER.lock();
    for &b in data {
        let _ = rx.push(b as char);
    }
}

/// 取出待发送的响应数据 (供 GATT notify 调用)
pub fn take_response() -> Option<String> {
    let mut tx = TX_BUFFER.lock();
    if tx.is_empty() {
        None
    } else {
        let s = tx.as_str().to_string();
        tx.clear();
        Some(s)
    }
}
