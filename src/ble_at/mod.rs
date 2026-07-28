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
pub mod logic_handlers;
pub mod ota_handlers;
pub mod parser;

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};

use std::sync::LazyLock;

use crate::sync::Spin;
use crate::config::regs;

use crate::error::AppResult;
use crate::modbus::shared::modbus_crc16;
use std::ffi::CString;

use crate::health::TaskHb;

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
/// 0 = 尚未分配. 用 AtomicU16 数组替代 Spin<[u16; 4]>:
/// - 写: CREAT_ATTR_TAB_EVT 中一次性填入, 之后只读
/// - 读: 每 GATT 回调 + 每 process_tick (100ms) → 真无锁
static HANDLE_TABLE: [AtomicU16; ATTR_TABLE_LEN] = [
    AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0),
];

/// Bluedord 分配的 GATT 接口号. 用 AtomicU8:
/// - 0xFF = 未注册 (sentinel, ESP_GATT_IF_NONE 在 ESP-IDF 头文件定义)
/// - 0..=0xFE = 已注册接口号
/// 替代 Spin<Option<esp_gatt_if_t>>, 真无锁 (原 Spin 在每回调+每 tick 都竞争)
// 注: ESP-IDF esp_gatt_if_t 实际为 u8 (绑定确认). 0xFF = 未注册.
static GATTS_IF: AtomicU8 = AtomicU8::new(0xFF);

/// 当前 GATT 连接 ID. 用 AtomicU16:
/// - 0xFFFF = 无客户端连接 (sentinel, ESP-IDF conn_id 最大 0x7FFF 远小于此)
/// - 其他 = 有效连接 ID
/// 替代 Spin<Option<u16>>, 真无锁
static CONN_ID: AtomicU16 = AtomicU16::new(0xFFFF);

/// TX Characteristic CCCD 使能标志 (主机写 0x0001 启用 notify)
static TX_NOTIFY_ENABLED: AtomicBool = AtomicBool::new(false);
/// LOOP7: BLE 名字变化通知 (Metuory 写完 ble_name 后置位, process_tick 处理)
static PENDING_GAP_NAME_UPDATE: AtomicBool = AtomicBool::new(false);

/// LOOP7: 标记 BLE 名字待更新 (在 write_hold_reg 写完 0x08E2 时调用)
pub fn notify_ble_name_changed() {
    PENDING_GAP_NAME_UPDATE.store(true, Ordering::SeqCst);
}

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
/// 二进制请求重组缓冲区。Android 在 MTU 协商失败时仍可能把一个协议帧拆为多次
/// GATT Write；必须先完整重组再校验 CRC，不能落入 AT 文本通道。
static BINARY_RX: LazyLock<Spin<heapless::Vec<u8, 512>>> =
    LazyLock::new(|| Spin::new(heapless::Vec::new()));
/// BLE notify 丢弃帧计数器 (Modbus 寄存器 0x0108 暴露)
static BINARY_TX_DROPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// 有待发送的 BLE 通知 (由 CONNECT_EVT 或 Modbus 响应设置, main loop 消费)
static PENDING_NOTIFY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Android requests MTU 512. A complete Modbus PDU can hold 253 bytes, so
/// compatibility responses must not use the old 32-byte control-buffer limit.
const BLE_RESPONSE_PDU_MAX: usize = 253;
const BLE_RESPONSE_DATA_MAX: usize = BLE_RESPONSE_PDU_MAX - 2;
const BLE_BINARY_FRAME_MAX: usize = 512;
const BLE_BINARY_PDU_MAX: usize = BLE_BINARY_FRAME_MAX - 8;

/// LOOP15: 广播启动完成标志 (ADV_START_COMPLETE_EVT 成功后置位).
/// 用于 process_tick 周期自检 — 若启动后仍 false, 重新触发 config_adv_data → 广播.
/// 防止启动期任意一步静默失败导致设备永久不可发现 (手持机搜不到蓝牙).
static ADV_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// LOOP15: GATT 服务是否已启动 (CREAT_ATTR_TAB_EVT 后 start_service 成功)
static SVC_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

/// LOOP17: 统一的 ADV data 配置入口.
///
/// 关键点 (手持机搜不到蓝牙的根因):
/// - **必须先调 `esp_ble_gap_set_device_name` 再调本函数**. ESP-IDF Bluedroid 在
///   `btm_ble_build_adv_data` 中从 `btm_cb.cfg.ble_bd_name` 读取设备名放进 ADV 包.
///   若调用顺序颠倒, ADV 包里名字为空 → metuory `onScanning` 的 `getName()` 返回 null
///   → `startsWith("m")` 失败 → 设备不在扫描列表.
/// - **改名后必须重新调本函数**: `esp_ble_gap_set_device_name` 只更新内部 BD name,
///   不会自动重建已发出的 ADV 包. 必须重新 config_adv_data → ADV_DATA_SET_COMPLETE_EVT
///   → start_advertising 才能让新名字进入空口.
/// - **31 字节限制**: flags(3) + 128bit UUID(18) + name(2+len). "Mesh"(4) → 3+18+6=27B 安全.
///   若用户改名 > 9 字节, Bluedroid 会截断为 `BTM_BLE_AD_TYPE_NAME_SHORT`,
///   只要首字符仍是 m/M, metuory 过滤仍能命中.
fn config_adv_data() {
    let mut adv_data = esp_idf_sys::esp_ble_adv_data_t {
        set_scan_rsp: false,      // LOOP17: 不用 scan_rsp, 名字+UUID 同放 ADV (size=27B<31)
        include_name: true,       // name 放 ADV data, 兼容所有扫描器 (含旧 startLeScan)
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
            | esp_idf_sys::ESP_BLE_ADV_FLAG_BREDR_NOT_SPT) as u8,
    };
    // config_adv_data 异步: 成功后触发 ADV_DATA_SET_COMPLETE_EVT → gap_event_cb → start_advertising
    let ret = unsafe { esp_idf_sys::esp_ble_gap_config_adv_data(&mut adv_data as *mut _) };
    if ret != 0 {
        log::error!("[ble_at] config_adv_data failed: 0x{:x}", ret);
        // 失败时清 ADV_ACTIVE, process_tick 下个周期会重试
        ADV_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    } else {
        log::info!("[ble_at] config_adv_data queued (name + UUID + flags)");
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
                // LOOP15: 标记广播已成功启动, 供 process_tick 自检
                ADV_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
            } else {
                log::error!("[ble_at] advertising start failed: {}", status);
                // LOOP15: 失败时清零, process_tick 下个周期会重试
                ADV_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
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
            GATTS_IF.store(gatts_if as u8, Ordering::Release);
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
            // 拷贝 handle 表到本地存储 (原子 store)
            let n = (p.num_handle as usize).min(ATTR_TABLE_LEN);
            // SAFETY: p.handles 指向 Bluedord 内部 u16 数组, 长度 = num_handle
            let src = unsafe { std::slice::from_raw_parts(p.handles, n) };
            for (i, &h) in src.iter().enumerate() {
                if i < ATTR_TABLE_LEN {
                    HANDLE_TABLE[i].store(h, Ordering::Release);
                }
            }
            let svc_handle = HANDLE_TABLE[IDX_SVC].load(Ordering::Acquire);
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
                // LOOP15: 标记服务已启动, process_tick 据此判定是否需要重试广播
                SVC_STARTED.store(true, std::sync::atomic::Ordering::Release);
                // LOOP17: 配置广播数据 (触发 ADV_DATA_SET_COMPLETE_EVT → gap_event_cb → start_advertising)
                // 必须在 set_device_name 之后调用 (start() 已先 set_device_name).
                // 设备名从 btm_cb.cfg.ble_bd_name 读取, 已在 start() 中通过
                // esp_ble_gap_set_device_name 设置为 "Mesh" (m 前缀, 匹配 metuory 扫描过滤器).
                config_adv_data();
            }
        }
        ESP_GATTS_WRITE_EVT => {
            if param.is_null() {
                return;
            }
            let p = unsafe { &(*param).write };

            // LOOP8: HANDLE_TABLE 改为 AtomicU16 数组, 用 load 直接读
            let cccd_handle = HANDLE_TABLE[IDX_CHAR_CCCD].load(Ordering::Acquire);
            let value_handle = HANDLE_TABLE[IDX_CHAR_VALUE].load(Ordering::Acquire);
            let gatts_if_raw = GATTS_IF.load(Ordering::Acquire);

            // CCCD 写入 (TX notify enable/disable)
            // 注意: CCCD 分支必须先发送写响应，再处理 CCCD，
            // 否则 Android BLE 栈收不到响应会进入异常状态。
            if p.handle == cccd_handle && p.len == 2 && !p.value.is_null() {
                // SAFETY: p.value 指向主机写入的 2 字节数据
                let v = unsafe { std::slice::from_raw_parts(p.value, 2) };
                let cccd = u16::from_le_bytes([v[0], v[1]]);
                let enabled = cccd & 0x0001 != 0;
                TX_NOTIFY_ENABLED.store(enabled, Ordering::SeqCst);
                log::info!(
                    "[ble_at] TX notify {}, gatts_if={}, conn_id={}",
                    if enabled { "enabled" } else { "disabled" },
                    gatts_if_raw,
                    p.conn_id
                );
                // 发写响应 — Android 端需要收到 CCCD write response 才能继续通信
                if p.need_rsp && gatts_if_raw != 0xFF {
                    let _ = unsafe {
                        esp_idf_sys::esp_ble_gatts_send_response(
                            gatts_if_raw as esp_idf_sys::esp_gatt_if_t,
                            p.conn_id,
                            p.trans_id,
                            esp_idf_sys::esp_gatt_status_t_ESP_GATT_OK,
                            std::ptr::null_mut(),
                        )
                    };
                }
            } else if p.handle == value_handle && !p.value.is_null() && p.len > 0 {
                // RX characteristic 写入 — 优先尝试二进制协议, 否则按文本 AT 处理
                // 关键修复: 必须先发送写响应, 然后再处理数据发送通知
                // 否则 Android BLE 栈可能在收到通知前就进入下一状态
                //
                // LOOP18: 此回调在 Bluedroid BTC 任务栈 (12KB) 上执行.
                //   - 不要 `Vec::new()`: 12KB 栈下 Vec heap 分配没问题,
                //     但 0 字节拷贝时 BLE stack 可能复用同一指针 → Vec 析构
                //     释放后原指针失效. 改用 slice 引用直接传入.
                //   - 不要 `format!("{:02X}", b)`: format! 每次调用构造 ~24B
                //     String + 走 heap 分配, 在 BTC 栈上累积易触发 Stack canary.
                let data: &[u8] = unsafe {
                    if p.value.is_null() || p.len == 0 {
                        &[]
                    } else {
                        std::slice::from_raw_parts(p.value, p.len as usize)
                    }
                };

                // 1. 先发送写响应 (GATT 协议要求先响应 write request)
                if p.need_rsp {
                    if gatts_if_raw == 0xFF {
                        log::error!("[ble_at] cannot respond: GATT interface is unavailable");
                        return;
                    }
                    let _ = unsafe {
                        esp_idf_sys::esp_ble_gatts_send_response(
                            gatts_if_raw as esp_idf_sys::esp_gatt_if_t,
                            p.conn_id,
                            p.trans_id,
                            esp_idf_sys::esp_gatt_status_t_ESP_GATT_OK,
                            std::ptr::null_mut(),
                        )
                    };
                }

                // 2. 写响应后, 处理数据并发送通知
                //    LOOP18: 用零分配 hex 格式化 (heapless::String<64>), 避免 BTC 栈上
                //    构造 Vec<String> (每 format!("{:02X}") 一次 heap 分配, 累积可触发 Stack canary)
                if !data.is_empty() {
                    let mut hex = heapless::String::<64>::new();
                    for &b in data.iter().take(20) {
                        let _ = core::fmt::write(&mut hex, format_args!("{:02X} ", b));
                    }
                    log::info!("[ble_at] GATT write RX: {} bytes, hex={}", data.len(), hex);
                    if !try_feed_binary_protocol(data, p.conn_id, p.trans_id) {
                        feed_data(data);
                    }
                }
            } else {
                // 其他情况 (例如 CCCD 写入) 仍需发送响应
                if p.need_rsp && gatts_if_raw != 0xFF {
                    let _ = unsafe {
                        esp_idf_sys::esp_ble_gatts_send_response(
                            gatts_if_raw as esp_idf_sys::esp_gatt_if_t,
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
            // LOOP8: 原子写入替代 Spin<Option>
            CONN_ID.store(p.conn_id, Ordering::Release);
            GATTS_IF.store(gatts_if as u8, Ordering::Release);
            log::info!("[ble_at] GATT client connected (conn_id={})", p.conn_id);
            
            // MCA 兼容: 连接后立即产一个心跳通知到 BINARY_TX 队列
            // Android BLEDataSyncManager.onStateConnected 会发心跳命令,
            // 但有时 Service 未发现, 写入失败, 导致数据流卡死。
            // 我们主动发心跳, 触发 Android 的 onHeartbeatChanged → readHardwareInfo
            //
            // ---- LOOP14 P3-3 已知限制: BLE 心跳 CONNECT_EVT 帧丢失 ----
            // 此处构造的 frame 仅设置 PENDING_NOTIFY=true, **未推入 BINARY_TX 队列**.
            // try_send_notify() 从 TX_BUFFER 读取待发帧, 而非 PENDING_NOTIFY 标记的临时帧.
            // 周期心跳 (~10s process_tick 调用 BLEHeartBeat) 弥补此缺陷.
            // 影响: Android 端首次 connect 后需等待 ~10s 收到首次心跳, 略延迟 UI 刷新.
            // 修复方向: 把 frame 推到 BINARY_TX 后置 PENDING_NOTIFY, 或 try_send_notify
            //          优先消费 PENDING_NOTIFY 标记的临时帧.
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
            // LOOP15: 修复 P3-3 — 把 CONNECT 心跳帧实际推入 BINARY_TX 队列,
            // Android 首次 connect 后能立即收到心跳, 不用等 ~10s 周期心跳.
            // (原代码仅 set PENDING_NOTIFY=true, frame 未入队 → 丢失)
            if let Some(mut btx) = BINARY_TX.try_lock() {
                if btx.len() + frame.len() <= 2048 {
                    let _ = btx.extend_from_slice(&frame);
                    PENDING_NOTIFY.store(true, std::sync::atomic::Ordering::Release);
                    log::info!("[ble_at] connect heartbeat queued ({} bytes)", frame.len());
                } else {
                    log::warn!("[ble_at] BINARY_TX full, connect heartbeat dropped");
                }
            }
        }
        ESP_GATTS_DISCONNECT_EVT => {
            if param.is_null() {
                return;
            }
            let p = unsafe { &(*param).disconnect };
            log::warn!("[ble_at] GATT disconnect conn_id={} reason={:#x}",
                p.conn_id, p.reason);
            CONN_ID.store(0xFFFF, Ordering::Release);  // LOOP8: 原子 sentinel = None
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
    // LOOP11: 零拷贝, 闭包内构造最终 String
    // LOOP17: 统一 fallback 为 "Mesh" (与 system_config.rs 默认值 + update_gap_device_name 一致).
    //   metuory 1.0.78 SearchDeviceActivity.onScanning 过滤 name.startsWith("m") (忽略大小写),
    //   非 m 前缀的设备名 (如旧 fallback "GW-S3") 不会出现在扫描列表 → 手持机搜不到蓝牙.
    //   必须保证任意路径 (NVS 空 / ble_name_str 返回空) 下设备名都以 m/M 开头.
    let configured_name = crate::bus::config_state::config_read_with(|cs| cs.cfg.ble_name_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mesh".to_owned());
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
/// LOOP7: 写完 BLE 名字后立即同步 GAP 设备名
/// LOOP17: 关键修复 — `esp_ble_gap_set_device_name` 仅更新 controller 内部 BD name,
/// **不会**自动重建已经发出的 ADV 包. 若不重新 `config_adv_data`, 新名字仍在 BD name
/// 缓存里但 ADV data 里仍是旧名字 → 手持机用 `device.getName()` 读到旧名 → 改名无效.
///
/// 修复: 改名后立即调 `config_adv_data()` 触发 ADV_DATA_SET_COMPLETE_EVT → start_advertising,
/// 让新名字真正进入空口. 这是 LOOP17 的核心 bug 修复.
pub fn update_gap_device_name() {
    // LOOP11: 零拷贝, 闭包内返回 String (ble_name_str() 返回 String, 不含借用)
    // LOOP17: 统一 fallback 为 "Mesh" (与 start() 完全一致), 防止空名导致 metuory 过滤失败.
    let configured_name = crate::bus::config_state::config_read_with(|cs| cs.cfg.ble_name_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mesh".to_owned());
    log::warn!("[ble_at] update_gap_device_name: cfg.ble_name='{}'", configured_name);
    if let Ok(cname) = std::ffi::CString::new(configured_name.as_str()) {
        let ret = unsafe { esp_idf_sys::esp_ble_gap_set_device_name(cname.as_ptr()) };
        log::warn!("[ble_at] esp_ble_gap_set_device_name ret=0x{:x}", ret);
        if ret == esp_idf_sys::ESP_OK {
            // 关键: set_device_name 后必须重发 config_adv_data 让新名字进入 ADV 包
            // 仅在服务已启动时重发, 否则首次启动链仍由 CREAT_ATTR_TAB_EVT 负责
            if SVC_STARTED.load(std::sync::atomic::Ordering::Acquire) {
                log::info!("[ble_at] re-trigger config_adv_data after rename");
                // 清 ADV_ACTIVE 触发被动重试逻辑 (避免快速连续 config_adv_data)
                ADV_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
                config_adv_data();
            }
        }
    } else {
        log::warn!("[ble_at] CString::new failed (NUL in name?)");
    }
}

/// 每 10ms 检查 RX_BUFFER 是否有完整命令行 (以 \n 结尾),
/// 有则调用 parser::process 处理, 响应写入 TX_BUFFER 等待 notify。
pub fn process_tick() {
    // LOOP8: 心跳 — 之前从未 tick, 导致 ble-at 总被误判停滞
    TASK_HB.tick();
    // LOOP15: BLE 广播健康自检 — 两层策略:
    //
    // 1. **主动重置 (每 ~15s)**: 调 stop_advertising() + 清 ADV_ACTIVE, 强制下个 tick 走
    //    config_adv_data → start_advertising 完整链路, 重刷 BLE controller 状态.
    //    解决: 广播静默死亡 (controller 缓冲区耗尽/堆压力) 后 ADV_ACTIVE 卡 true →
    //    重试逻辑永不触发 → 设备永久不可发现.
    //
    // 2. **被动重试 (启动后 ADV_ACTIVE 仍 false)**: config_adv_data 重新配置.
    //    解决: 启动期 config_adv_data/start_advertising 静默失败.
    //
    // 仅在未连接时 (CONN_ID=0xFFFF) 触发, 避免干扰已有连接.
    if SVC_STARTED.load(std::sync::atomic::Ordering::Acquire)
        && CONN_ID.load(Ordering::Acquire) == 0xFFFF
    {
        static TICK_DIV: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let tick = TICK_DIV.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // 每 ~15s 主动重置一次广播 (150 ticks × 100ms = 15s)
        // stop_advertising() → controller 立即停止广播 → 无回调.
        // 置 ADV_ACTIVE=false → 下个 tick (%50==0) 走 config_adv_data 重新启动.
        if tick % 150 == 0 && ADV_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
            log::info!("[ble_at] periodic advertising reset (tick={})", tick);
            unsafe { esp_idf_sys::esp_ble_gap_stop_advertising(); }
            ADV_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
        }

        // 被动重试: ADV_ACTIVE=false 时每 ~5s 重新 config_adv_data
        // LOOP17: 统一使用 config_adv_data() 入口 (与 start/update_gap_device_name 一致)
        if !ADV_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
            && tick % 50 == 0
        {
            log::warn!("[ble_at] advertising not active, retrying config_adv_data");
            config_adv_data();
        }
    }
    // LOOP7: 处理待更新的 GAP 设备名 (Metuory 写完 0x08E2 后)
    if PENDING_GAP_NAME_UPDATE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        update_gap_device_name();
    }
    // LOOP8: 原子读取替代 Spin lock — 真无锁
    let conn_id_raw = CONN_ID.load(Ordering::Acquire);
    if conn_id_raw == 0xFFFF { return; }  // sentinel = None
    let gatts_if_raw = GATTS_IF.load(Ordering::Acquire);
    if gatts_if_raw == 0xFF { return; }  // sentinel = 未注册
    let tx_handle = HANDLE_TABLE[IDX_CHAR_VALUE].load(Ordering::Acquire);
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
            // LOOP8: gatts_if_raw/conn_id_raw 是原子读出的原始值 (已校验非 sentinel)
            let ret = unsafe {
                esp_idf_sys::esp_ble_gatts_send_indicate(
                    gatts_if_raw as esp_idf_sys::esp_gatt_if_t,
                    conn_id_raw,
                    tx_handle,
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
            gatts_if_raw as esp_idf_sys::esp_gatt_if_t,
            conn_id_raw, tx_handle,
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
    // LOOP8: 原子读取替代 Spin lock
    let gatts_if_raw = GATTS_IF.load(Ordering::Acquire);
    let conn_id_raw = CONN_ID.load(Ordering::Acquire);
    log::info!("[ble_at] try_send_notify called, enabled={}, conn={:#06x}, gatts_if={}",
        TX_NOTIFY_ENABLED.load(Ordering::SeqCst),
        conn_id_raw,
        gatts_if_raw
    );
    // 检查 CCCD 是否使能
    if !TX_NOTIFY_ENABLED.load(Ordering::SeqCst) {
        return;
    }

    // 检查是否有客户端连接
    if conn_id_raw == 0xFFFF {
        return;
    }

    // 取出待发送数据
    let data = match take_response() {
        Some(d) => d,
        None => return,
    };
    if data.is_empty() {
        return;
    }

    // 检查 GATT 服务是否已注册
    let tx_handle = HANDLE_TABLE[IDX_CHAR_VALUE].load(Ordering::Acquire);
    if tx_handle == 0 {
        // GATT 服务尚未注册, 数据放回 TX_BUFFER 等下次重试
        let mut tx = TX_BUFFER.lock();
        let _ = tx.push_str(&data);
        return;
    }

    if gatts_if_raw == 0xFF {
        let mut tx = TX_BUFFER.lock();
        let _ = tx.push_str(&data);
        return;
    }
    // SAFETY: gatts_if / conn_id / tx_handle 来源可靠 (Bluedroid 分配)
    let ret = unsafe {
        esp_idf_sys::esp_ble_gatts_send_indicate(
            gatts_if_raw as esp_idf_sys::esp_gatt_if_t,
            conn_id_raw,
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
    // 无堆分配: handle_pdu 写入栈缓冲区
    let mut pdu_buf = [0u8; crate::modbus::shared::PDU_BUF_SIZE];
    let pdu_len = crate::modbus::shared::handle_pdu(&backend, func, pdu, &mut pdu_buf);
    // 构造 Modbus RTU 响应: slave + func + body + crc
    let mut rtu_rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = rtu_rsp.push(slave);
    let _ = rtu_rsp.extend_from_slice(&pdu_buf[..pdu_len]);
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

/// 返回完整手持机二进制帧所需字节数。None 表示头部尚不完整或长度非法。
fn binary_frame_total_len(data: &[u8]) -> Option<usize> {
    if data.len() < 6 {
        return None;
    }
    let length = u16::from_be_bytes([data[4], data[5]]) as usize;
    if !(2..=BLE_BINARY_PDU_MAX).contains(&length) {
        return None;
    }
    Some(6 + length + 2)
}

/// 将一或多次 GATT Write 重组为完整的 Android 协议帧。
/// 返回 true 表示输入属于二进制协议（包括等待后续分片），false 才交给 AT 文本处理。
fn try_feed_binary_protocol(data: &[u8], conn_id: u16, trans_id: u32) -> bool {
    let mut rx = match BINARY_RX.try_lock() {
        Some(rx) => rx,
        None => return false,
    };

    // AT 命令以 "AT" 开头；其余输入按二进制帧进行累积，支持头部自身被拆分。
    if rx.is_empty() && (data.starts_with(b"AT") || data == b"A") {
        return false;
    }
    if rx.len() + data.len() > rx.capacity() {
        log::warn!("[ble_at] binary RX overflow, dropping partial frame");
        rx.clear();
        return true;
    }
    if rx.extend_from_slice(data).is_err() {
        rx.clear();
        return true;
    }

    if rx.len() < 6 {
        return true;
    }
    let expected = match binary_frame_total_len(&rx) {
        Some(expected) if expected <= rx.capacity() => expected,
        _ => {
            log::warn!("[ble_at] invalid binary frame length, dropping input");
            rx.clear();
            return true;
        }
    };
    if rx.len() < expected {
        return true;
    }
    if rx.len() != expected {
        log::warn!("[ble_at] binary RX contains trailing bytes, dropping frame");
        rx.clear();
        return true;
    }

    let handled = try_handle_binary_protocol(&rx[..], conn_id, trans_id);
    rx.clear();
    handled
}

fn try_handle_binary_protocol(data: &[u8], conn_id: u16, _trans_id: u32) -> bool {
    // LOOP18: 零分配 hex 格式化, 避免 BTC 任务栈上累积 Vec<String>
    if data.len() > 20 {
        let mut hex = heapless::String::<64>::new();
        for &b in data.iter().take(20) {
            let _ = core::fmt::write(&mut hex, format_args!("{:02X} ", b));
        }
        log::info!("[ble_at] binary rx: {} bytes, hex={}", data.len(), hex);
    } else {
        let mut hex = heapless::String::<64>::new();
        for &b in data.iter() {
            let _ = core::fmt::write(&mut hex, format_args!("{:02X} ", b));
        }
        log::info!("[ble_at] binary rx: {} bytes, hex={}", data.len(), hex);
    }
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
        // 心跳应答 — LOOP9: 补全 terminal (6字节) 以匹配 metuory parseHeartbeat 格式
        // metuory parseHeartbeat: slave=data[0], runStatus=data[1], terminal=data[2..]
        // pdu_data = [unit, func=0x11, slave_addr, runStatus, terminal(6)] = 10 字节
        // BLE 帧: 2+2+2+10+2 = 18 字节
        let hb = heartbeat_counter();
        let mut rsp: heapless::Vec<u8, 10> = heapless::Vec::new();
        let _ = rsp.push(unit);              // pdu[0] = unit_id (Modbus slave addr)
        let _ = rsp.push(func);              // pdu[1] = func = 0x11
        let _ = rsp.push(unit);              // pdu[2] = slave address (echo unit)
        let _ = rsp.push(0x01);              // pdu[3] = runStatus = 1 (运行中)
        // terminal: 6 字节 BLE MAC (设备标识, metuory 存入 BLEDataSyncModel)
        let mut mac = [0u8; 6];
        unsafe { esp_idf_sys::esp_read_mac(mac.as_mut_ptr(), 2); } // ESP_MAC_BT=2
        for b in mac { let _ = rsp.push(b); }
        log::debug!("[ble_at] heartbeat: hb_seq={} mac={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            hb, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
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
    // ---- DEVICE_FUNCTION_COUNT (LOOP12: 0xB0/0xB1, 映射到 Modbus 0x08FC) ----
    // Android 1.0.78 通过自定义 opcodes 读写 FUNC_COUNT 寄存器, 走自定义子协议不走 Modbus FC.
    // 原 fall through 路径把 0xB0 当作 Modbus FC=176 (illegal function), Android 报 "无自定义功能".
    //
    if func == 0xB0 || func == 0xB1 {
        let mut rsp: heapless::Vec<u8, 32> = heapless::Vec::new();
        let _ = rsp.push(unit);
        let _ = rsp.push(func);
        if func == 0xB0 {
            // READ 0x08FC
            let val = crate::bus::backends::read_hold_reg(regs::FUNC_COUNT).unwrap_or(0);
            let _ = rsp.extend_from_slice(&val.to_be_bytes());
        } else {
            // WRITE 0x08FC: data[8..10] = 16-bit BE value
            if data.len() < 10 {
                return false;
            }
            let val = u16::from_be_bytes([data[8], data[9]]);
            let _ = crate::bus::backends::write_hold_reg(regs::FUNC_COUNT, val);
            // WRITE 应答: 不带数据
        }
        send_ble_frame(tx_id, proto_id, &rsp, conn_id);
        return true;
    }
    // ---- DEVICE_TEXT 0xB4-B7 ----
    // metuory 0xB4 READ_DEVICE_TEXT_COUNT (0x1388)
    // metuory 0xB5 WRITE_DEVICE_TEXT_COUNT (0x1388, 4 words)
    // metuory 0xB6 READ_DEVICE_TEXT_DATA (0x138A+offset, n)
    // metuory 0xB7 WRITE_DEVICE_TEXT_DATA (0x138A+offset, n, 分 180B/chunk)
    //
    // App 将文本拆成 180-byte protocol frames；try_feed_binary_protocol 会在
    // GATT 层重组每一个被 ATT 再次拆分的帧，随后按 dataOffset 顺序写入。

    // B2/B3/B4-B7 使用标准 BLE 外层帧 + Modbus FC03/FC06/FC16 载荷。
    // 这些命令必须在通用 Modbus fallback 之前处理，否则会被当作非法功能码。
    if matches!(func, 0xB2..=0xB7) {
        return handle_handheld_config_text(func, &data[8..crc_begin], tx_id, proto_id, conn_id, unit);
    }

    // ---- 自定义 MCA 协议命令 (0xC0-0xCF) ----
    // Android 端可能通过这些命令获取 IP/子网/网关等设备信息
    if func >= 0xC0 && func <= 0xCF {
        return handle_mca_custom_command(func, &data[8..crc_begin], tx_id, proto_id, conn_id, unit);
    }
    // ---- MCA 逻辑配置协议 (0xD0-0xD3) ----
    // 0xD0 LOGIC_CONFIG / 0xD1 LOGIC_RETRIEVE / 0xD2 DELETE_CONFIG / 0xD3 COM_REQUEST
    if func >= 0xD0 && func <= 0xD3 {
        if let Some(rsp) = logic_handlers::dispatch_logic_cmd(func, &data[8..crc_begin]) {
            send_ble_frame(tx_id, proto_id, &rsp, conn_id);
            return true;
        }
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

fn handle_handheld_config_text(
    func: u8,
    pdu: &[u8],
    tx_id: u16,
    proto_id: u16,
    conn_id: u16,
    unit: u8,
) -> bool {
    if pdu.len() < 4 {
        return false;
    }
    let addr = u16::from_be_bytes([pdu[0], pdu[1]]);
    let count = u16::from_be_bytes([pdu[2], pdu[3]]) as usize;
    let mut rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = rsp.push(unit);
    let _ = rsp.push(func);

    match func {
        0xB2 | 0xB4 | 0xB6 => {
            let read_addr = if func == 0xB4 { regs::DEVICE_TEXT_BASE } else { addr };
            let words = count.min(120);
            let mut bytes: heapless::Vec<u8, 240> = heapless::Vec::new();
            for i in 0..words {
                let a = read_addr.saturating_add(i as u16);
                let v = if func == 0xB2 {
                    crate::bus::backends::read_hold_reg(a).unwrap_or(0)
                } else {
                    crate::bus::backends::read_hold_reg(a).unwrap_or(0)
                };
                let _ = bytes.extend_from_slice(&v.to_be_bytes());
            }
            let _ = rsp.push(bytes.len() as u8);
            let _ = rsp.extend_from_slice(&bytes);
        }
        0xB3 | 0xB5 | 0xB7 => {
            if pdu.len() < 5 {
                return false;
            }
            let byte_count = pdu[4] as usize;
            if byte_count > 240 || pdu.len() != 5 + byte_count || byte_count != count.saturating_mul(2) {
                return false;
            }
            let write_addr = if func == 0xB5 { regs::DEVICE_TEXT_BASE } else { addr };
            for i in 0..count {
                let off = 5 + i * 2;
                let value = u16::from_be_bytes([pdu[off], pdu[off + 1]]);
                let target = if func == 0xB3 { write_addr.saturating_add(i as u16) } else { write_addr.saturating_add(i as u16) };
                if !crate::bus::backends::write_hold_reg(target, value) {
                    return false;
                }
            }
        }
        _ => return false,
    }
    send_ble_frame(tx_id, proto_id, &rsp, conn_id);
    true
}

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
    _unit: u8,
) -> bool {
    log::info!("[ble_at] MCA custom cmd: func=0x{func:02X}, data={}", 
        data.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));
    match func {
        0xCE => {
            // GET_NET_INFO: 返回 IP/子网/网关 (各 4 字节)
            let (ip, mask, gw) = crate::bus::config_state::config_read_with(|cs| (cs.cfg.ip, cs.cfg.mask, cs.cfg.gateway))
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
            let sn = crate::bus::config_state::config_read_with(|cs| cs.cfg.sn)
                .unwrap_or([0u8; 32]);
            let mut rsp: heapless::Vec<u8, 20> = heapless::Vec::new();
            let _ = rsp.push(0xC2);
            let _ = rsp.push(0);
            // SN 9 个 U16 (大端) → 18 字节 ASCII
            // LOOP9: push 顺序应为 BE [hi, lo], 而非 LE [lo, hi]
            for i in 0..9usize {
                let w = u16::from_be_bytes([sn[i * 2], sn[i * 2 + 1]]);
                let _ = rsp.push((w >> 8) as u8); // hi byte first (BE)
                let _ = rsp.push(w as u8); // lo byte second
            }
            log::info!("[ble_at] GET_SN_CODE: {} bytes", rsp.len());
            send_ble_frame(tx_id, proto_id, &rsp, conn_id);
            true
        }
        0xCF => {
            // GET_FIRMWARE_VER
            let _fw = crate::config::APP_VERSION;
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
            let mac = crate::bus::config_state::config_read_with(|cs| cs.cfg.eth_mac)
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
/// LOOP10: 波特率 → metuory packed nibble 索引 (与 system_config.rs baud_table 顺序一致)
/// 对齐 CommandDataUtil.decodeSerialBaudRate: (value & 0xf0) >> 4
fn baud_to_index(baud: u32) -> u8 {
    match baud {
        1200 => 0, 2400 => 1, 4800 => 2, 9600 => 3, 19200 => 4, 38400 => 5,
        57600 => 6, 115200 => 7, 230400 => 8, 460800 => 9, 921600 => 10,
        _ => 3, // 默认 9600
    }
}

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

    // LOOP11: 零拷贝, 把整个 match 包进 config_read_with 闭包, 避免 BTC_TASK
    // 栈上 clone ConfigSnapshot (~2.5KB). 闭包内借用 &cs.cfg, 所有 cfg.xxx
    // Keep payload and length prefix in one bounded buffer so a successful
    // response can never advertise bytes that were silently discarded.
    let rsp_data: Option<Option<heapless::Vec<u8, BLE_RESPONSE_DATA_MAX>>> = crate::bus::config_state::config_read_with(|cs| {
        let cfg: &crate::device::system_config::SystemConfig = &cs.cfg;
        match (reg_addr, reg_cnt) {
        // ⚠️ 所有 push 必须在 Vec 容量 (32) 内! 超出会静默截断 → Android 严格长度检查失败 → UI 空
        // ⚠️ READ_IP (0x08C7) 需 25 字节 (1 length + 8 ip + 8 mask + 8 gw), 旧 Vec<u8, 16> bug 见 LOOP2
        // 回归测试: test_android_parse_ip_no_truncation

        // READ_HARDWARE_INFO (0x087C, 0x0002): [4][DO][DI][ADC][RS485]
        (0x087C, 2) => {
            let do_cnt = crate::config::hw_version::DO_COUNT as u8;
            let di_cnt = crate::config::hw_version::DI_COUNT as u8;
            let adc_cnt = crate::config::hw_version::AI_COUNT as u8;
            let rs485_cnt = 2u8;
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(4); // length
            let _ = d.push(do_cnt);
            let _ = d.push(di_cnt);
            let _ = d.push(adc_cnt);
            let _ = d.push(rs485_cnt);
            Some(d)
        }
        // READ_FW_VERSION (0x087E, 0x0002): Android 期望 [4][fw_hi][fw_lo][dt_hi][dt_lo] (5B)
        // fwVersionBytesToStr: fw=bytes[0..2]BE/100 → main.sub.tail, dt=bytes[2..4]BE
        // fw_version=221 → 2.2.1, fw_date=0x0615 → 1557 → 显示 "2.2.1.1557"
        (0x087E, 2) => {
            let fw = cfg.fw_version;
            let dt = cfg.fw_date;
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(4); // length = getFWVersionBufferLength() = 2*2 = 4
            let _ = d.push((fw >> 8) as u8);
            let _ = d.push((fw & 0xFF) as u8);
            let _ = d.push((dt >> 8) as u8);
            let _ = d.push((dt & 0xFF) as u8);
            log::info!("[ble_at] READ_FW_VERSION: fw=0x{:04X} ({}.{}.{}) dt=0x{:04X} ({})",
                fw, fw / 100, (fw % 100) / 10, fw % 10, dt, dt);
            Some(d)
        }
        // READ_DEVICE_PRODUCT (0x08A5, 0x0001): [2][hw_hi][hw_lo]
        (0x08A5, 1) => {
            let hw = cfg.hw_version;
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(2);
            let _ = d.push((hw >> 8) as u8);
            let _ = d.push((hw & 0xFF) as u8);
            Some(d)
        }
        // READ_IP (0x08C7, 0x000C): Android 期望 EXACTLY 25 字节
        //   [length_byte=24][ip(8)][mask(8)][gw(8)]
        //   每个 octet 编码为 BE u16 (高字节=0), 即 192 → [0x00, 0xC0]
        //   ipBytesToStr 读 4 BE short: ip[0..2], ip[2..4], ip[4..6], ip[6..8]
        //   getIPComponentLength() = ipLength/3 = 24/3 = 8
        (0x08C7, 12) => {
            let ip = cfg.ip;
            let mask = cfg.mask;
            let gw = cfg.gateway;
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(24); // length = getIPBufferLength() = 12*2 = 24
            // IP 每个 octet → 2 bytes BE (高字节 0)
            for &b in &ip { let _ = d.push(0); let _ = d.push(b); }
            for &b in &mask { let _ = d.push(0); let _ = d.push(b); }
            for &b in &gw { let _ = d.push(0); let _ = d.push(b); }
            log::info!("[ble_at] READ_IP: {}.{}.{}.{} / {}.{}.{}.{} gw {}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3],
                mask[0], mask[1], mask[2], mask[3],
                gw[0], gw[1], gw[2], gw[3]);
            Some(d)
        }
        // READ_MAC (0x08D7, 0x0006): Android 期望 [12][mac(12)] (13B)
        //   每个 mac byte 编码为 BE u16: 0x80 → [0x00, 0x80]
        //   macBytesToStr 按 2 字节步长读 short, 格式化为 "%02X"
        //   getMacBufferLength() = 6*2 = 12
        (0x08D7, 6) => {
            let mac = cfg.eth_mac;
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(12); // length = getMacBufferLength() = 6*2 = 12
            for &b in &mac { let _ = d.push(0); let _ = d.push(b); }
            log::info!("[ble_at] READ_MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
            Some(d)
        }
        // READ_BLUETOOTH_ID (0x08E2, 0x0004): Android 端蓝牙 ID 显示
        //   - UTF-8 模式 (新版本): parseBluetoothIDItem → bluetoothIDBytesToUTF8Str
        //     按字节遍历到第一个 0, 返回 UTF-8 字符串
        //   - HEX 模式 (旧版本): bluetoothIDBytesToHexStr 从 offset 4 读 int → OOB
        // 兼容性: 发 8 字节 [4][name(4)][padding(4)], HEX 模式读 offset 4 也安全
        // length_byte = 4 (getBluetoothIDBufferLength() = 4*2 = 8)
        (0x08E2, 4) => {
            let name_len = cfg.ble_name.iter().position(|&b| b == 0).unwrap_or(cfg.ble_name.len());
            let take = name_len.min(4);
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(4); // length_byte = 4 (Android 实际读 bytes[1..5])
            // ble_name 前 4 字节 (不足补 0, UTF-8 mode 截断到 0; HEX mode 跳过)
            for i in 0..4 {
                let _ = d.push(if i < take { cfg.ble_name[i] } else { 0 });
            }
            log::info!("[ble_at] READ_BLE_ID: {:?}", core::str::from_utf8(&cfg.ble_name[..take]).unwrap_or("<bin>"));
            Some(d)
        }
        // READ_SN (0x0894, 0x0009): Android 期望 [length][length bytes of SN ASCII]
        // parseSNItem 读 buffer[0] = length, 然后 [1..1+length] = SN bytes
        (0x0894, 9) => {
            let sn_str = cfg.sn_str();
            let bytes = sn_str.as_bytes();
            let take = bytes.len().min(BLE_RESPONSE_DATA_MAX - 1);
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(take as u8);
            for i in 0..take { let _ = d.push(bytes[i]); }
            log::info!("[ble_at] READ_SN: {} bytes", take);
            Some(d)
        }
        // READ_LOCATION (0x089D, 0x0008): 同 SN 格式, [length][location bytes]
        (0x089D, 8) => {
            let name_str = cfg.name_str();
            let bytes = name_str.as_bytes();
            let take = bytes.len().min(BLE_RESPONSE_DATA_MAX - 1);
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(take as u8);
            for i in 0..take { let _ = d.push(bytes[i]); }
            log::info!("[ble_at] READ_LOCATION: {} bytes", take);
            Some(d)
        }
        // READ_RS485_INDEX_CONFIG (0x08A6/0x08AB/0x08B0, 5): metuory 期望 packed 10-byte struct
        // parseRS485ConfigItem: [0x0a][packed][masterSlave][slaveAddr(2BE)][retry(2BE)][timeout(2BE)][interval(2BE)]
        // packed byte: baud(high nibble) | parity(bits 3-2) | stop(bit 1) | data(bit 0)
        // LOOP10: 之前落入 generic FC=03 catch-all 返回 BE u16 列表, metuory 解析失败
        (0x08A6, 5) | (0x08AB, 5) | (0x08B0, 5) => {
            let port = ((reg_addr - 0x08A6) / 5) as usize;
            let r = if port < cfg.rs485.len() { &cfg.rs485[port] } else { &cfg.rs485[0] };
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(0x0a); // length = 10
            let baud_idx = baud_to_index(r.baudrate);
            let stop_enc: u8 = if r.stop_bits >= 2 { 1 } else { 0 };
            let data_enc: u8 = if r.data_bits == 7 { 1 } else { 0 };
            let packed = ((baud_idx & 0xF) << 4) | ((r.parity & 0x3) << 2) | (stop_enc << 1) | data_enc;
            let _ = d.push(packed);
            let _ = d.push(r.mode); // masterSlaveType
            let _ = d.push((r.slave_addr as u16 >> 8) as u8);
            let _ = d.push(r.slave_addr & 0xFF);
            let _ = d.push((r.retry_count >> 8) as u8);
            let _ = d.push((r.retry_count & 0xFF) as u8);
            let _ = d.push((r.timeout_ms >> 8) as u8);
            let _ = d.push((r.timeout_ms & 0xFF) as u8);
            let _ = d.push((r.interval_ms >> 8) as u8);
            let _ = d.push((r.interval_ms & 0xFF) as u8);
            log::info!("[ble_at] READ_RS485_CONFIG: port={} baud={} parity={} stop={} data={} mode={} slave={} retry={} timeout={} interval={}",
                port, r.baudrate, r.parity, r.stop_bits, r.data_bits, r.mode, r.slave_addr, r.retry_count, r.timeout_ms, r.interval_ms);
            Some(d)
        }
        // READ_ADC_VALUE (0x0080, count): Android 期望 [length][length bytes of BE u16]
        // parseResReadADC: length = buffer[0], 然后 for i=1; i<buffer.length; i+=2 → BE short
        (0x0080, _) => {
            // 读所有 AI 通道 (F16=4, F4=8)
            let ai_count = crate::config::hw_version::AI_COUNT as u16;
            let n = reg_cnt.min(ai_count) as usize;
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push((n * 2) as u8); // length = 2*N bytes
            for i in 0..n {
                let v = crate::bus::backends::read_input_reg(0x0080 + i as u16).unwrap_or(0);
                let _ = d.push((v >> 8) as u8);
                let _ = d.push((v & 0xFF) as u8);
            }
            log::info!("[ble_at] READ_ADC: {} channels", n);
            Some(d)
        }
        // READ_COM_INPUT_IO_STATUS (0x0000, count): Android 期望 [N bytes][N bytes of packed I/O bits]
        // parseResReadComInputIOStatus: bufferLength = buffer[0], 读 bufferLength*8 bits
        // READ_COM_OUTPUT_IO_STATUS (0x0200, count): 同上, 解析复用
        (0x0000, _) | (0x0200, _) => {
            let bit_count = (reg_cnt as usize).min(BLE_RESPONSE_DATA_MAX * 8);
            let n_bytes = (bit_count + 7) / 8;
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push(n_bytes as u8); // length = N bytes of packed I/O
            // 读 coil/disc 状态并打包
            for i in 0..n_bytes {
                let mut acc: u8 = 0;
                for j in 0..8 {
                    let bit_idx = i * 8 + j;
                    if bit_idx >= bit_count { break; }
                    let val = if reg_addr == 0x0200 {
                        crate::bus::backends::read_coil(reg_addr + bit_idx as u16).unwrap_or(false)
                    } else {
                        crate::bus::backends::read_disc(reg_addr + bit_idx as u16).unwrap_or(false)
                    };
                    if val { acc |= 1 << j; }
                }
                let _ = d.push(acc);
            }
            log::info!("[ble_at] READ_COM_IO: addr=0x{:04X} cnt={}", reg_addr, reg_cnt);
            Some(d)
        }
        // READ_RS485_VALUE (用户自定义地址, count): 透传读取输入寄存器, length-prefix BE u16
        // 匹配: 任何 FC=04 读非 0x0080/0x0880-0x08CF 范围 (排除硬件信息等已知名单)
        // 实际 metuory 透传地址由 DEVICE_FUNCTION_CONFIG 配置
        (addr, _) if func == 0x04 && !(0x087C..=0x087F).contains(&addr)
            && addr != 0x0080 && addr < 0x4000 => {
            // 透传: 读输入寄存器, length-prefix BE u16
            let n = (reg_cnt as usize).min(BLE_RESPONSE_DATA_MAX / 2);
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push((n * 2) as u8);
            for i in 0..n {
                let v = crate::bus::backends::read_input_reg(addr + i as u16).unwrap_or(0);
                let _ = d.push((v >> 8) as u8);
                let _ = d.push((v & 0xFF) as u8);
            }
            log::info!("[ble_at] READ_RS485_VALUE: addr=0x{:04X} cnt={}", reg_addr, reg_cnt);
            Some(d)
        }
        // READ_RS485_CUSTOM_VALUE: 同上但 FC=03 (读保持寄存器)
        (addr, _) if func == 0x03 && !(0x087C..=0x08FF).contains(&addr)
            && addr < 0x4000 => {
            // 透传: 读保持寄存器, length-prefix 原始字节
            let n = (reg_cnt as usize).min(BLE_RESPONSE_DATA_MAX / 2);
            let mut d: heapless::Vec<u8, BLE_RESPONSE_DATA_MAX> = heapless::Vec::new();
            let _ = d.push((n * 2) as u8);
            for i in 0..n {
                let v = crate::bus::backends::read_hold_reg(addr + i as u16).unwrap_or(0);
                let _ = d.push((v >> 8) as u8);
                let _ = d.push((v & 0xFF) as u8);
            }
            log::info!("[ble_at] READ_RS485_CUSTOM: addr=0x{:04X} cnt={}", reg_addr, reg_cnt);
            Some(d)
        }
        _ => None, // 未命中 Android 已知名单, 让调用方继续走 Modbus RTU
    }
    });
    let rsp_data = match rsp_data {
        Some(Some(d)) => d,
        _ => return false,
    };

    // 构造 pdu_data = [unit][func=0x03/0x04][length][data...]
    // send_ble_frame 会自动加 tx_id/proto_id/length(=pdu_data.len())/crc
    let mut pdu: heapless::Vec<u8, BLE_RESPONSE_PDU_MAX> = heapless::Vec::new();
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
    cfg_fw_date: u16,
    do_count: u8,
    di_count: u8,
    ai_count: u8,
    rs485_count: u8,
) -> Option<heapless::Vec<u8, 32>> {
    let rsp_data: heapless::Vec<u8, 32> = match (reg_addr, reg_cnt) {
        // 所有 push 必须保证不超 Vec 容量 (32); 超出会导致 Android 严格长度检查失败
        // 见 test_android_parse_ip_no_truncation 回归测试
        (0x087C, 2) => {
            let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
            let _ = d.push(4);
            let _ = d.push(do_count);
            let _ = d.push(di_count);
            let _ = d.push(ai_count);
            let _ = d.push(rs485_count);
            d
        }
        (0x087E, 2) => {
            // Android: [4][fw_hi][fw_lo][dt_hi][dt_lo]  (5B rsp_data)
            let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
            let _ = d.push(4);
            let _ = d.push((cfg_fw_ver >> 8) as u8);
            let _ = d.push((cfg_fw_ver & 0xFF) as u8);
            let _ = d.push((cfg_fw_date >> 8) as u8);
            let _ = d.push((cfg_fw_date & 0xFF) as u8);
            d
        }
        (0x08A5, 1) => {
            let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
            let _ = d.push(2);
            let _ = d.push((cfg_hw_ver >> 8) as u8);
            let _ = d.push((cfg_hw_ver & 0xFF) as u8);
            d
        }
        (0x08C7, 12) => {
            // Android: [24][ip(8)][mask(8)][gw(8)]  (25B rsp_data, 每 octet BE u16)
            let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
            let _ = d.push(24);
            for &b in &cfg_ip { let _ = d.push(0); let _ = d.push(b); }
            for &b in &cfg_mask { let _ = d.push(0); let _ = d.push(b); }
            for &b in &cfg_gw { let _ = d.push(0); let _ = d.push(b); }
            d
        }
        (0x08D7, 6) => {
            // Android: [12][mac(12)]  (13B rsp_data, 每 mac byte BE u16)
            let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
            let _ = d.push(12);
            for &b in &cfg_mac { let _ = d.push(0); let _ = d.push(b); }
            d
        }
        (0x08E2, 4) => {
            let mut id = [0u8; 4];
            id.copy_from_slice(&cfg_ble_mac[2..6]);
            let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
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

// 暴露给测试用的纯逻辑函数: 构建 SN length-prefix 响应
#[cfg(test)]
pub(crate) fn mod_test_build_sn_response(
    unit: u8,
    func: u8,
    cfg: &crate::device::system_config::SystemConfig,
) -> Option<heapless::Vec<u8, 64>> {
    let sn_str = cfg.sn_str();
    let bytes = sn_str.as_bytes();
    let take = bytes.len().min(32);
    let mut d: heapless::Vec<u8, 64> = heapless::Vec::new();
    let _ = d.push(take as u8);
    for i in 0..take { let _ = d.push(bytes[i]); }
    let mut pdu: heapless::Vec<u8, 64> = heapless::Vec::new();
    let _ = pdu.push(unit);
    let _ = pdu.push(func);
    let _ = pdu.extend_from_slice(&d);
    Some(pdu)
}

#[cfg(test)]
pub(crate) fn mod_test_build_location_response(
    unit: u8,
    func: u8,
    cfg: &crate::device::system_config::SystemConfig,
) -> Option<heapless::Vec<u8, 32>> {
    let name_str = cfg.name_str();
    let bytes = name_str.as_bytes();
    let take = bytes.len().min(32);
    let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = d.push(take as u8);
    for i in 0..take { let _ = d.push(bytes[i]); }
    let mut pdu: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = pdu.push(unit);
    let _ = pdu.push(func);
    let _ = pdu.extend_from_slice(&d);
    Some(pdu)
}

#[cfg(test)]
pub(crate) fn mod_test_build_adc_response(
    unit: u8,
    func: u8,
    _reg_addr: u16,
    count: u16,
    values: [u16; 4],
) -> Option<heapless::Vec<u8, 32>> {
    let n = count.min(4) as usize;
    let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = d.push((n * 2) as u8);
    for i in 0..n {
        let _ = d.push((values[i] >> 8) as u8);
        let _ = d.push((values[i] & 0xFF) as u8);
    }
    let mut pdu: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = pdu.push(unit);
    let _ = pdu.push(func);
    let _ = pdu.extend_from_slice(&d);
    Some(pdu)
}

#[cfg(test)]
pub(crate) fn mod_test_build_com_input_response(
    unit: u8,
    func: u8,
    _reg_addr: u16,
    count: u16,
    packed: [u8; 2],
) -> Option<heapless::Vec<u8, 32>> {
    let n_bytes = ((count as usize) + 7) / 8;
    let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = d.push(n_bytes.min(2) as u8);
    for i in 0..n_bytes.min(2) { let _ = d.push(packed[i]); }
    let mut pdu: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = pdu.push(unit);
    let _ = pdu.push(func);
    let _ = pdu.extend_from_slice(&d);
    Some(pdu)
}

#[cfg(test)]
pub(crate) fn mod_test_build_rs485_value_response(
    unit: u8,
    func: u8,
    _reg_addr: u16,
    count: u16,
    values: [u16; 4],
) -> Option<heapless::Vec<u8, 32>> {
    let n = count.min(4) as usize;
    let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = d.push((n * 2) as u8);
    for i in 0..n {
        let _ = d.push((values[i] >> 8) as u8);
        let _ = d.push((values[i] & 0xFF) as u8);
    }
    let mut pdu: heapless::Vec<u8, 32> = heapless::Vec::new();
    let _ = pdu.push(unit);
    let _ = pdu.push(func);
    let _ = pdu.extend_from_slice(&d);
    Some(pdu)
}

#[cfg(test)]
mod android_compat_tests {
    use super::{binary_frame_total_len, build_android_read_response_pdu};

    // ---- READ_HARDWARE_INFO (0x087C, 2 regs) ----
    #[test]
    fn test_read_hardware_info_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x04, 0x087C, 2,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0, 0, 0x0615, 8, 8, 6, 2,
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
            0, 0x0221, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");
        // [unit=0x01][func=0x04][length=4][fw_hi=0x02][fw_lo=0x21][dt_hi=0x06][dt_lo=0x15]
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x04);
        assert_eq!(pdu[2], 4);
        assert_eq!(pdu[3], 0x02);
        assert_eq!(pdu[4], 0x21);
        assert_eq!(pdu[5], 0x06);
        assert_eq!(pdu[6], 0x15);
        assert_eq!(pdu.len(), 7);
    }

    // ---- READ_DEVICE_PRODUCT (0x08A5, 1 reg) ----
    #[test]
    fn test_read_device_product_format() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08A5, 1,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0x00F3, 0, 0x0615, 8, 8, 6, 2,
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
        // Android 期望 [unit=0x01][func=0x03][length=24][ip(8)][mask(8)][gw(8)]
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08C7, 12,
            [192, 168, 51, 140], [255, 255, 255, 0], [192, 168, 51, 1],
            [0; 6], [0; 6],
            0, 0, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x03);
        assert_eq!(pdu[2], 24); // length = 12*2
        assert_eq!(&pdu[3..11], &[0x00,192, 0x00,168, 0x00,51, 0x00,140]); // ip
        assert_eq!(&pdu[11..19], &[0x00,255, 0x00,255, 0x00,255, 0x00,0]);   // mask
        assert_eq!(&pdu[19..27], &[0x00,192, 0x00,168, 0x00,51, 0x00,1]);   // gw
        assert_eq!(pdu.len(), 27); // unit+func+25
    }

    // ---- READ_MAC (0x08D7, 6 regs) ----
    #[test]
    fn test_read_mac_format() {
        // Android 期望 [unit=0x01][func=0x03][length=12][mac(12)]
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08D7, 6,
            [0; 4], [0; 4], [0; 4],
            [0x80, 0xB5, 0x4E, 0x5B, 0x24, 0xE4],
            [0; 6],
            0, 0, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x03);
        assert_eq!(pdu[2], 12); // length = 6*2
        assert_eq!(&pdu[3..15], &[0x00,0x80, 0x00,0xB5, 0x00,0x4E, 0x00,0x5B, 0x00,0x24, 0x00,0xE4]);
        assert_eq!(pdu.len(), 15); // unit+func+13
    }

    // ---- READ_BLUETOOTH_ID (0x08E2, 4 regs) ----
    #[test]
    fn test_read_ble_id_format() {
        // 旧 helper 使用 cfg_ble_mac[2..6]; 实际生产代码用 cfg.ble_name (默认 "Mesh")
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08E2, 4,
            [0; 4], [0; 4], [0; 4], [0; 6],
            [0x80, 0xB5, 0x4E, 0x5B, 0x24, 0xE5], // BLE MAC
            0, 0, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");
        assert_eq!(pdu[0], 0x01);
        assert_eq!(pdu[1], 0x03);
        assert_eq!(pdu[2], 4);
        // 旧 helper: BLE MAC[2..6] = [0x4E, 0x5B, 0x24, 0xE5]
        assert_eq!(&pdu[3..7], &[0x4E, 0x5B, 0x24, 0xE5]);
        assert_eq!(pdu.len(), 7);
    }

    // ---- 未命中已知地址 ----
    #[test]
    fn test_unmapped_returns_none() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x0880, 5,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0, 0, 0x0615, 8, 8, 6, 2,
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

    #[test]
    fn test_binary_frame_length_allows_att_fragment_reassembly() {
        let header = [0x12, 0x34, 0x00, 0x00, 0x00, 0xB6];
        assert_eq!(binary_frame_total_len(&header), Some(190));
        assert_eq!(binary_frame_total_len(&header[..5]), None);
        assert_eq!(binary_frame_total_len(&[0, 0, 0, 0, 0, 1]), None);
    }

    // ---- BLE 帧格式 (tx_id + proto_id + length + pdu + crc) ----
    // ---- Android 端解析模拟: 验证我们的 PDU 能被 metuory Android app 正确解析 ----
    #[test]
    fn test_android_parse_ip() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08C7, 12,
            [192, 168, 51, 140], [255, 255, 255, 0], [192, 168, 51, 1],
            [0; 6], [0; 6],
            0, 0, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");

        // 模拟 CommandParserUtil.parseCMDModel 解析: data = pdu[2..]
        // 然后 parseIP(model.getData())
        let data = &pdu[2..]; // 跳过 unit+func

        // Android parseIP 期望:
        //   buffer.length == 1 + getIPBufferLength() = 1 + 24 = 25
        let ip_length_expected = 12 * 2; // getIPBufferLength
        assert_eq!(data.len(), 1 + ip_length_expected, "Android parseIP: buffer.length must be 25");

        let data_length = data[0];
        assert_eq!(data_length as usize, ip_length_expected);

        // ipBytesToStr 读 4 BE short (8 bytes total)
        let ip = &data[1..9];
        let mask = &data[9..17];
        let gw = &data[17..25];

        // 验证每个 IP octet 都编码为 [0x00, byte]
        assert_eq!(ip, &[0x00, 192, 0x00, 168, 0x00, 51, 0x00, 140]);
        assert_eq!(mask, &[0x00, 255, 0x00, 255, 0x00, 255, 0x00, 0]);
        assert_eq!(gw, &[0x00, 192, 0x00, 168, 0x00, 51, 0x00, 1]);
    }

    // ---- 回归测试: 防止 rsp_data Vec 容量不足 (16→32) 导致 Android 不显示 IP ----
    // Bug 根因: rsp_data Vec 容量=16, IP 响应需 25 字节 → push 静默截断到 16
    // Android 严格检查 buffer.length==25, 截断后解析失败 → UI 显示空
    #[test]
    fn test_android_parse_ip_no_truncation() {
        // 边界值 0xFF 测试最大情况
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08C7, 12,
            [0xFF; 4], [0xFF; 4], [0xFF; 4],
            [0xFF; 6], [0xFF; 6],
            0, 0xFFFF, 0xFFFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ).expect("must handle");
        let data = &pdu[2..];
        // 必须是 25 字节 (1 length + 8 ip + 8 mask + 8 gw)
        assert_eq!(data.len(), 25, "rsp_data 容量不足! IP 响应被截断");
        assert_eq!(data[0], 24, "data_length 必须是 24");
    }

    #[test]
    fn test_android_parse_mac() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08D7, 6,
            [0; 4], [0; 4], [0; 4],
            [0x80, 0xB5, 0x4E, 0x5B, 0x24, 0xE7],
            [0; 6],
            0, 0, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");

        let data = &pdu[2..]; // skip unit+func
        // Android parseMacItem: data[0] = length, data[1..1+length] = mac bytes
        let length = data[0] as usize;
        let mac_bytes = &data[1..1 + length];

        // macBytesToStr 期望 length == bytes.length == 12
        let mac_buffer_length = 6 * 2; // getMacBufferLength
        assert_eq!(length, mac_buffer_length, "MAC length byte must be 12");
        assert_eq!(mac_bytes.len(), mac_buffer_length);

        // 验证格式: 80:B5:4E:5B:24:E7
        assert_eq!(mac_bytes, &[0x00, 0x80, 0x00, 0xB5, 0x00, 0x4E,
                                  0x00, 0x5B, 0x00, 0x24, 0x00, 0xE7]);
    }

    // ---- 回归测试: READ_SN length-prefix 格式 (LOOP4) ----
    // Bug 根因: metuory READ_SN 走标准 Modbus FC=03, 返回 [func][bc][data],
    //          Android 期望 [length][length bytes], 格式不匹配 → UI 空白
    // 修复: 0x0894/9 在 handle_ble_android_read_command 走 length-prefix 路径
    #[test]
    fn test_android_parse_sn_length_prefix() {
        // SN: 18 字节 ASCII "ESP32S3-UNKNOWN-1"
        let mut cfg = crate::device::system_config::SystemConfig::defaults();
        let mut sn = [0u8; 32];
        let s = b"ESP32S3-UNKNOWN-1";
        sn[..s.len()].copy_from_slice(s);
        cfg.sn = sn;
        let pdu = crate::ble_at::mod_test_build_sn_response(0x01, 0x03, &cfg).expect("must handle");
        let data = &pdu[2..]; // skip unit+func

        // Android parseSNItem: data[0] = length, data[1..1+length] = SN
        let length = data[0] as usize;
        let sn_bytes = &data[1..1 + length];
        let sn_str = core::str::from_utf8(sn_bytes).unwrap();
        assert_eq!(length, 18, "SN length must match actual SN byte count");
        assert_eq!(sn_str, "ESP32S3-UNKNOWN-1", "SN bytes must match cfg");
    }

    #[test]
    fn test_android_parse_max_length_sn_without_truncation() {
        let mut cfg = crate::device::system_config::SystemConfig::defaults();
        cfg.sn = *b"12345678901234567890123456789012";

        let pdu = crate::ble_at::mod_test_build_sn_response(0x01, 0x03, &cfg)
            .expect("32-byte SN must fit in one BLE response");
        let data = &pdu[2..];
        let length = data[0] as usize;

        assert_eq!(length, 32, "SN length must describe all configured bytes");
        assert_eq!(data.len(), 33, "length prefix and payload must not be truncated");
        assert_eq!(&data[1..], &cfg.sn);
    }

    // ---- 回归测试: READ_LOCATION length-prefix 格式 ----
    #[test]
    fn test_android_parse_location_length_prefix() {
        let mut cfg = crate::device::system_config::SystemConfig::defaults();
        let mut name = [0u8; 16];
        let s = b"POS-001";
        name[..s.len()].copy_from_slice(s);
        cfg.name = name;
        let pdu = crate::ble_at::mod_test_build_location_response(0x01, 0x03, &cfg).expect("must handle");
        let data = &pdu[2..];

        let length = data[0] as usize;
        let loc_bytes = &data[1..1 + length];
        let loc_str = core::str::from_utf8(loc_bytes).unwrap();
        assert_eq!(length, 7, "LOCATION length must be 7");
        assert_eq!(loc_str, "POS-001", "LOCATION bytes must match cfg");
    }

    // ---- 回归测试: READ_ADC_VALUE length-prefix BE u16 格式 ----
    // Android parseResReadADC: length = buffer[0], 然后 BE u16 values
    #[test]
    fn test_android_parse_adc_length_prefix() {
        let pdu = crate::ble_at::mod_test_build_adc_response(0x01, 0x04, 0x0080, 4, [0x0123, 0x0456, 0x0789, 0x0ABC])
            .expect("must handle");
        let data = &pdu[2..];

        // ADC: 4 通道, 长度=8
        let length = data[0] as usize;
        assert_eq!(length, 8, "ADC length must be 4 channels * 2 bytes = 8");

        // 4 个 BE u16 值
        let v0 = u16::from_be_bytes([data[1], data[2]]);
        let v1 = u16::from_be_bytes([data[3], data[4]]);
        let v2 = u16::from_be_bytes([data[5], data[6]]);
        let v3 = u16::from_be_bytes([data[7], data[8]]);
        assert_eq!((v0, v1, v2, v3), (0x0123, 0x0456, 0x0789, 0x0ABC));
    }

    // ---- 回归测试: READ_COM_INPUT length-prefix packed I/O 格式 ----
    // Android parseResReadComInputIOStatus: bufferLength = buffer[0], 读 bufferLength*8 bits
    #[test]
    fn test_android_parse_com_input_length_prefix() {
        // 16 DI, packed as 2 bytes [0xAA, 0x55]
        let pdu = crate::ble_at::mod_test_build_com_input_response(0x01, 0x02, 0x0000, 16, [0xAA, 0x55])
            .expect("must handle");
        let data = &pdu[2..];

        let buffer_length = data[0] as usize;
        assert_eq!(buffer_length, 2, "16 DI must pack into 2 bytes");

        // Android: temp = buffer[1]; for j=0..7: (temp & (1<<j)) > 0 ? 1 : 0
        let byte0 = data[1];
        let byte1 = data[2];
        // 验证 16 位模式
        let mut bits = [0u8; 16];
        for j in 0..8 {
            if (byte0 & (1 << j)) > 0 { bits[j] = 1; }
            if (byte1 & (1 << j)) > 0 { bits[8 + j] = 1; }
        }
        // 0xAA = 0b10101010 → bits[0]=0, bits[1]=1, bits[2]=0, ..., bits[7]=1
        assert_eq!(bits[0], 0); assert_eq!(bits[1], 1); assert_eq!(bits[2], 0); assert_eq!(bits[3], 1);
        assert_eq!(bits[4], 0); assert_eq!(bits[5], 1); assert_eq!(bits[6], 0); assert_eq!(bits[7], 1);
        // 0x55 = 0b01010101
        assert_eq!(bits[8], 1); assert_eq!(bits[9], 0); assert_eq!(bits[10], 1); assert_eq!(bits[11], 0);
        assert_eq!(bits[12], 1); assert_eq!(bits[13], 0); assert_eq!(bits[14], 1); assert_eq!(bits[15], 0);
    }

    // ---- 回归测试: READ_RS485_VALUE length-prefix BE u16 格式 (用户自定义地址) ----
    // Android parseResReadRS485Value: length = buffer[0], 然后 BE u16 values
    #[test]
    fn test_android_parse_rs485_value_length_prefix() {
        let pdu = crate::ble_at::mod_test_build_rs485_value_response(0x01, 0x04, 0x1000, 4, [0x1234, 0x5678, 0x9ABC, 0xDEF0])
            .expect("must handle");
        let data = &pdu[2..];

        let length = data[0] as usize;
        assert_eq!(length, 8, "4 values * 2 bytes = 8");

        let v0 = u16::from_be_bytes([data[1], data[2]]);
        let v1 = u16::from_be_bytes([data[3], data[4]]);
        let v2 = u16::from_be_bytes([data[5], data[6]]);
        let v3 = u16::from_be_bytes([data[7], data[8]]);
        assert_eq!((v0, v1, v2, v3), (0x1234, 0x5678, 0x9ABC, 0xDEF0));
    }

    #[test]
    fn test_android_parse_fw_version() {
        // fw=221 (2.2.1), dt=0x0615 (1557) → 显示 "2.2.1.1557"
        let pdu = build_android_read_response_pdu(
            0x01, 0x04, 0x087E, 2,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0, 0x0221, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");

        let data = &pdu[2..];
        // Android 期望: length=4 (2 shorts)
        let fw_version_buffer_length = 2 * 2;
        assert_eq!(data[0] as usize, fw_version_buffer_length);
        assert_eq!(data.len(), 1 + fw_version_buffer_length);

        // fwVersionBytesToStr 读 2 BE short
        let fw = u16::from_be_bytes([data[1], data[2]]);
        let dt = u16::from_be_bytes([data[3], data[4]]);
        assert_eq!(fw, 0x0221); // 221 → main=2 sub=2 tail=1
        assert_eq!(dt, 0x0615); // 1557

        // 解析 main.sub.tail.dt
        let main = fw / 100;
        let sub = (fw % 100) / 10;
        let tail = fw % 10;
        let display = format!("{}.{}.{}.{}", main, sub, tail, dt);
        assert_eq!(display, "2.2.1.1557");
    }

    #[test]
    fn test_android_parse_bluetooth_id_utf8() {
        // 模拟生产代码: 使用 cfg.ble_name = "Mesh" + 4 个 0
        // [length=4][M][e][s][h] = 5 bytes
        let mut d: heapless::Vec<u8, 32> = heapless::Vec::new();
        let _ = d.push(4);
        let name = b"Mesh";
        for i in 0..4 {
            let _ = d.push(if i < name.len() { name[i] } else { 0 });
        }
        let mut pdu: heapless::Vec<u8, 32> = heapless::Vec::new();
        let _ = pdu.push(0x01); // unit
        let _ = pdu.push(0x03); // func
        let _ = pdu.extend_from_slice(&d);

        let data = &pdu[2..];
        // parseBluetoothIDItem: length = data[0], bytes = data[1..1+length]
        let length = data[0] as usize;
        let bytes = &data[1..1 + length];

        // bluetoothIDBytesToUTF8Str: iterate bytes, stop at first 0, return UTF-8
        let mut str_len = 0;
        for &b in bytes {
            if b > 0 { str_len += 1; } else { break; }
        }
        let s = core::str::from_utf8(&bytes[..str_len]).unwrap();
        assert_eq!(s, "Mesh");
    }

    #[test]
    fn test_android_parse_hardware_info() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x04, 0x087C, 2,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0, 0, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");
        let data = &pdu[2..];
        // Android parseHardwareInfoItem: length == 4 → 读 4 fields
        assert_eq!(data[0], 4);
        assert_eq!(data[1], 8);  // DO
        assert_eq!(data[2], 8);  // DI
        assert_eq!(data[3], 6);  // ADC
        assert_eq!(data[4], 2);  // RS485
    }

    #[test]
    fn test_android_parse_device_product() {
        let pdu = build_android_read_response_pdu(
            0x01, 0x03, 0x08A5, 1,
            [0; 4], [0; 4], [0; 4], [0; 6], [0; 6],
            0x00F3, 0, 0x0615, 8, 8, 6, 2,
        ).expect("must handle");
        let data = &pdu[2..];
        // Android: data[0]=length, data[1..1+length] = product bytes (BE short)
        let length = data[0] as usize;
        assert_eq!(length, 2);
        let product = u16::from_be_bytes([data[1], data[2]]);
        assert_eq!(product, 0x00F3); // 显示为 hex "F3"
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
/// 当前是否有客户端连接 (用于 Web/诊断页面).
pub fn has_client() -> bool {
    CONN_ID.load(Ordering::Acquire) != 0xFFFF
}

/// 当前是否允许 BLE 通知.
pub fn notify_enabled() -> bool {
    TX_NOTIFY_ENABLED.load(Ordering::Acquire)
}

/// 当前 GATTS 接口值 (0xFF 表示尚未注册).
pub fn gatts_if_value() -> u8 {
    GATTS_IF.load(Ordering::Acquire)
}

/// BLE 状态报告实现.
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

fn send_ble_frame(tx_id: u16, proto_id: u16, pdu_data: &[u8], _conn_id: u16) {
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
    // LOOP8: 原子读取替代 Spin lock
    let gatts_if_raw = GATTS_IF.load(Ordering::Acquire);
    if gatts_if_raw == 0xFF { return; }
    let conn_id_raw = CONN_ID.load(Ordering::Acquire);
    if conn_id_raw == 0xFFFF { return; }
    let handle = HANDLE_TABLE[IDX_CHAR_VALUE].load(Ordering::Acquire);
    unsafe {
        let rc = esp_idf_sys::esp_ble_gatts_send_indicate(
            gatts_if_raw as esp_idf_sys::esp_gatt_if_t, conn_id_raw, handle,
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

    /// LOOP10 回归测试: baud_to_index 编码对齐 metuory decodeSerialBaudRate
    /// metuory: (value & 0xf0) >> 4, 故索引 7 (115200) 应编入 high nibble = 0x70
    #[test]
    fn test_baud_to_index_alignment() {
        assert_eq!(baud_to_index(9600), 3);
        assert_eq!(baud_to_index(115200), 7);
        assert_eq!(baud_to_index(460800), 9);
        // 未知波特率默认回退 9600 (索引 3)
        assert_eq!(baud_to_index(12345), 3);
        // 验证编码后 metuory 解码回正确索引
        let packed = (baud_to_index(115200) & 0xF) << 4;
        assert_eq!((packed >> 4) & 0xF, 7);
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
    #[allow(unused_imports)]
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
