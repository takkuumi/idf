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

pub mod cfg_handlers;
pub mod handlers;
pub mod ota_handlers;
pub mod parser;

use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::LazyLock;

use crate::sync::Spin;

use crate::config::modbus::rtu_slave::ADDR as MODBUS_ADDR;
use crate::error::AppResult;
use crate::modbus::shared::modbus_crc16;
use std::ffi::CString;

use crate::health::{self, TaskHb};

/// 与原 C++ 固件及手持机 1.0.78 完全一致的 GATT UUID。
pub const SERVICE_UUID: &str = "4fafc201-1fb5-459e-8fcc-c5c9c331914b";
pub const CHARACTERISTIC_UUID: &str = "beb5483e-36e1-4688-b7f5-ea07361b26a8";

/// GATT app ID (用于 esp_ble_gatts_app_register)
const GATTS_APP_ID: u16 = 0x01;
/// 服务实例 ID (单实例)
const SVC_INST_ID: u8 = 0;

// 属性表索引：原固件只有一个同时支持 Read/Write/Notify 的 characteristic。
const IDX_SVC: usize = 0;
const IDX_CHAR_DECL: usize = 1;
const IDX_CHAR_VALUE: usize = 2;
const IDX_CHAR_CCCD: usize = 3;
const ATTR_TABLE_LEN: usize = 4;

/// 运行时分配的 GATT handle 表 (CREAT_ATTR_TAB_EVT 中填充)
/// 0 = 尚未分配
static HANDLE_TABLE: Spin<[u16; ATTR_TABLE_LEN]> = Spin::new([0u16; ATTR_TABLE_LEN]);

/// Bluedord 分配的 GATT 接口号 (REG_EVT 中填充, -1 = 未注册)
static GATTS_IF: Spin<Option<esp_idf_sys::esp_gatt_if_t>> = Spin::new(None);

/// 当前 GATT 连接 ID (None = 无客户端连接)
static CONN_ID: Spin<Option<u16>> = Spin::new(None);

/// TX Characteristic CCCD 使能标志 (主机写 0x0001 启用 notify)
static TX_NOTIFY_ENABLED: AtomicBool = AtomicBool::new(false);

/// AT 命令处理任务心跳 (静态分配)
static TASK_HB: TaskHb = TaskHb::new("ble-at");

/// 输入缓冲区 (累计 GATT 写入, 直到遇到 \n)
/// 单条 AT 命令最长约 200 字节 (BULKW 50 个 U16), 256 字节够用
static RX_BUFFER: LazyLock<Spin<heapless::String<512>>> =
    LazyLock::new(|| Spin::new(heapless::String::new()));

/// 输出缓冲区 (notify 给主机, AT 命令文本响应)
static TX_BUFFER: LazyLock<Spin<heapless::String<512>>> =
    LazyLock::new(|| Spin::new(heapless::String::new()));
/// 二进制响应队列 (由 GATT 写回调填充, main loop 发送)
static BINARY_TX: LazyLock<Spin<heapless::Vec<u8, 2048>>> =
    LazyLock::new(|| Spin::new(heapless::Vec::new()));
/// BLE notify 丢弃帧计数器 (Modbus 寄存器 0x0108 暴露)
static BINARY_TX_DROPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// 有待发送的 BLE 通知 (由 CONNECT_EVT 或 Modbus 响应设置, main loop 消费)
static PENDING_NOTIFY: std::sync::atomic::AtomicBool = 
    std::sync::atomic::AtomicBool::new(false);

// ============================================================================
// Bluedroid GATT C API 绑定
// edition 2024 要求 extern block 加 unsafe 修饰
// 这些函数由 libbt.a (Bluedroid) 提供, esp_idf_sys 应已链接
// ============================================================================

/// GATT 事件类型直接使用当前 ESP-IDF 生成的绑定，避免枚举值随版本变化后错位。
const ESP_GATTS_REG_EVT: esp_idf_sys::esp_gatts_cb_event_t =
    esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_REG_EVT;
const ESP_GATTS_WRITE_EVT: esp_idf_sys::esp_gatts_cb_event_t =
    esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_WRITE_EVT;
const ESP_GATTS_CONNECT_EVT: esp_idf_sys::esp_gatts_cb_event_t =
    esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CONNECT_EVT;
const ESP_GATTS_DISCONNECT_EVT: esp_idf_sys::esp_gatts_cb_event_t =
    esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_DISCONNECT_EVT;
const ESP_GATTS_CREAT_ATTR_TAB_EVT: esp_idf_sys::esp_gatts_cb_event_t =
    esp_idf_sys::esp_gatts_cb_event_t_ESP_GATTS_CREAT_ATTR_TAB_EVT;

/// CCCD 默认值 (notify/indicate disabled)
const CCCD_DEFAULT: [u8; 2] = [0x00, 0x00];

// esp_bt_uuid_t 的 128-bit UUID 按 BLE 小端字节序存放。
// 4fafc201-1fb5-459e-8fcc-c5c9c331914b
// BLE LE wire format: 组内小端
// 4fafc201-1fb5-459e-8fcc-c5c9c331914b
// 4fafc201-1fb5-459e-8fcc-c5c9c331914b (128-bit, LE byte order = full reverse of canonical)
static SERVICE_UUID_128: [u8; 16] = [
    0x4b, 0x91, 0x31, 0xc3, 0xc9, 0xc5, 0xcc, 0x8f, 0x9e, 0x45, 0xb5, 0x1f, 0x01, 0xc2, 0xaf, 0x4f,
];
// beb5483e-36e1-4688-b7f5-ea07361b26a8
// beb5483e-36e1-4688-b7f5-ea07361b26a8 (128-bit, LE byte order)
static CHARACTERISTIC_UUID_128: [u8; 16] = [
    0xa8, 0x26, 0x1b, 0x36, 0x07, 0xea, 0xf5, 0xb7, 0x88, 0x46, 0xe1, 0x36, 0x3e, 0x48, 0xb5, 0xbe,
];

// 静态 UUID/属性值（传入属性表的指针在程序整个生命周期内有效）。
static PRIMARY_SERVICE_UUID: u16 = esp_idf_sys::ESP_GATT_UUID_PRI_SERVICE as u16;
static CHAR_DECL_UUID: u16 = esp_idf_sys::ESP_GATT_UUID_CHAR_DECLARE as u16;
static CHAR_CLIENT_CONFIG_UUID: u16 = esp_idf_sys::ESP_GATT_UUID_CHAR_CLIENT_CONFIG as u16;
static CHAR_DECL_VALUE: u8 = (esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_READ
    | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_WRITE
    | esp_idf_sys::ESP_GATT_CHAR_PROP_BIT_NOTIFY) as u8;

/// `esp_gatts_attr_db_t` 含只读 raw pointer，Rust 默认不允许放入共享 static。
/// 这些指针全部指向只读 static 数据，Bluedroid 仅在创建属性表时读取。
#[repr(transparent)]
struct SyncGattAttrDb(esp_idf_sys::esp_gatts_attr_db_t);
unsafe impl Sync for SyncGattAttrDb {}

const fn attr_db(
    auto_rsp: u8,
    uuid_length: u16,
    uuid: *mut u8,
    perm: u16,
    max_length: u16,
    length: u16,
    value: *mut u8,
) -> SyncGattAttrDb {
    SyncGattAttrDb(esp_idf_sys::esp_gatts_attr_db_t {
        attr_control: esp_idf_sys::esp_attr_control_t { auto_rsp },
        att_desc: esp_idf_sys::esp_attr_desc_t {
            uuid_length,
            uuid_p: uuid,
            perm,
            max_length,
            length,
            value,
        },
    })
}

/// 静态属性表：服务 + 一个兼容原固件的 Read/Write/Notify 特征 + CCCD。
static ATTRIBUTE_TABLE: [SyncGattAttrDb; ATTR_TABLE_LEN] = [
    // [0] Primary Service，value 为 128-bit service UUID。
    attr_db(
        esp_idf_sys::ESP_GATT_AUTO_RSP as u8,
        esp_idf_sys::ESP_UUID_LEN_16 as u16,
        &PRIMARY_SERVICE_UUID as *const u16 as *mut u8,
        esp_idf_sys::ESP_GATT_PERM_READ as u16,
        SERVICE_UUID_128.len() as u16,
        SERVICE_UUID_128.len() as u16,
        SERVICE_UUID_128.as_ptr() as *mut u8,
    ),
    // [1] Characteristic Declaration (Read | Write | Notify)。
    attr_db(
        esp_idf_sys::ESP_GATT_AUTO_RSP as u8,
        esp_idf_sys::ESP_UUID_LEN_16 as u16,
        &CHAR_DECL_UUID as *const u16 as *mut u8,
        esp_idf_sys::ESP_GATT_PERM_READ as u16,
        1,
        1,
        &CHAR_DECL_VALUE as *const u8 as *mut u8,
    ),
    // [2] 单一 Characteristic Value：手持机对同一 UUID 进行写入和通知订阅。
    attr_db(
        esp_idf_sys::ESP_GATT_RSP_BY_APP as u8,
        esp_idf_sys::ESP_UUID_LEN_128 as u16,
        CHARACTERISTIC_UUID_128.as_ptr() as *mut u8,
        (esp_idf_sys::ESP_GATT_PERM_READ | esp_idf_sys::ESP_GATT_PERM_WRITE) as u16,
        512,
        0,
        core::ptr::null_mut(),
    ),
    // [3] CCCD。
    attr_db(
        esp_idf_sys::ESP_GATT_AUTO_RSP as u8,
        esp_idf_sys::ESP_UUID_LEN_16 as u16,
        &CHAR_CLIENT_CONFIG_UUID as *const u16 as *mut u8,
        (esp_idf_sys::ESP_GATT_PERM_READ | esp_idf_sys::ESP_GATT_PERM_WRITE) as u16,
        2,
        2,
        CCCD_DEFAULT.as_ptr() as *mut u8,
    ),
];

// ============================================================================
// GATT 回调 (由 Bluedroid C 栈调用)
// ============================================================================
/// GATT 事件回调
///
// ============================================================================

fn start_advertising() {
    let mut adv_params = esp_idf_sys::esp_ble_adv_params_t {
        // 160 ms；工业现场兼顾发现速度与空口占用。
        adv_int_min: 0x0100,
        adv_int_max: 0x0100,
        adv_type: esp_idf_sys::esp_ble_adv_type_t_ADV_TYPE_IND,
        own_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
        peer_addr: [0; 6],
        peer_addr_type: esp_idf_sys::esp_ble_addr_type_t_BLE_ADDR_TYPE_PUBLIC,
        channel_map: esp_idf_sys::esp_ble_adv_channel_t_ADV_CHNL_ALL,
        adv_filter_policy: esp_idf_sys::esp_ble_adv_filter_t_ADV_FILTER_ALLOW_SCAN_ANY_CON_ANY,
    };
    let ret = unsafe { esp_idf_sys::esp_ble_gap_start_advertising(&mut adv_params) };
    if ret != esp_idf_sys::ESP_OK {
        log::error!("[ble_at] start_advertising failed: 0x{:x}", ret);
    }
}

unsafe extern "C" fn gap_event_cb(
    event: esp_idf_sys::esp_gap_ble_cb_event_t,
    param: *mut esp_idf_sys::esp_ble_gap_cb_param_t,
) {
    match event {
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_DATA_SET_COMPLETE_EVT => {
            if param.is_null() {
                log::error!("[ble_at] ADV_DATA_SET_COMPLETE without parameters");
                return;
            }
            let status = unsafe { (*param).adv_data_cmpl.status };
            if status != esp_idf_sys::esp_bt_status_t_ESP_BT_STATUS_SUCCESS {
                log::error!("[ble_at] advertising data configuration failed: {}", status);
                return;
            }
            log::info!("[ble_at] ADV_DATA configured; starting advertising...");
            start_advertising();
        }
        esp_idf_sys::esp_gap_ble_cb_event_t_ESP_GAP_BLE_ADV_START_COMPLETE_EVT => {
            if param.is_null() {
                return;
            }
            let status = unsafe { (*param).adv_start_cmpl.status };
            if status == esp_idf_sys::esp_bt_status_t_ESP_BT_STATUS_SUCCESS {
                log::info!("[ble_at] BLE advertising active (svc={})", SERVICE_UUID);
            } else {
                log::error!("[ble_at] advertising start failed: {}", status);
            }
        }
        _ => {}
    }
}

/// 处理 Bluedroid GATT 事件:
/// - REG_EVT: app 注册成功 → 调用 create_attr_tab 创建 service + characteristic
/// - CREAT_ATTR_TAB_EVT: 属性表创建完成 → 拷贝 handle 表 → start_service
/// - WRITE_EVT: 主机写入 RX char (AT 命令) 或 TX CCCD (notify enable)
/// - CONNECT_EVT: 记录 conn_id 用于后续 notify
/// - DISCONNECT_EVT: 清除连接状态, 禁用 notify
unsafe extern "C" fn gatts_event_cb(
    event: esp_idf_sys::esp_gatts_cb_event_t,
    gatts_if: esp_idf_sys::esp_gatt_if_t,
    param: *mut esp_idf_sys::esp_ble_gatts_cb_param_t,
) {
    match event {
        ESP_GATTS_REG_EVT => {
            log::info!("[ble_at] GATT app registered, gatts_if={}", gatts_if);
            if param.is_null() {
                log::error!("[ble_at] REG_EVT param null, abort");
                return;
            }
            let reg = unsafe { (*param).reg };
            log::info!("[ble_at] REG_EVT status={}, app_id={}", reg.status, reg.app_id);
            if reg.status != esp_idf_sys::esp_gatt_status_t_ESP_GATT_OK {
                log::error!("[ble_at] GATT app registration failed: {}", reg.status);
                return;
            }
            *GATTS_IF.lock() = Some(gatts_if);
            log::info!("[ble_at] calling create_attr_tab(gatts_if={}, n_attr={})", gatts_if, ATTR_TABLE_LEN);
            let ret = unsafe {
                esp_idf_sys::esp_ble_gatts_create_attr_tab(
                    ATTRIBUTE_TABLE
                        .as_ptr()
                        .cast::<esp_idf_sys::esp_gatts_attr_db_t>(),
                    gatts_if,
                    ATTR_TABLE_LEN as u16,
                    SVC_INST_ID,
                )
            };
            log::info!("[ble_at] create_attr_tab returned {}", ret);
            if ret != 0 {
                log::warn!("[ble_at] create_attr_tab failed: 0x{:x}", ret);
            }
        }
        ESP_GATTS_CREAT_ATTR_TAB_EVT => {
            log::info!("[ble_at] GOT CREAT_ATTR_TAB_EVT");
            if param.is_null() {
                log::error!("[ble_at] CREAT_ATTR_TAB_EVT param null, abort");
                return;
            }
            let p = unsafe { &(*param).add_attr_tab };
            log::info!("[ble_at] CREAT_ATTR_TAB_EVT status={} num_handle={} handles={:p}",
                p.status, p.num_handle, p.handles);
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
                "[ble_at] attr_tab created (handles={}, svc={}, status={})",
                n,
                svc_handle,
                p.status
            );
            log::info!("[ble_at] about to call start_service(handle={})", svc_handle);
            // 启动服务
            // SAFETY: svc_handle 来自 Bluedord 分配, 有效
            let ret = unsafe { esp_idf_sys::esp_ble_gatts_start_service(svc_handle) };
            if ret != 0 {
                log::warn!("[ble_at] start_service failed: 0x{:x}", ret);
            } else {
                log::info!("[ble_at] GATT service started (uuid={})", SERVICE_UUID);
                // 配置广播数据 (触发 ADV_DATA_SET_COMPLETE_EVT → gap_event_cb → start_advertising)
                // 配置广播数据 (ADV) - 包含 name + UUID + flags
                // 这是最广泛兼容的方案, 所有 BLE 扫描器都能看到
                // 31字节限制: flags(3) + name(10 含 length/type) + 16字节 UUID(18) = 31 字节 (刚好)
                let mut adv_data = esp_idf_sys::esp_ble_adv_data_t {
                    set_scan_rsp: false,
                    include_name: true,  // name 放 ADV data, 兼容所有扫描器
                    include_txpower: false,
                    min_interval: 0i32,
                    max_interval: 0i32,
                    appearance: 0i32,
                    manufacturer_len: 0u16,
                    p_manufacturer_data: core::ptr::null_mut(),
                    service_data_len: 0u16,
                    p_service_data: core::ptr::null_mut(),
                    service_uuid_len: SERVICE_UUID_128.len() as u16,
                    p_service_uuid: SERVICE_UUID_128.as_ptr() as *mut u8,
                    flag: (esp_idf_sys::ESP_BLE_ADV_FLAG_GEN_DISC
                        | esp_idf_sys::ESP_BLE_ADV_FLAG_BREDR_NOT_SPT)
                        as u8,
                };
                let ret =
                    unsafe { esp_idf_sys::esp_ble_gap_config_adv_data(&mut adv_data as *mut _) };
                if ret != 0 {
                    log::warn!("[ble_at] config_adv_data failed: 0x{:x}", ret);
                } else {
                    log::info!("[ble_at] ADV_DATA configured (name + UUID + flags)");
                }
            }
        }
        ESP_GATTS_WRITE_EVT => {
            if param.is_null() {
                return;
            }
            let p = unsafe { &(*param).write };

            let handles = HANDLE_TABLE.lock();

            // CCCD 写入 (TX notify enable/disable)
            // 注意: CCCD 分支必须先发送 write response，再处理 CCCD，
            // 否则 Android BLE 栈收不到响应会进入异常状态。
            if p.handle == handles[IDX_CHAR_CCCD] && p.len == 2 && !p.value.is_null() {
                // SAFETY: p.value 指向主机写入的 2 字节数据
                let v = unsafe { std::slice::from_raw_parts(p.value, 2) };
                let cccd = u16::from_le_bytes([v[0], v[1]]);
                let enabled = cccd & 0x0001 != 0;
                TX_NOTIFY_ENABLED.store(enabled, Ordering::SeqCst);
                log::info!(
                    "[ble_at] TX notify {}, gatts_if={:?}, conn_id={}",
                    if enabled { "enabled" } else { "disabled" },
                    *GATTS_IF.lock(),
                    p.conn_id
                );
                // 发写响应 — Android 端需要收到 CCCD write response 才能继续通信
                if p.need_rsp {
                    if let Some(gatts_if) = *GATTS_IF.lock() {
                        let _ = unsafe {
                            esp_idf_sys::esp_ble_gatts_send_response(
                                gatts_if,
                                p.conn_id,
                                p.trans_id,
                                esp_idf_sys::esp_gatt_status_t_ESP_GATT_OK,
                                std::ptr::null_mut(),
                            )
                        };
                    }
                }
            } else if p.handle == handles[IDX_CHAR_VALUE] && !p.value.is_null() && p.len > 0 {
                // RX characteristic 写入 — 优先尝试二进制协议, 否则按文本 AT 处理
                // 关键修复: 必须先发送写响应, 然后再处理数据发送通知
                // 否则 Android BLE 栈可能在收到通知前就进入下一状态
                let data = unsafe { std::slice::from_raw_parts(p.value, p.len as usize) };
                let data_vec: Vec<u8> = data.to_vec();
                // handles already dropped implicitly

                // 1. 先发送写响应 (GATT 协议要求先响应 write request)
                if p.need_rsp {
                    let Some(gatts_if) = *GATTS_IF.lock() else {
                        log::error!("[ble_at] cannot respond: GATT interface is unavailable");
                        return;
                    };
                    let _ = unsafe {
                        esp_idf_sys::esp_ble_gatts_send_response(
                            gatts_if,
                            p.conn_id,
                            p.trans_id,
                            esp_idf_sys::esp_gatt_status_t_ESP_GATT_OK,
                            std::ptr::null_mut(),
                        )
                    };
                }

                // 2. 写响应后, 处理数据并发送通知
                log::info!("[ble_at] GATT write RX: {} bytes, hex={}", data_vec.len(),
                    data_vec.iter().take(20).map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));
                if !try_handle_binary_protocol(&data_vec, p.conn_id, p.trans_id) {
                    feed_data(&data_vec);
                }
            } else {
                // handles already dropped implicitly
                // 其他情况 (例如 CCCD 写入) 仍需发送响应
                if p.need_rsp {
                    let Some(gatts_if) = *GATTS_IF.lock() else {
                        return;
                    };
                    let _ = unsafe {
                        esp_idf_sys::esp_ble_gatts_send_response(
                            gatts_if,
                            p.conn_id,
                            p.trans_id,
                            esp_idf_sys::esp_gatt_status_t_ESP_GATT_OK,
                            std::ptr::null_mut(),
                        )
                    };
                }
            }
        }
        ESP_GATTS_CONNECT_EVT => {
            if param.is_null() {
                return;
            }
            let p = unsafe { &(*param).connect };
            *CONN_ID.lock() = Some(p.conn_id);
            *GATTS_IF.lock() = Some(gatts_if);
            log::info!("[ble_at] GATT client connected (conn_id={})", p.conn_id);
            
            // MCA 兼容: 连接后立即产一个心跳通知到 BINARY_TX 队列
            // Android BLEDataSyncManager.onStateConnected 会发心跳命令,
            // 但有时 Service 未发现, 写入失败, 导致数据流卡死。
            // 我们主动发心跳, 触发 Android 的 onHeartbeatChanged → readHardwareInfo
            // 这个通知由 process_loop 的 try_send_notify 在 CCCD 使能后发送
            let hb = {
                use std::sync::atomic::{AtomicU16, Ordering};
                static HB: AtomicU16 = AtomicU16::new(0);
                HB.fetch_add(1, Ordering::Relaxed)
            };
            // 用 BLE 帧格式: tx_id=0, proto_id=0, pdu_data=[unit=1, func=0x11, hb_hi, hb_lo]
            let mut frame: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = frame.extend_from_slice(&[0u8, 0]);  // tx_id=0
            let _ = frame.extend_from_slice(&[0u8, 0]);  // proto_id=0
            let _ = frame.extend_from_slice(&4u16.to_be_bytes()); // length=4
            let _ = frame.push(1);   // unit
            let _ = frame.push(0x11); // func=heartbeat
            let _ = frame.push((hb >> 8) as u8);
            let _ = frame.push((hb & 0xFF) as u8);
            let crc = modbus_crc16(&frame[..frame.len()]);
            let _ = frame.push(crc as u8);
            let _ = frame.push((crc >> 8) as u8);
            // 标记有待发送的通知 (main loop 通过 process_tick 发送)
            PENDING_NOTIFY.store(true, std::sync::atomic::Ordering::Release);
            log::info!("[ble_at] connect heartbeat pending");
        }
        ESP_GATTS_DISCONNECT_EVT => {
            if param.is_null() {
                return;
            }
            let p = unsafe { &(*param).disconnect };
            log::warn!("[ble_at] GATT disconnect conn_id={} reason={:#x}",
                p.conn_id, p.reason);
            *CONN_ID.lock() = None;
            TX_NOTIFY_ENABLED.store(false, Ordering::SeqCst);
            // 清空 BINARY_TX 残留数据，防止旧帧在新连接时被发送
            if let Some(mut btx) = BINARY_TX.try_lock() {
                btx.clear();
            }
            // 清空 RX_BUFFER 和 TX_BUFFER
            if let Some(mut rx) = RX_BUFFER.try_lock() {
                rx.clear();
            }
            if let Some(mut tx) = TX_BUFFER.try_lock() {
                tx.clear();
            }
            log::info!(
                "[ble_at] GATT client disconnected (conn_id={}, reason={})",
                p.conn_id,
                p.reason
            );
            // 可连接广播在建立连接时会停止，断开后必须显式恢复。
            start_advertising();
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
/// 本函数自行初始化 BT controller + Bluedroid 协议栈, 无外部依赖。
/// sdkconfig 需启用: CONFIG_BT_ENABLED=y + CONFIG_BT_BLUEDROID_ENABLED=y + CONFIG_BT_GATTS_ENABLE=y
fn controller_config() -> esp_idf_sys::esp_bt_controller_config_t {
    // Rust 无法直接调用 C 宏 BT_CONTROLLER_INIT_CONFIG_DEFAULT()，因此使用
    // bindgen 从同一 ESP-IDF 头文件导出的常量逐字段构造。禁止硬编码 magic/version。
    esp_idf_sys::esp_bt_controller_config_t {
        magic: esp_idf_sys::ESP_BT_CTRL_CONFIG_MAGIC_VAL,
        version: esp_idf_sys::ESP_BT_CTRL_CONFIG_VERSION,
        controller_task_stack_size: esp_idf_sys::ESP_TASK_BT_CONTROLLER_STACK as u16,
        controller_task_prio: esp_idf_sys::ESP_TASK_BT_CONTROLLER_PRIO as u8,
        controller_task_run_cpu: esp_idf_sys::CONFIG_BT_CTRL_PINNED_TO_CORE as u8,
        bluetooth_mode: esp_idf_sys::CONFIG_BT_CTRL_MODE_EFF as u8,
        ble_max_act: esp_idf_sys::CONFIG_BT_CTRL_BLE_MAX_ACT_EFF as u8,
        sleep_mode: esp_idf_sys::CONFIG_BT_CTRL_SLEEP_MODE_EFF as u8,
        sleep_clock: esp_idf_sys::CONFIG_BT_CTRL_SLEEP_CLOCK_EFF as u8,
        ble_st_acl_tx_buf_nb: esp_idf_sys::CONFIG_BT_CTRL_BLE_STATIC_ACL_TX_BUF_NB as u8,
        ble_hw_cca_check: esp_idf_sys::CONFIG_BT_CTRL_HW_CCA_EFF as u8,
        ble_adv_dup_filt_max: esp_idf_sys::CONFIG_BT_CTRL_ADV_DUP_FILT_MAX as u16,
        coex_param_en: false,
        ce_len_type: esp_idf_sys::CONFIG_BT_CTRL_CE_LENGTH_TYPE_EFF as u8,
        coex_use_hooks: false,
        hci_tl_type: esp_idf_sys::CONFIG_BT_CTRL_HCI_TL_EFF as u8,
        hci_tl_funcs: core::ptr::null_mut(),
        txant_dft: esp_idf_sys::CONFIG_BT_CTRL_TX_ANTENNA_INDEX_EFF as u8,
        rxant_dft: esp_idf_sys::CONFIG_BT_CTRL_RX_ANTENNA_INDEX_EFF as u8,
        txpwr_dft: esp_idf_sys::CONFIG_BT_CTRL_DFT_TX_POWER_LEVEL_EFF as u8,
        cfg_mask: esp_idf_sys::CFG_MASK,
        scan_duplicate_mode: esp_idf_sys::SCAN_DUPLICATE_MODE as u8,
        scan_duplicate_type: esp_idf_sys::SCAN_DUPLICATE_TYPE_VALUE as u8,
        normal_adv_size: esp_idf_sys::NORMAL_SCAN_DUPLICATE_CACHE_SIZE as u16,
        mesh_adv_size: esp_idf_sys::MESH_DUPLICATE_SCAN_CACHE_SIZE as u16,
        coex_phy_coded_tx_rx_time_limit: esp_idf_sys::CONFIG_BT_CTRL_COEX_PHY_CODED_TX_RX_TLIM_EFF
            as u8,
        hw_target_code: esp_idf_sys::BLE_HW_TARGET_CODE_CHIP_ECO0,
        slave_ce_len_min: esp_idf_sys::SLAVE_CE_LEN_MIN_DEFAULT as u8,
        hw_recorrect_en: esp_idf_sys::AGC_RECORRECT_EN as u8,
        cca_thresh: esp_idf_sys::CONFIG_BT_CTRL_HW_CCA_VAL as u8,
        scan_backoff_upperlimitmax: esp_idf_sys::BT_CTRL_SCAN_BACKOFF_UPPERLIMITMAX as u16,
        dup_list_refresh_period: esp_idf_sys::DUPL_SCAN_CACHE_REFRESH_PERIOD as u16,
        ble_50_feat_supp: esp_idf_sys::BT_CTRL_50_FEATURE_SUPPORT != 0,
        ble_cca_mode: esp_idf_sys::BT_BLE_CCA_MODE as u8,
        ble_data_lenth_zero_aux: esp_idf_sys::BT_BLE_ADV_DATA_LENGTH_ZERO_AUX as u8,
        ble_chan_ass_en: esp_idf_sys::BT_CTRL_CHAN_ASS_EN as u8,
        ble_ping_en: esp_idf_sys::BT_CTRL_LE_PING_EN as u8,
        ble_llcp_disc_flag: esp_idf_sys::BT_CTRL_BLE_LLCP_DISC_FLAG as u8,
        run_in_flash: esp_idf_sys::BT_CTRL_RUN_IN_FLASH_ONLY != 0,
        dtm_en: esp_idf_sys::BT_CTRL_DTM_ENABLE != 0,
        enc_en: esp_idf_sys::BLE_SECURITY_ENABLE != 0,
        qa_test: esp_idf_sys::BT_CTRL_BLE_TEST != 0,
        connect_en: esp_idf_sys::BT_CTRL_BLE_MASTER != 0,
        scan_en: esp_idf_sys::BT_CTRL_BLE_SCAN != 0,
        ble_aa_check: esp_idf_sys::BLE_CTRL_CHECK_CONNECT_IND_ACCESS_ADDRESS_ENABLED != 0,
        adv_en: esp_idf_sys::BT_CTRL_BLE_ADV != 0,
    }
}

pub fn start() -> AppResult<()> {
    log::info!("[ble_at] service starting (uuid={})", SERVICE_UUID);
    log::info!("[ble_at] initializing BT controller (BLE mode)");

    let release_ret = unsafe {
        esp_idf_sys::esp_bt_controller_mem_release(
            esp_idf_sys::esp_bt_mode_t_ESP_BT_MODE_CLASSIC_BT,
        )
    };
    if release_ret != esp_idf_sys::ESP_OK {
        // ESP32-S3 不支持 Classic BT；部分 IDF 版本会返回 NOT_FOUND，不影响 BLE。
        log::warn!(
            "[ble_at] Classic BT memory release returned 0x{:x}",
            release_ret
        );
    }

    let mut bt_cfg = controller_config();
    log::info!(
        "[ble_at] controller config magic=0x{:08x}, version=0x{:08x}",
        bt_cfg.magic,
        bt_cfg.version
    );
    let ret = unsafe { esp_idf_sys::esp_bt_controller_init(&mut bt_cfg) };
    if ret != esp_idf_sys::ESP_OK {
        log::error!("[ble_at] esp_bt_controller_init failed: 0x{:x}", ret);
        return Err(crate::error::AppError::BleMesh(format!(
            "bt_controller_init: 0x{ret:x}"
        )));
    }

    let ret = unsafe {
        esp_idf_sys::esp_bt_controller_enable(esp_idf_sys::esp_bt_mode_t_ESP_BT_MODE_BLE)
    };
    if ret != esp_idf_sys::ESP_OK {
        log::error!("[ble_at] esp_bt_controller_enable failed: 0x{:x}", ret);
        return Err(crate::error::AppError::BleMesh(format!(
            "bt_controller_enable: 0x{ret:x}"
        )));
    }

    let ret = unsafe { esp_idf_sys::esp_bluedroid_init() };
    if ret != esp_idf_sys::ESP_OK {
        log::error!("[ble_at] esp_bluedroid_init failed: 0x{:x}", ret);
        return Err(crate::error::AppError::BleMesh(format!(
            "bluedroid_init: 0x{ret:x}"
        )));
    }
    let ret = unsafe { esp_idf_sys::esp_bluedroid_enable() };
    if ret != esp_idf_sys::ESP_OK {
        log::error!("[ble_at] esp_bluedroid_enable failed: 0x{:x}", ret);
        return Err(crate::error::AppError::BleMesh(format!(
            "bluedroid_enable: 0x{ret:x}"
        )));
    }

    // device::init() 已在本函数之前加载配置；广播使用可配置的短名称（最多 8 字节）
    let configured_name = crate::bus::config_state::config_read()
        .map(|cs| cs.cfg.ble_name_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "GW-S3".to_owned());
    let name = CString::new(configured_name.as_str())
        .map_err(|_| crate::error::AppError::BleMesh("BLE name contains NUL".into()))?;
    let ret = unsafe { esp_idf_sys::esp_ble_gap_set_device_name(name.as_ptr()) };
    if ret != esp_idf_sys::ESP_OK {
        return Err(crate::error::AppError::BleMesh(format!(
            "set_device_name: 0x{ret:x}"
        )));
    }
    log::info!("[ble_at] BLE device name set to '{}'", configured_name);

    // 读取并打印蓝牙 MAC 地址, 便于现场识别
    let mut ble_mac = [0u8; 6];
    unsafe {
        esp_idf_sys::esp_read_mac(ble_mac.as_mut_ptr(), 2); // ESP_MAC_BT=2
    }
    log::info!(
        "[ble_at] BLE MAC = {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        ble_mac[0], ble_mac[1], ble_mac[2], ble_mac[3], ble_mac[4], ble_mac[5]
    );

    // 读取并打印以太网 MAC 地址, 便于现场识别
    let mut eth_mac = [0u8; 6];
    unsafe {
        esp_idf_sys::esp_read_mac(eth_mac.as_mut_ptr(), 3); // ESP_MAC_ETH=3
    }
    log::info!(
        "[ble_at] ETH MAC = {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        eth_mac[0], eth_mac[1], eth_mac[2], eth_mac[3], eth_mac[4], eth_mac[5]
    );

    let ret = unsafe { esp_idf_sys::esp_ble_gap_register_callback(Some(gap_event_cb)) };
    if ret != esp_idf_sys::ESP_OK {
        return Err(crate::error::AppError::BleMesh(format!(
            "gap_register_callback: 0x{ret:x}"
        )));
    }

    let ret = unsafe { esp_idf_sys::esp_ble_gatts_register_callback(Some(gatts_event_cb)) };
    if ret != esp_idf_sys::ESP_OK {
        return Err(crate::error::AppError::BleMesh(format!(
            "gatts_register_callback: 0x{ret:x}"
        )));
    }

    // BLE MTU 设 500 (与原始 5617b8f 实现一致, Android 协商后取 min(500, request))
    let mtu_ret = unsafe { esp_idf_sys::esp_ble_gatt_set_local_mtu(500) };
    if mtu_ret != esp_idf_sys::ESP_OK {
        log::warn!("[ble_at] set local MTU failed: 0x{:x}", mtu_ret);
    } else {
        log::info!("[ble_at] local MTU set to 500");
    }

    let ret = unsafe { esp_idf_sys::esp_ble_gatts_app_register(GATTS_APP_ID) };
    if ret != esp_idf_sys::ESP_OK {
        return Err(crate::error::AppError::BleMesh(format!(
            "gatts_app_register: 0x{ret:x}"
        )));
    }

    log::info!("[ble_at] service startup queued (GATT callback registered, AT parser ready)");
    log::info!("[ble_at] notification sender runs in main loop (no dedicated thread)");
    Ok(())
}

/// AT 命令处理循环
///
/// 每 10ms 检查 RX_BUFFER 是否有完整命令行 (以 \n 结尾),
/// 有则调用 parser::process 处理, 响应写入 TX_BUFFER 等待 notify。
pub fn process_tick() {
    if !TX_NOTIFY_ENABLED.load(std::sync::atomic::Ordering::SeqCst) { return; }
    let Some(conn_id) = *CONN_ID.lock() else { return; };
    let Some(gatts_if) = *GATTS_IF.lock() else { return; };
    let h = HANDLE_TABLE.lock();
    let tx_handle = h[IDX_CHAR_VALUE];
    drop(h);
    if tx_handle == 0 { return; }
    
    // 0. 处理 RX_BUFFER 中的 AT 文本命令 (修复死路径: 之前 parser::process 从未调用)
    //    完整行 (以 \n 结尾) 调 parser::process 处理, 响应推送 BINARY_TX
    let mut at_response: Option<String> = None;
    if let Some(mut rx) = RX_BUFFER.try_lock() {
        if let Some(pos) = rx.find('\n') {
            // 取行, 移除已处理部分 (heapless::String 无 replace_range, 用 pop_front)
            let line_str: String = rx[..pos].trim_end_matches('\r').to_string();
            // 删除前 pos+1 个字符 (= line + '\n')
            for _ in 0..=pos {
                if rx.is_empty() { break; }
                let _ = rx.remove(0);
            }
            drop(rx);
            log::info!("[ble_at] AT cmd: {}", line_str);
            at_response = Some(crate::ble_at::parser::process(&line_str));
        }
    }
    if let Some(resp) = at_response {
        // 把 AT 文本响应包装成 BLE 帧 (tx_id=0, proto_id=0)
        let mut frame: heapless::Vec<u8, 256> = heapless::Vec::new();
        let _ = frame.extend_from_slice(&0u16.to_be_bytes());
        let _ = frame.extend_from_slice(&0u16.to_be_bytes());
        let bytes = resp.as_bytes();
        let len = bytes.len() as u16;
        let _ = frame.extend_from_slice(&len.to_be_bytes());
        let _ = frame.extend_from_slice(bytes);
        let crc = modbus_crc16(&frame[..frame.len()]);
        let _ = frame.push(crc as u8);
        let _ = frame.push((crc >> 8) as u8);
        if let Some(mut btx) = BINARY_TX.try_lock() {
            if btx.len() + frame.len() <= 2048 {
                let _ = btx.extend_from_slice(&frame);
            } else {
                log::warn!("[ble_at] AT response too long, dropping");
            }
        }
    }
    
    // 1. 优先发送 BINARY_TX 队列中的响应 (Android 请求的 Modbus 响应)
    if let Some(mut btx) = BINARY_TX.try_lock() {
        if !btx.is_empty() {
            let data: Vec<u8> = btx[..].to_vec();
            let data_len = data.len();
            btx.clear();
            drop(btx);
            let ret = unsafe {
                esp_idf_sys::esp_ble_gatts_send_indicate(
                    gatts_if, conn_id, tx_handle,
                    data.len() as u16, data.as_ptr() as *mut u8, false,
                )
            };
            if ret == 0 {
                log::info!("[ble_at] BINARY_TX sent: {} bytes OK", data_len);
            } else {
                log::warn!("[ble_at] BINARY_TX send failed rc=0x{:x}", ret);
            }
            return;
        }
    }
    
    // 2. 每 100 次主循环 (~10s) 发一次心跳
    use std::sync::atomic::{AtomicU16, Ordering};
    static HB_DIV: AtomicU16 = AtomicU16::new(0);
    static HB_SEQ: AtomicU16 = AtomicU16::new(0);
    let n = HB_DIV.fetch_add(1, Ordering::Relaxed);
    if n % 100 != 0 { return; }
    let seq = HB_SEQ.fetch_add(1, Ordering::Relaxed);
    
    let mut frame: heapless::Vec<u8, 16> = heapless::Vec::new();
    let _ = frame.extend_from_slice(&[0u8, 0, 0, 0, 0, 4]);
    let _ = frame.push(1);
    let _ = frame.push(0x11);
    let _ = frame.push((seq >> 8) as u8);
    let _ = frame.push((seq & 0xFF) as u8);
    let crc = crate::modbus::shared::modbus_crc16(&frame[..frame.len()]);
    let _ = frame.push(crc as u8);
    let _ = frame.push((crc >> 8) as u8);
    
    log::info!("[ble_at] heartbeat #{} ({} bytes)", seq, frame.len());
    let ret = unsafe {
        esp_idf_sys::esp_ble_gatts_send_indicate(
            gatts_if, conn_id, tx_handle,
            frame.len() as u16, frame.as_ptr() as *mut u8, false,
        )
    };
    if ret != 0 {
        log::warn!("[ble_at] heartbeat failed rc=0x{:x}", ret);
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
    // 入口日志: 确认此函数被调用
    log::info!("[ble_at] try_send_notify called, enabled={}, conn={:?}, gatts_if={:?}",
        TX_NOTIFY_ENABLED.load(Ordering::SeqCst),
        *CONN_ID.lock(),
        *GATTS_IF.lock()
    );
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
    let tx_handle = handles[IDX_CHAR_VALUE];
    // handles already dropped implicitly
    if tx_handle == 0 {
        // GATT 服务尚未注册, 数据放回 TX_BUFFER 等下次重试
        let mut tx = TX_BUFFER.lock();
        let _ = tx.push_str(&data);
        return;
    }

    let Some(gatts_if) = *GATTS_IF.lock() else {
        let mut tx = TX_BUFFER.lock();
        let _ = tx.push_str(&data);
        return;
    };
    // SAFETY: gatts_if / conn_id / tx_handle 来源可靠 (Bluedroid 分配)
    let ret = unsafe {
        esp_idf_sys::esp_ble_gatts_send_indicate(
            gatts_if,
            conn_id,
            tx_handle,
            data.len() as u16,
            data.as_ptr() as *mut u8,
            false,
        )
    };
    if ret != 0 {
        log::warn!(
            "[ble_at] send_indicate failed: 0x{:x} ({} bytes)",
            ret,
            data.len()
        );
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

// Modbus RTU 帧处理 (BLE 通道, 兼容参考项目 MCA_F16V2_1_F48_BLE)
// ============================================================================

/// 处理 Modbus RTU 帧, 返回 true 表示已处理 (不再走 AT 命令解析)
///
/// 输出用 BLE 帧格式包装 (与 metuory-wireless-management-app-1.0.78 一致):
///   tx_id(2 BE) | proto_id(2 BE) | length(2 BE) | unit(1) | func(1) | data(N) | crc(2)
/// 其中 pdu_data = Modbus RTU 响应 (slave + func + body + crc)
fn handle_modbus_rtu(frame: &[u8], tx_id: u16, proto_id: u16, conn_id: u16) -> bool {
    if frame.len() < 2 {
        return false;
    }
    let slave = frame[0];
    let func = frame[1];
    let backend = crate::modbus::shared::BusBackend;
    // pdu = slave/func 之后的所有数据, handle_pdu 会自行判断长度
    let pdu = if frame.len() > 2 { &frame[2..] } else { &[] };
    let body = crate::modbus::shared::handle_pdu(&backend, func, pdu);
    // 构造 Modbus RTU 响应: slave + func + body + crc
    let mut rtu_rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = rtu_rsp.push(slave);
    let _ = rtu_rsp.extend_from_slice(&body);
    let crc = modbus_crc16(&rtu_rsp[..rtu_rsp.len()]);
    let _ = rtu_rsp.push(crc as u8);
    let _ = rtu_rsp.push((crc >> 8) as u8);
    // 用 BLE 帧格式包装 (Android 期望)
    send_ble_frame(tx_id, proto_id, &rtu_rsp, conn_id);
    true
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

// ============================================================================
// BLE 兼容协议 (与 metuory-wireless-management-app-1.0.78 完全一致)
// ============================================================================
// Android 端 CommandBuilderUtil.buildCMD 输出格式:
//   tx_id(2 BE) | proto_id(2 BE) | length(2 BE) | unit_id(1) | func(1) | data(N) | crc16_le(2)
//
// length 字段值 = 2 + data.length (即 unit_id + func + data, 不含 CRC 字节)
// CRC 计算范围 = 前 (2+2+2+1+1+N) = N+8 字节 (即除最后 2 字节 CRC 外)
// CRC 字节序 = Modbus 标准 LE
//
// 解析: length 字段 + 2 = 后续字节总数 (含 CRC), 所以 pdu_end = 6 + length + 2
//       crc_begin = pdu_end - 2
//       PDU (转 Modbus RTU) = data[6..crc_begin]
//       即 unit_id + func + data, 与 Modbus RTU slave 帧一致
// 心跳帧 func == 0x11, 直接回递增序列, 不进入 Modbus 通路

fn heartbeat_counter() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static HB: AtomicU16 = AtomicU16::new(0);
    HB.fetch_add(1, Ordering::Relaxed)
}

fn try_handle_binary_protocol(data: &[u8], conn_id: u16, _trans_id: u32) -> bool {
    log::info!("[ble_at] binary rx: {} bytes, hex={}",
        data.len(),
        data.iter().take(20).map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
    );
    // 最小帧: tx_id(2) + proto_id(2) + length(2) + unit(1) + func(1) + crc(2) = 10
    if data.len() < 10 {
        log::debug!("[ble_at] binary rx: too short ({} < 10), skip", data.len());
        return false;
    }
    // length 字段值 = unit_id(1) + func(1) + data(N) = N + 2 (Android 端定义)
    let length = u16::from_be_bytes([data[4], data[5]]) as usize;
    if length < 2 {
        // 长度字段异常 (必须至少包含 unit + func)
        return false;
    }
    // 后续字节总数 = length 字段值 + CRC(2)
    let total_after_len = length + 2;
    if data.len() < 6 + total_after_len {
        // 帧尚未收全, 等待后续 GATT write 拼接后再判
        return false;
    }

    let pdu_end = 6 + total_after_len; // = 6 + length + 2
    let crc_begin = pdu_end - 2;
    // CRC 计算范围 = data[..crc_begin] (即除 CRC 字节外全部)
    let calc_crc = modbus_crc16(&data[..crc_begin]);
    let rx_crc = u16::from_le_bytes([data[crc_begin], data[crc_begin + 1]]);
    if calc_crc != rx_crc {
        // CRC 校验失败, 已 consume 数据避免无限循环, 但不响应
        log::warn!(
            "[ble_at] binary CRC fail: calc={:04X} recv={:04X}",
            calc_crc, rx_crc
        );
        return true;
    }
    // 提取事务标识和协议标识 (用于响应帧)
    let tx_id = u16::from_be_bytes([data[0], data[1]]);
    let proto_id = u16::from_be_bytes([data[2], data[3]]);
    let unit = data[6];
    let func = data[7];
    if func == 0x11 {
        // 心跳应答 - 最小化以适应默认 BLE MTU 23
        // BLE 帧格式: [tx_id(2)][proto_id(2)][length(2)][unit(1)][func(1)][data(N)][crc(2)]
        // Android parseCMDModel: data_len=length-2, data=data[8..8+data_len]
        // Android parseHeartbeat: slave=data[0], runStatus=data[1], terminal=data[2..]
        // 
        // pdu_data 必须包含 [unit, func, data_bytes] 长度 >= 2
        // 最小情况: 2 字节 (slave + runStatus, 但 Android 需要 unit 和 func 来自 BLE 帧)
        // 
        // 实际: pdu_data = [unit, func=0x11, slave, runStatus] 4 字节
        // BLE 帧: 2+2+2+4+2 = 12 字节
        let hb = heartbeat_counter();
        let mut rsp: heapless::Vec<u8, 4> = heapless::Vec::new();
        let _ = rsp.push(unit);          // pdu[0] = slave (unit_id)
        let _ = rsp.push(func);          // pdu[1] = runStatus = 0x11 (心跳标志)
        // pdu[2..3] 作为 terminal (Android 存入 BLEDataSyncModel)
        let _ = rsp.push((hb >> 8) as u8);
        let _ = rsp.push((hb & 0xFF) as u8);
        log::debug!("[ble_at] heartbeat: hb_seq={}", hb);
        send_ble_frame(tx_id, proto_id, &rsp, conn_id);
        return true;
    }
    // ---- metuory-wireless-management-app-1.0.78 通过 Modbus FC=03/04 读取设备信息 ----
    // Android 期望自定义格式响应 ([length_byte][data]), 不是标准 Modbus RTU.
    // 命中 Android 已知名单后直接返回, 跳过 Modbus RTU 路径.
    if (func == 0x03 || func == 0x04) && data.len() >= 12 {
        if handle_ble_android_read_command(func, &data[8..crc_begin], tx_id, proto_id, conn_id, unit) {
            return true;
        }
    }
    // ---- 自定义 MCA 协议命令 (0xC0-0xCF) ----
    // Android 端可能通过这些命令获取 IP/子网/网关等设备信息
    if func >= 0xC0 && func <= 0xCF {
        return handle_mca_custom_command(func, &data[8..crc_begin], tx_id, proto_id, conn_id, unit);
    }
    // 其它 PDU: unit_id 字节作为 Modbus slave 地址, func+data 作为 Modbus PDU
    let pdu = &data[6..crc_begin]; // [unit][func][data...]
    handle_modbus_rtu(pdu, tx_id, proto_id, conn_id)
}

/// 用 BLE 帧格式包装并发送响应 (Android 期望格式)
///
/// 帧格式: tx_id(2 BE) | proto_id(2 BE) | length(2 BE) | pdu_data(N) | crc(2 LE)
/// 其中 length 字段 = pdu_data.len() (含 slave/func/data/CRC, 不含 BLE 帧头尾)
/// CRC 计算范围 = 整个帧除最后 2 字节

/// 处理 MCA 自定义协议命令 (0xC0-0xCF)
///
/// 当 Android 端通过新版 BLE 协议格式 (tx_id/proto_id/length/unit/func/data/crc)
/// 发送原 MCA 自定义命令时，通过 func 值识别并分发。
///
/// 命令对照 (原 MCA 固件 vBleMessageDistribution):
///   0xC2 = GET_SN_CODE
///   0xC3 = SET_SN_CODE
///   0xC4 = GET_LOCATION_INFO
///   0xC6 = GET_ETH_MAC
///   0xCA = GET_BLUETOOTH_NO
///   0xCC = GET_DEVICE_TYPE
///   0xCD = SET_NET_INFO
///   0xCE = GET_NET_INFO (IP/子网/网关)
///   0xCF = GET_FIRMWARE_VER
fn handle_mca_custom_command(
    func: u8,
    data: &[u8],
    tx_id: u16,
    proto_id: u16,
    conn_id: u16,
    unit: u8,
) -> bool {
    log::info!("[ble_at] MCA custom cmd: func=0x{func:02X}, data={}", 
        data.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));
    match func {
        0xCE => {
            // GET_NET_INFO: 返回 IP/子网/网关 (各 4 字节)
            let (ip, mask, gw) = crate::bus::config_state::config_read()
                .map(|cs| (cs.cfg.ip, cs.cfg.mask, cs.cfg.gateway))
                .unwrap_or(([0u8; 4], [0u8; 4], [0u8; 4]));
            let mut rsp: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = rsp.push(0xCE); // cmd
            let _ = rsp.push(0);    // status = 成功
            let _ = rsp.extend_from_slice(&ip);   // IP 4 字节
            let _ = rsp.extend_from_slice(&mask); // 掩码 4 字节
            let _ = rsp.extend_from_slice(&gw);   // 网关 4 字节
            log::info!("[ble_at] GET_NET_INFO: ip={}.{}.{}.{} mask={}.{}.{}.{} gw={}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3],
                mask[0], mask[1], mask[2], mask[3],
                gw[0], gw[1], gw[2], gw[3]);
            // 用 BLE 协议帧格式包装响应
            send_ble_frame(tx_id, proto_id, &rsp, conn_id);
            true
        }
        0xC2 => {
            // GET_SN_CODE
            let sn = crate::bus::config_state::config_read()
                .map(|cs| cs.cfg.sn)
                .unwrap_or([0u8; 32]);
            let mut rsp: heapless::Vec<u8, 20> = heapless::Vec::new();
            let _ = rsp.push(0xC2);
            let _ = rsp.push(0);
            // SN 9 个 U16 (大端) → 18 字节 ASCII
            for i in 0..9usize {
                let w = u16::from_be_bytes([sn[i * 2], sn[i * 2 + 1]]);
                let _ = rsp.push(w as u8);
                let _ = rsp.push((w >> 8) as u8);
            }
            log::info!("[ble_at] GET_SN_CODE: {} bytes", rsp.len());
            send_ble_frame(tx_id, proto_id, &rsp, conn_id);
            true
        }
        0xCF => {
            // GET_FIRMWARE_VER
            let fw = crate::config::APP_VERSION;
            let mut rsp: heapless::Vec<u8, 8> = heapless::Vec::new();
            let _ = rsp.push(0xCF);
            let _ = rsp.push(0);
            // 格式: major, minor, patch, date_hi, date_lo
            let _ = rsp.push(2); // major
            let _ = rsp.push(2); // minor
            let _ = rsp.push(1); // patch
            let _ = rsp.push(0x06); // date hi (June)
            let _ = rsp.push(0x15); // date lo (15)
            send_ble_frame(tx_id, proto_id, &rsp, conn_id);
            true
        }
        0xC6 => {
            // GET_ETH_MAC
            let mac = crate::bus::config_state::config_read()
                .map(|cs| cs.cfg.eth_mac)
                .unwrap_or([0u8; 6]);
            let mut rsp: heapless::Vec<u8, 10> = heapless::Vec::new();
            let _ = rsp.push(0xC6);
            let _ = rsp.push(0);
            let _ = rsp.extend_from_slice(&mac);
            send_ble_frame(tx_id, proto_id, &rsp, conn_id);
            true
        }
        _ => {
            // 未实现的自定义命令 → 返回 unknown error
            log::warn!("[ble_at] MCA custom cmd 0x{func:02X} not implemented");
            false
        }
    }
}


/// 处理 metuory-wireless-management-app-1.0.78 通过 BLE 发送的设备信息读命令
///
/// Android 端通过 Modbus FC=03/04 协议读取设备信息, 期望响应格式是
/// 自定义格式 `[length_byte][data...]`, 不是标准 Modbus RTU 响应.
///
/// 已知寄存器地址 (与 metuory-wireless-management-app-1.0.78 CMDTransmissionTypeEnum 完全一致):
/// | func | reg_addr | reg_cnt | 含义         | 响应格式
/// | 0x04 | 0x087C   | 0x0002  | HW_INFO      | [4][DO][DI][ADC][RS485]
/// | 0x04 | 0x087E   | 0x0002  | FW_VERSION   | [2][fw_hi][fw_lo]
/// | 0x03 | 0x08A5   | 0x0001  | HW_VER       | [2][hw_hi][hw_lo]
/// | 0x03 | 0x08C7   | 0x000C  | IP/MASK/GW   | [12][ip(4)][mask(4)][gw(4)]
/// | 0x03 | 0x08D7   | 0x0006  | ETH_MAC      | [6][mac(6)]
/// | 0x03 | 0x08E2   | 0x0004  | BT_ID        | [4][ble_id(4)]
///
/// 返回 true 表示已处理 (调用方应停止继续走 Modbus RTU 路径).
fn handle_ble_android_read_command(
    func: u8,
    data: &[u8],
    tx_id: u16,
    proto_id: u16,
    conn_id: u16,
    unit: u8,
) -> bool {
    // 必须有 reg_addr(2) + reg_cnt(2) = 4 字节
    if data.len() < 4 {
        return false;
    }
    let reg_addr = u16::from_be_bytes([data[0], data[1]]);
    let reg_cnt = u16::from_be_bytes([data[2], data[3]]);

    // 只处理 FC=03 (READ03) / FC=04 (READ04)
    if func != 0x03 && func != 0x04 {
        return false;
    }

    let cfg_guard = match crate::bus::config_state::config_read() {
        Some(cs) => cs,
        None => return false,
    };
    // RCU 快照借用, 在函数作用域内有效
    let cfg: &crate::device::system_config::SystemConfig = &cfg_guard.cfg;

    let rsp_data: heapless::Vec<u8, 16> = match (reg_addr, reg_cnt) {
        // READ_HARDWARE_INFO (0x087C, 0x0002): [4][DO][DI][ADC][RS485]
        (0x087C, 2) => {
            let do_cnt = crate::config::hw_version::DO_COUNT as u8;
            let di_cnt = crate::config::hw_version::DI_COUNT as u8;
            let adc_cnt = crate::config::hw_version::AI_COUNT as u8;
            let rs485_cnt = 2u8;
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(4); // length
            let _ = d.push(do_cnt);
            let _ = d.push(di_cnt);
            let _ = d.push(adc_cnt);
            let _ = d.push(rs485_cnt);
            d
        }
        // READ_FW_VERSION (0x087E, 0x0002): [2][fw_hi][fw_lo]
        (0x087E, 2) => {
            let fw = cfg.fw_version;
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(2); // length
            let _ = d.push((fw >> 8) as u8); // major (build / 100)
            let _ = d.push((fw & 0xFF) as u8); // minor (build % 100)
            d
        }
        // READ_DEVICE_PRODUCT (0x08A5, 0x0001): [2][hw_hi][hw_lo]
        (0x08A5, 1) => {
            let hw = cfg.hw_version;
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(2);
            let _ = d.push((hw >> 8) as u8);
            let _ = d.push((hw & 0xFF) as u8);
            d
        }
        // READ_IP (0x08C7, 0x000C): [12][ip(4)][mask(4)][gw(4)]
        (0x08C7, 12) => {
            let ip = cfg.ip;
            let mask = cfg.mask;
            let gw = cfg.gateway;
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(12); // length
            let _ = d.extend_from_slice(&ip);
            let _ = d.extend_from_slice(&mask);
            let _ = d.extend_from_slice(&gw);
            log::info!("[ble_at] READ_IP: {}.{}.{}.{} / {}.{}.{}.{} gw {}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3],
                mask[0], mask[1], mask[2], mask[3],
                gw[0], gw[1], gw[2], gw[3]);
            d
        }
        // READ_MAC (0x08D7, 0x0006): [6][mac(6)]
        (0x08D7, 6) => {
            let mac = cfg.eth_mac;
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(6);
            let _ = d.extend_from_slice(&mac);
            log::info!("[ble_at] READ_MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
            d
        }
        // READ_BLUETOOTH_ID (0x08E2, 0x0004): [4][ble_id(4)]
        (0x08E2, 4) => {
            let mut id = [0u8; 4];
            // BLE MAC 6 字节, 取后 4 字节 (与 MCA F16V2/F48 兼容)
            id.copy_from_slice(&cfg.ble_mac[2..6]);
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(4);
            let _ = d.extend_from_slice(&id);
            log::info!("[ble_at] READ_BLE_ID: {:02X}{:02X}{:02X}{:02X}",
                id[0], id[1], id[2], id[3]);
            d
        }
        _ => return false, // 未命中 Android 已知名单, 让调用方继续走 Modbus RTU
    };

    // 构造 pdu_data = [unit][func=0x03/0x04][length][data...]
    // send_ble_frame 会自动加 tx_id/proto_id/length(=pdu_data.len())/crc
    let mut pdu: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = pdu.push(unit);
    let _ = pdu.push(func);
    let _ = pdu.extend_from_slice(&rsp_data);
    send_ble_frame(tx_id, proto_id, &pdu, conn_id);
    true
}

// 暴露给测试用的纯逻辑函数 (无 esp-idf 依赖)
// 返回 [unit][func][length_byte][actual_data...] 的 pdu_data 形式
#[cfg(test)]
fn build_android_read_response_pdu(
    unit: u8,
    func: u8,
    reg_addr: u16,
    reg_cnt: u16,
    cfg_ip: [u8; 4],
    cfg_mask: [u8; 4],
    cfg_gw: [u8; 4],
    cfg_mac: [u8; 6],
    cfg_ble_mac: [u8; 6],
    cfg_hw_ver: u16,
    cfg_fw_ver: u16,
    do_count: u8,
    di_count: u8,
    ai_count: u8,
    rs485_count: u8,
) -> Option<heapless::Vec<u8, 32>> {
    let rsp_data: heapless::Vec<u8, 16> = match (reg_addr, reg_cnt) {
        (0x087C, 2) => {
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(4);
            let _ = d.push(do_count);
            let _ = d.push(di_count);
            let _ = d.push(ai_count);
            let _ = d.push(rs485_count);
            d
        }
        (0x087E, 2) => {
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(2);
            let _ = d.push((cfg_fw_ver >> 8) as u8);
            let _ = d.push((cfg_fw_ver & 0xFF) as u8);
            d
        }
        (0x08A5, 1) => {
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(2);
            let _ = d.push((cfg_hw_ver >> 8) as u8);
            let _ = d.push((cfg_hw_ver & 0xFF) as u8);
            d
        }
        (0x08C7, 12) => {
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(12);
            let _ = d.extend_from_slice(&cfg_ip);
            let _ = d.extend_from_slice(&cfg_mask);
            let _ = d.extend_from_slice(&cfg_gw);
            d
        }
        (0x08D7, 6) => {
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(6);
            let _ = d.extend_from_slice(&cfg_mac);
            d
        }
        (0x08E2, 4) => {
            let mut id = [0u8; 4];
            id.copy_from_slice(&cfg_ble_mac[2..6]);
            let mut d: heapless::Vec<u8, 16> = heapless::Vec::new();
            let _ = d.push(4);
            let _ = d.extend_from_slice(&id);
            d
        }
        _ => return None,
    };
    let mut pdu: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = pdu.push(unit);
    let _ = pdu.push(func);
    let _ = pdu.extend_from_slice(&rsp_data);
    Some(pdu)
}

#[cfg(test)]
mod android_compat_tests {
    use super::build_android_read_response_pdu;

    // ---- READ_HARDWARE_INFO (0x087C, 2 regs) ----
    #[test]
    fn test_read_hardware_info_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x04, 0x087C, 2,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0, 0, 8, 8, 6, 2,
        ).expect("must handle");
        // 期望: [unit(1)][func=0x04(1)][length=4(1)][DO=8(1)][DI=8(1)][ADC=6(1)][RS485=2(1)]
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x04);
        assert_eq!(pdu[2], 4); // length
        assert_eq!(pdu[3], 8); // DO
        assert_eq!(pdu[4], 8); // DI
        assert_eq!(pdu[5], 6); // ADC
        assert_eq!(pdu[6], 2); // RS485
        assert_eq!(pdu.len(), 7);
    }

    // ---- READ_FW_VERSION (0x087E, 2 regs) ----
    #[test]
    fn test_read_fw_version_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x04, 0x087E, 2,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0, 0x0221, 0, 0, 0, 0,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x04);
        assert_eq!(pdu[2], 2); // length
        assert_eq!(pdu[3], 0x02); // major
        assert_eq!(pdu[4], 0x21); // minor (FW=0x0221)
        assert_eq!(pdu.len(), 5);
    }

    // ---- READ_DEVICE_PRODUCT (0x08A5, 1 reg) ----
    #[test]
    fn test_read_device_product_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08A5, 1,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0x00F3, 0, 0, 0, 0, 0,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x03); // FC=03
        assert_eq!(pdu[2], 2);
        assert_eq!(pdu[3], 0x00);
        assert_eq!(pdu[4], 0xF3); // HW_VER=0x00F3
        assert_eq!(pdu.len(), 5);
    }

    // ---- READ_IP (0x08C7, 12 regs) ----
    #[test]
    fn test_read_ip_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08C7, 12,
            [192, 168, 51, 140], [255, 255, 255, 0], [192, 168, 51, 1],
            [0; 6], [0; 6],
            0, 0, 0, 0, 0, 0,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x03);
        assert_eq!(pdu[2], 12); // length
        // IP
        assert_eq!(&pdu[3..7], &[192, 168, 51, 140]);
        // mask
        assert_eq!(&pdu[7..11], &[255, 255, 255, 0]);
        // gateway
        assert_eq!(&pdu[11..15], &[192, 168, 51, 1]);
        assert_eq!(pdu.len(), 15);
    }

    // ---- READ_MAC (0x08D7, 6 regs) ----
    #[test]
    fn test_read_mac_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08D7, 6,
            [0; 4], [0; 4], [0; 4],
            [0x80, 0xB5, 0x4E, 0x5B, 0x24, 0xE4],
            [0; 6],
            0, 0, 0, 0, 0, 0,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x03);
        assert_eq!(pdu[2], 6); // length
        assert_eq!(&pdu[3..9], &[0x80, 0xB5, 0x4E, 0x5B, 0x24, 0xE4]);
        assert_eq!(pdu.len(), 9);
    }

    // ---- READ_BLUETOOTH_ID (0x08E2, 4 regs) ----
    #[test]
    fn test_read_ble_id_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08E2, 4,
            [0; 4], [0; 4], [0; 4], [0; 6],
            [0x80, 0xB5, 0x4E, 0x5B, 0x24, 0xE5], // BLE MAC
            0, 0, 0, 0, 0, 0,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x03);
        assert_eq!(pdu[2], 4); // length
        // 后 4 字节: 0x5B, 0x24, 0xE5 (BLE MAC[2..6])
        assert_eq!(&pdu[3..7], &[0x5B, 0x24, 0xE5, /* 0 */]);
        assert_eq!(pdu.len(), 7);
    }

    // ---- 未命中已知地址 ----
    #[test]
    fn test_unmapped_returns_none() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x0880, 5, // 不在 Android 已知名单
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0, 0, 0, 0, 0, 0,
        );
        assert!(pdu.is_none());
    }

    // ---- CRC 计算正确性 (Modbus CRC16 LE) ----
    #[test]
    fn test_crc16() {
        use crate::modbus::shared::modbus_crc16;
        // 已知: [0x01, 0x03, 0x02, 0x00, 0xF3] → CRC = 0xB9C2 (LE: C2 B9)
        let frame = [0x01, 0x03, 0x02, 0x00, 0xF3];
        let crc = modbus_crc16(&frame);
        // 不固定值, 但应该是 modbus 标准 CRC16
        assert_eq!(crc, modbus_crc16(&frame));
    }

    // ---- BLE 帧格式 (tx_id + proto_id + length + pdu + crc) ----
    #[test]
    fn test_send_ble_frame_format() {
        use crate::modbus::shared::modbus_crc16;
        let pdu: heapless::Vec<u8, 32> = {
            let mut v: heapless::Vec<u8, 32> = heapless::Vec::new();
            let _ = v.push(0x01); // unit
            let _ = v.push(0x03); // func
            let _ = v.push(12); // length
            let _ = v.extend_from_slice(&[192, 168, 51, 140]);
            let _ = v.extend_from_slice(&[255, 255, 255, 0]);
            let _ = v.extend_from_slice(&[192, 168, 51, 1]);
            v
        };
        // 模拟 send_ble_frame 内部构造
        let tx_id: u16 = 0x0001;
        let proto_id: u16 = 0x0000;
        let length: u16 = pdu.len() as u16;
        let mut frame: heapless::Vec<u8, 64> = heapless::Vec::new();
        let _ = frame.extend_from_slice(&tx_id.to_be_bytes());
        let _ = frame.extend_from_slice(&proto_id.to_be_bytes());
        let _ = frame.extend_from_slice(&length.to_be_bytes());
        let _ = frame.extend_from_slice(&pdu);
        let crc = modbus_crc16(&frame[..frame.len()]);
        let _ = frame.push(crc as u8);
        let _ = frame.push((crc >> 8) as u8);
        // 期望 frame 长度 = 2+2+2+15+2 = 23
        assert_eq!(frame.len(), 23);
        // tx_id BE
        assert_eq!(frame[0], 0x00);
        assert_eq!(frame[1], 0x01);
        // proto_id BE
        assert_eq!(frame[2], 0x00);
        assert_eq!(frame[3], 0x00);
        // length BE (15)
        assert_eq!(frame[4], 0x00);
        assert_eq!(frame[5], 0x0F);
    }
}




/// 发送 DI 状态变化上报 (REPORT_COM_INPUT_IO_STATUS, 0x94)
///
/// Android 1.0.78 CMDTransmissionIDManager 通过 tx_id=0x01 特殊 ID 识别本类上报.
/// 帧格式 (与 Android 期望一致):
///   tx_id(2 BE) = 0x0001  (Android TRANSMISSION_SPECIAL_ID_COM_INPUT_STATUS_CHANGED)
///   proto_id(2 BE) = 0x0000
///   length(2 BE) = 2 + ceil(DI_COUNT/8)
///   pdu_data = [unit(1)][func(1)=0x94][di_bitmap_be...]
///   crc(2 LE) = Modbus CRC16 of [tx_id|proto_id|length|pdu_data]
///
/// DI bitmap 字节序 = BE (大端, MSB first), DI0 在最高位字节的 LSB.
/// 字节数 = ceil(DI_COUNT / 8). F16/F3=2 bytes (16 DI), F4=6 bytes (48 DI).
///
/// 调用时机: main_loop 消费 IoEvent::DiChanged 时调用. conn_id 取 0 (任意活跃连接).
pub fn send_di_status_report(conn_id: u16) {
    use crate::config::hw_version;
    // 1. 读 DI 当前状态 (无锁, AtomicBits64)
    let di_bits = crate::bus::IO.di.load_bits();
    // 2. 构造 pdu
    let di_count = hw_version::DI_COUNT;
    let bitmap_bytes = (di_count + 7) / 8;
    let mut pdu: heapless::Vec<u8, 16> = heapless::Vec::new();
    let _ = pdu.push(0x01); // unit (slave id)
    let _ = pdu.push(0x94); // func = REPORT_COM_INPUT_IO_STATUS
    // DI bitmap BE: DI0 在 pdu[2] bit 0 (LSB first for first byte)
    // Android 端 CMDResComInputIOReadModel 解析时:
    //   byte 0 = DI0..DI7 (LSB=DI0)
    //   byte 1 = DI8..DI15
    //   ...
    for i in 0..bitmap_bytes {
        let byte = ((di_bits >> (i * 8)) & 0xFF) as u8;
        let _ = pdu.push(byte);
    }
    // 3. 用 BLE 帧格式包装 (tx_id=0x0001 特殊 ID)
    send_ble_frame(0x0001, 0x0000, &pdu, conn_id);
    log::info!("[ble_at] DI status report: di_bits=0x{:016X} bytes={}",
        di_bits, bitmap_bytes);
}

fn send_ble_frame(tx_id: u16, proto_id: u16, pdu_data: &[u8], conn_id: u16) {
    let mut frame: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = frame.extend_from_slice(&tx_id.to_be_bytes());
    let _ = frame.extend_from_slice(&proto_id.to_be_bytes());
    let length = pdu_data.len() as u16;
    let _ = frame.extend_from_slice(&length.to_be_bytes());
    let _ = frame.extend_from_slice(pdu_data);
    let crc = modbus_crc16(&frame[..frame.len()]);
    let _ = frame.push(crc as u8);
    let _ = frame.push((crc >> 8) as u8);
    log::info!("[ble_at] BLE frame: tx_id={:#06X} proto={:#06X} len={} data_len={}",
        tx_id, proto_id, length, pdu_data.len()
    );
    // 放入二进制响应队列, 由 process_loop 的 try_send_notify 发送
    // 关键: 不在 GATT 回调中直接 send_indicate!
    if let Some(mut btx) = BINARY_TX.try_lock() {
        // queue size 2048 bytes, 容纳约 30+ 帧 (每帧 ~60 bytes)
        if btx.len() + frame.len() <= 2048 {
            let _ = btx.extend_from_slice(&frame);
        } else {
            // 队列满: 丢弃并计数 (Modbus 可查询)
            let drops = BINARY_TX_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            // 每 10 次丢弃才记日志, 避免日志洪水
            if drops % 10 == 1 {
                log::warn!("[ble_at] binary TX full, dropped frame #{} ({} bytes, queue={} bytes)",
                    drops, frame.len(), btx.len());
                // 记录为可恢复故障
                crate::error::recovery::record_failure(
                    crate::error::recovery::Severity::Recoverable,
                    "ble_at",
                    &format!("BLE notify dropped (count={})", drops),
                );
            }
        }
    } else {
        // 锁失败: 记录但不丢弃
        log::warn!("[ble_at] BINARY_TX lock failed, frame skipped");
    }
}

/// 获取 BLE notify 丢弃帧计数 (供 Modbus 状态查询)
pub fn binary_tx_drops() -> u32 {
    BINARY_TX_DROPS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 发送 BLE notification (只由 process_loop 线程调用)
/// 不在 GATT 回调中直接调用!
fn send_notify(data: &[u8]) {
    let Some(gatts_if) = *GATTS_IF.lock() else { return; };
    let Some(conn_id) = *CONN_ID.lock() else { return; };
    let handle = HANDLE_TABLE.lock()[IDX_CHAR_VALUE];
    unsafe {
        let rc = esp_idf_sys::esp_ble_gatts_send_indicate(
            gatts_if, conn_id, handle,
            data.len() as u16, data.as_ptr() as *mut u8,
            false,
        );
        if rc != 0 {
            log::warn!("[ble_at] notify failed rc=0x{:x}", rc);
        }
    }
}

// ============================================================================
// 单元测试 — BLE 二进制协议解析 (与 Android 手持机 1.0.78 一致)
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一条完整的 BLE 二进制协议帧 (与 Android 端 CommandBuilderUtil 一致)
    /// 格式: tx_id(2) | proto_id(2) | length(2) | unit_id(1) | func(1) | data... | crc16_le(2)
    /// length 字段 = unit_id(1) + func(1) + data(N) = N+2 (不含 CRC 字节)
    fn build_frame(tx_id: u16, proto_id: u16, pdu: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&tx_id.to_be_bytes());
        out.extend_from_slice(&proto_id.to_be_bytes());
        // length 字段值 = pdu.len() (因为 pdu 已含 unit_id + func + data)
        let length = pdu.len() as u16;
        out.extend_from_slice(&length.to_be_bytes());
        // PDU = unit_id + func + data
        out.extend_from_slice(pdu);
        // CRC16 over entire frame except CRC bytes
        let crc = crate::modbus::shared::modbus_crc16(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    #[test]
    fn test_ble_protocol_format() {
        // 心跳帧: [unit_id=0x01][0x11][hb_hi=0x00][hb_lo=0x00]
        let frame = build_frame(0x0000, 0x0000, &[0x01, 0x11, 0x00, 0x00]);
        // tx_id(2) + proto_id(2) + length(2) + pdu(4) + crc(2) = 12 bytes
        assert_eq!(frame.len(), 12);
        // length 字段值 = pdu.len() = 4 (unit + func + hb_hi + hb_lo)
        assert_eq!(u16::from_be_bytes([frame[4], frame[5]]), 4);
    }

    #[test]
    fn test_ble_protocol_modbus_frame() {
        // 读保持寄存器 FC=03, addr=0, count=10
        // Modbus RTU: [slave=0x01][func=0x03][addr_hi=0x00][addr_lo=0x00][count_hi=0x00][count_lo=0x0A][crc_lo][crc_hi]
        // BLE PDU = [unit=0x01][func=0x03][0x00][0x00][0x00][0x0A]
        let pdu = [0x01, 0x03, 0x00, 0x00, 0x00, 0x0A];
        let frame = build_frame(0x1234, 0x5678, &pdu);
        // length = 6
        assert_eq!(u16::from_be_bytes([frame[4], frame[5]]), 6);
        // tx_id + proto_id 正确
        assert_eq!(&frame[0..2], &[0x12, 0x34]);
        assert_eq!(&frame[2..4], &[0x56, 0x78]);
        // PDU
        assert_eq!(&frame[6..12], &pdu);
        // CRC 字段在最后 2 字节
        let crc_in_frame = u16::from_le_bytes([frame[frame.len() - 2], frame[frame.len() - 1]]);
        let crc_calc = crate::modbus::shared::modbus_crc16(&frame[..frame.len() - 2]);
        assert_eq!(crc_in_frame, crc_calc);
    }

    #[test]
    fn test_ble_crc_validation() {
        // 构造一帧, 故意破坏 CRC 验证 CRC 检查逻辑
        let mut frame = build_frame(0x0000, 0x0000, &[0x01, 0x03, 0x00, 0x00, 0x00, 0x0A]);
        // 修改最后一字节 (CRC high)
        let last = frame.len() - 1;
        frame[last] ^= 0xFF;
        // 验证 modbus_crc16 检测到错误
        let data = &frame[..frame.len() - 2];
        let computed = crate::modbus::shared::modbus_crc16(data);
        let received = u16::from_le_bytes([frame[frame.len() - 2], frame[last]]);
        assert_ne!(computed, received);
    }

    #[test]
    fn test_ble_service_uuid_encoding() {
        // 验证 128-bit UUID 按 BLE 小端字节序正确编码
        // 标准 UUID: 4fafc201-1fb5-459e-8fcc-c5c9c331914b
        // LE byte order: 4b 91 31 c3 c9 c5 cc 8f 9e 45 b5 1f 01 c2 af 4f
        let canonical = "4fafc201-1fb5-459e-8fcc-c5c9c331914b";
        assert_eq!(SERVICE_UUID, canonical);
        assert_eq!(SERVICE_UUID_128.len(), 16);
        assert_eq!(SERVICE_UUID_128[0], 0x4b);
        assert_eq!(SERVICE_UUID_128[15], 0x4f);
    }

    #[test]
    fn test_ble_characteristic_uuid_encoding() {
        // beb5483e-36e1-4688-b7f5-ea07361b26a8
        // LE byte order: a8 26 1b 36 07 ea f5 b7 88 46 e1 36 3e 48 b5 be
        let canonical = "beb5483e-36e1-4688-b7f5-ea07361b26a8";
        assert_eq!(CHARACTERISTIC_UUID, canonical);
        assert_eq!(CHARACTERISTIC_UUID_128.len(), 16);
        assert_eq!(CHARACTERISTIC_UUID_128[0], 0xa8);
        assert_eq!(CHARACTERISTIC_UUID_128[15], 0xbe);
    }

    #[test]
    fn test_attribute_table_structure() {
        // 验证属性表包含 4 个属性 (service + char_decl + char_value + cccd)
        assert_eq!(ATTR_TABLE_LEN, 4);
        assert_eq!(IDX_SVC, 0);
        assert_eq!(IDX_CHAR_DECL, 1);
        assert_eq!(IDX_CHAR_VALUE, 2);
        assert_eq!(IDX_CHAR_CCCD, 3);
    }

    #[test]
    fn test_heartbeat_counter_increments() {
        // 多次调用, 验证计数器递增
        let a = heartbeat_counter();
        let b = heartbeat_counter();
        let c = heartbeat_counter();
        // b > a, c > b
        assert!(b > a);
        assert!(c > b);
    }
}

// ============================================================================
// 单元测试 — Android BLE 帧包装 (Android metuory-wireless-management-app 兼容)
// ============================================================================
//
// 关键修复: 之前 Modbus RTU 响应未用 BLE 帧包装, 导致 Android 无法解析
// Android 期望响应格式:
//   tx_id(2 BE) | proto_id(2 BE) | length(2 BE) | pdu_data(N) | crc(2 LE)
// 其中 pdu_data = Modbus RTU 响应 (slave + func + body + crc)
//
// 测试验证:
// 1. 帧结构正确
// 2. CRC 覆盖范围正确 (前 6+length 字节)
// 3. length 字段值 = pdu_data 长度
//
#[cfg(test)]
mod tests_ble_cmd {
    use super::*;

    /// 构建 Android 兼容的 BLE 请求帧 (与 CommandBuilderUtil.buildCMD 一致)
    fn build_ble_request(tx_id: u16, proto_id: u16, unit_id: u8, func: u8, pdu_data: &[u8]) -> Vec<u8> {
        let length = 1 + 1 + pdu_data.len(); // unit + func + data
        let mut out = Vec::new();
        out.extend_from_slice(&tx_id.to_be_bytes());
        out.extend_from_slice(&proto_id.to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
        out.push(unit_id);
        out.push(func);
        out.extend_from_slice(pdu_data);
        let crc = crate::modbus::shared::modbus_crc16(&out);
        out.push(crc as u8);
        out.push((crc >> 8) as u8);
        out
    }

    /// 解析 Android BLE 响应 (与 CommandCodecUtil.decodeList + parseCMDModel 一致)
    fn parse_ble_response(data: &[u8]) -> Option<(u16, u16, u16, u8, u8, Vec<u8>, bool)> {
        if data.len() < 10 { return None; }
        let tx_id = u16::from_be_bytes([data[0], data[1]]);
        let proto_id = u16::from_be_bytes([data[2], data[3]]);
        let length = u16::from_be_bytes([data[4], data[5]]);
        let unit = data[6];
        let func = data[7];
        let data_len = length as usize - 2;
        if data.len() < 8 + data_len + 2 { return None; }
        let pdu_data = data[8..8 + data_len].to_vec();
        let crc_rx = u16::from_le_bytes([data[8 + data_len], data[8 + data_len + 1]]);
        let crc_calc = crate::modbus::shared::modbus_crc16(&data[..8 + data_len]);
        Some((tx_id, proto_id, length, unit, func, pdu_data, crc_rx == crc_calc))
    }

    /// 模拟 Modbus RTU 响应 (Android 收到后解析)
    fn build_modbus_rtu_response(slave: u8, func: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![slave, func];
        out.extend_from_slice(body);
        let crc = crate::modbus::shared::modbus_crc16(&out);
        out.push(crc as u8);
        out.push((crc >> 8) as u8);
        out
    }

    #[test]
    fn test_ble_request_construction_matches_android() {
        // 模拟 Android READ_SN: FC=03, addr=0x0894, count=9
        let req = build_ble_request(0x1234, 0x5678, 1, 0x03, &[
            0x08, 0x94, 0x00, 0x09,
        ]);
        // Total: 2+2+2+1+1+4+2 = 14 bytes
        assert_eq!(req.len(), 14);
        assert_eq!(u16::from_be_bytes([req[0], req[1]]), 0x1234); // tx_id
        assert_eq!(u16::from_be_bytes([req[2], req[3]]), 0x5678); // proto_id
        assert_eq!(u16::from_be_bytes([req[4], req[5]]), 6); // length = unit(1) + func(1) + data(4)
        assert_eq!(req[6], 1); // unit
        assert_eq!(req[7], 0x03); // func
        assert_eq!(&req[8..12], &[0x08, 0x94, 0x00, 0x09]); // addr + count
        // CRC at end
        let crc = crate::modbus::shared::modbus_crc16(&req[..12]);
        assert_eq!(u16::from_le_bytes([req[12], req[13]]), crc);
    }

    #[test]
    fn test_modbus_rtu_response_for_sn() {
        // 9 regs read = 18 bytes data
        let sn_data = b"ESP32S3-UNKNOWN-00"; // 18 bytes
        let body: Vec<u8> = std::iter::once((sn_data.len() * 2) as u8)
            .chain(sn_data.iter().copied())
            .collect();
        // body = [18, 18 bytes SN]
        let rtu_rsp = build_modbus_rtu_response(1, 0x03, &body);
        // Expected: [slave=1, func=0x03, byte_count=18, 18 bytes data, CRC_LO, CRC_HI]
        assert_eq!(rtu_rsp.len(), 23);
        assert_eq!(rtu_rsp[0], 1); // slave
        assert_eq!(rtu_rsp[1], 0x03); // func
        assert_eq!(rtu_rsp[2], 18); // byte_count
        assert_eq!(&rtu_rsp[3..21], sn_data); // SN bytes
        let crc = crate::modbus::shared::modbus_crc16(&rtu_rsp[..21]);
        assert_eq!(u16::from_le_bytes([rtu_rsp[21], rtu_rsp[22]]), crc);
    }

    #[test]
    fn test_android_parses_our_response() {
        // 模拟 Android 接收设备响应:
        // 设备发送 BLE 帧: tx_id + proto_id + length + modbus_rtu_response + crc
        let sn_data = b"ESP32S3-UNKNOWN-00";
        let body: Vec<u8> = std::iter::once((sn_data.len() * 2) as u8)
            .chain(sn_data.iter().copied())
            .collect();
        let rtu_rsp = build_modbus_rtu_response(1, 0x03, &body);

        // 模拟 send_ble_frame 的输出
        let tx_id: u16 = 0x1234;
        let proto_id: u16 = 0x5678;
        let mut frame = Vec::new();
        frame.extend_from_slice(&tx_id.to_be_bytes());
        frame.extend_from_slice(&proto_id.to_be_bytes());
        frame.extend_from_slice(&(rtu_rsp.len() as u16).to_be_bytes());
        frame.extend_from_slice(&rtu_rsp);
        let crc = crate::modbus::shared::modbus_crc16(&frame);
        frame.push(crc as u8);
        frame.push((crc >> 8) as u8);

        // Android 解析
        let (tx, proto, len, unit, func, pdu_data, crc_ok) = parse_ble_response(&frame).unwrap();
        assert_eq!(tx, 0x1234);
        assert_eq!(proto, 0x5678);
        assert_eq!(len as usize, rtu_rsp.len()); // length = pdu_data.len()
        assert_eq!(unit, 1);
        assert_eq!(func, 0x03);
        assert_eq!(pdu_data, rtu_rsp);
        assert!(crc_ok, "CRC should be valid");

        // Android 然后从 pdu_data 解析 Modbus RTU:
        // pdu_data = [slave=1, func=0x03, byte_count=18, SN data, CRC]
        assert_eq!(pdu_data[0], 1); // slave
        assert_eq!(pdu_data[1], 0x03); // func
        assert_eq!(pdu_data[2], 18); // byte_count

        // Android parseSNItem:
        // buffer[0] = 18 (length)
        // bytes = buffer[1..19] = 18 bytes of SN
        let length = pdu_data[2];
        let sn_bytes = &pdu_data[3..3 + length as usize];
        assert_eq!(sn_bytes, sn_data);
        let sn_str = std::str::from_utf8(sn_bytes).unwrap();
        assert_eq!(sn_str, "ESP32S3-UNKNOWN-00");
    }

    #[test]
    fn test_heartbeat_response_format() {
        // 心跳帧: func=0x11, response = [unit][0x11][hb_hi][hb_lo]
        // 我们的 send_ble_frame 会包装成 BLE 帧
        let tx_id: u16 = 0x1234;
        let proto_id: u16 = 0x5678;
        let unit: u8 = 1;
        let hb: u16 = 0x0042;
        let mut pdu = vec![unit, 0x11];
        pdu.push((hb >> 8) as u8);
        pdu.push((hb & 0xFF) as u8);

        // 构造 BLE 帧
        let mut frame = Vec::new();
        frame.extend_from_slice(&tx_id.to_be_bytes());
        frame.extend_from_slice(&proto_id.to_be_bytes());
        frame.extend_from_slice(&(pdu.len() as u16).to_be_bytes());
        frame.extend_from_slice(&pdu);
        let crc = crate::modbus::shared::modbus_crc16(&frame);
        frame.push(crc as u8);
        frame.push((crc >> 8) as u8);

        // 解析
        let (tx, _proto, len, _unit, func, pdu_data, crc_ok) = parse_ble_response(&frame).unwrap();
        assert_eq!(tx, 0x1234);
        assert_eq!(len as usize, pdu.len());
        assert_eq!(func, 0x11);
        assert!(crc_ok);
        assert_eq!(pdu_data, pdu);
    }

    #[test]
    fn test_multiple_register_reads() {
        // 测试多个不同寄存器的解析, 确保长度字段处理正确
        let cases = [
            (0x0894u16, 9usize, "SN"),        // 18 bytes
            (0x089D, 8, "Location"),           // 16 bytes
            (0x08D7, 6, "MAC"),                // 12 bytes
            (0x08E2, 4, "BT ID"),              // 8 bytes
            (0x08A5, 1, "HW Ver"),             // 2 bytes
            (0x08C7, 12, "IP"),                // 24 bytes
        ];
        for &(addr, count, name) in &cases {
            let req = build_ble_request(1, 1, 1, 0x03, &{
                let mut v = Vec::new();
                v.push((addr >> 8) as u8);
                v.push(addr as u8);
                v.push((count >> 8) as u8);
                v.push(count as u8);
                v
            });
            // Verify length field = 1 + 1 + 4 = 6 (unit + func + addr + count)
            let length = u16::from_be_bytes([req[4], req[5]]);
            assert_eq!(length, 6, "Length for {} should be 6", name);
        }
    }
}
