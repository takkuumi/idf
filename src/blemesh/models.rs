//! Generic OnOff Server / Client 模型定义
//!
//! - Server: 收到 OnOff Set/Get → 更新 `bus::BUS.do_.bits` bit0 (映射到 DO0)
//! - Client: 供本机主动控制其它节点 (send OnOff Set / 接收 Status)
//!
//! 模型 ID 取自 `config::ble_mesh` (SIG: 0x1000 / 0x1001)。
//!
//! GPIO 物理输出由 `io` 任务根据总线状态刷新 (模块解耦);
//! 如需在 mesh 回调中直接驱动 GPIO, 见 `bindings` TODO (需将 hal 存入全局 OnceCell)。

use std::os::raw::c_void;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::bus;
use crate::config::ble_mesh as cfg;

use super::bindings::{
    EspBleMeshComp, EspBleMeshElem, EspBleMeshModel, EspBleMeshMsgCtx, EspBleMeshOp, ModelOpParam,
    OP_GEN_ONOFF_GET, OP_GEN_ONOFF_SET, OP_GEN_ONOFF_SET_UNACK, OP_GEN_ONOFF_STATUS,
    ROLE_NODE,
};

/// OnOff Set 消息的 Transaction Identifier (tid) 全局计数器
///
/// BLE Mesh 协议要求同一源地址发送的相同 tid 消息在一段时间内不重复,
/// 用于接收端去重。每个独立事务使用递增的 tid (0-255 循环)。
/// 同一目标可重用 tid, 但接收端会按 (src, tid, 6s 时间窗) 去重。
static TID_COUNTER: AtomicU8 = AtomicU8::new(0);

// ----------------------------------------------------------------------------
// 操作码表 (数组以 {0} 终止)
// ----------------------------------------------------------------------------
/// Server 接收: GET / SET / SET_UNACK
/// EspBleMeshOp: opcode(u32) + min_len(usize) + param_cb(u32)
/// param_cb=0 表示 blocking 消息 (默认)
static SRV_OPS: [EspBleMeshOp; 4] = [
    EspBleMeshOp { opcode: OP_GEN_ONOFF_GET, min_len: 0, param_cb: 0 },
    EspBleMeshOp { opcode: OP_GEN_ONOFF_SET, min_len: 2, param_cb: 0 },
    EspBleMeshOp { opcode: OP_GEN_ONOFF_SET_UNACK, min_len: 2, param_cb: 0 },
    EspBleMeshOp { opcode: 0, min_len: 0, param_cb: 0 }, // 终止符
];

/// Client 接收: STATUS
static CLI_OPS: [EspBleMeshOp; 2] = [
    EspBleMeshOp { opcode: OP_GEN_ONOFF_STATUS, min_len: 1, param_cb: 0 },
    EspBleMeshOp { opcode: 0, min_len: 0, param_cb: 0 },
];

// ----------------------------------------------------------------------------
// 模型 / 元素 / 组合数据 (静态, 供 esp_ble_mesh_init 引用)
// ----------------------------------------------------------------------------
/// 静态 SIG 模型数组
///
/// element_idx/model_idx/flags/element/pub_/keys/groups/cb 等字段在静态初始化时置 0,
/// 由 esp_ble_mesh_init 在注册时回填。
pub static SIG_MODELS: [EspBleMeshModel; 2] = [
    EspBleMeshModel {
        model_id: cfg::MODEL_ID_ONOFF_SRV,
        company_id: 0, // SIG model
        element_idx: 0,
        model_idx: 0,
        flags: 0,
        element: std::ptr::null_mut(),
        pub_: std::ptr::null_mut(),
        keys: [0; super::bindings::CONFIG_BLE_MESH_MODEL_KEY_COUNT],
        groups: [0; super::bindings::CONFIG_BLE_MESH_MODEL_GROUP_COUNT],
        op: SRV_OPS.as_ptr() as *mut EspBleMeshOp,
        cb: std::ptr::null_mut(),
        user_data: std::ptr::null_mut(),
    },
    EspBleMeshModel {
        model_id: cfg::MODEL_ID_ONOFF_CLI,
        company_id: 0,
        element_idx: 0,
        model_idx: 0,
        flags: 0,
        element: std::ptr::null_mut(),
        pub_: std::ptr::null_mut(),
        keys: [0; super::bindings::CONFIG_BLE_MESH_MODEL_KEY_COUNT],
        groups: [0; super::bindings::CONFIG_BLE_MESH_MODEL_GROUP_COUNT],
        op: CLI_OPS.as_ptr() as *mut EspBleMeshOp,
        cb: std::ptr::null_mut(),
        user_data: std::ptr::null_mut(),
    },
];

static ELEMENTS: [EspBleMeshElem; 1] = [EspBleMeshElem {
    element_addr: 0, // 由 esp_ble_mesh_init 回填为主单播地址
    location: 0, // TODO: primary location
    sig_model_count: 2,
    vnd_model_count: 0,
    sig_models: SIG_MODELS.as_ptr() as *mut EspBleMeshModel,
    vnd_models: std::ptr::null_mut(),
}];

static COMP: EspBleMeshComp = EspBleMeshComp {
    cid: 0x0001, // TODO: 公司 ID (测试值)
    pid: 0x0001,
    vid: 0x0001,
    element_count: 1,
    elements: ELEMENTS.as_ptr() as *mut EspBleMeshElem,
};

/// 返回组合数据指针, 供 `esp_ble_mesh_init`
pub fn composition() -> *const EspBleMeshComp {
    &COMP as *const _
}

// ----------------------------------------------------------------------------
// 模型操作回调 (由 bindings::model_event_cb 转发)
// ----------------------------------------------------------------------------
/// 处理 ESP_BLE_MESH_MODEL_OPERATION_EVT
///
/// `param` 实为 `esp_ble_mesh_model_cb_param_t*`, 这里按 `ModelOpParam` 解析。
/// v5.5.2 字段: opcode / model / ctx (指针) / length / msg (无 errcode 字段)
pub unsafe fn handle_model_operation(param: *mut c_void) {
    if param.is_null() {
        return;
    }
    let p = unsafe { &*(param as *const ModelOpParam) };
    // v5.5.2: model_operation 事件参数无 errcode 字段, 直接处理 opcode
    // 通过 ctx 指针解引用获取消息上下文 (ctx 是指针, 非 inline)
    let ctx = match unsafe { p.ctx.as_ref() } {
        Some(c) => c,
        None => {
            log::warn!("[mesh-model] null ctx in model_operation");
            return;
        }
    };
    match p.opcode {
        OP_GEN_ONOFF_GET => {
            log::info!("[mesh-model] OnOff Get (src=0x{:04x})", ctx.recv_dst);
            send_status(p.model, ctx);
        }
        OP_GEN_ONOFF_SET | OP_GEN_ONOFF_SET_UNACK => {
            // msg[0]=onoff, msg[1]=tid, (msg[2]=trans, msg[3]=delay)
            if p.length >= 1 && !p.msg.is_null() {
                let val = unsafe { *p.msg } != 0;
                apply_onoff(val);
                if p.opcode == OP_GEN_ONOFF_SET {
                    send_status(p.model, ctx); // acknowledged → 回复 status
                }
            }
        }
        OP_GEN_ONOFF_STATUS => {
            // client 收到远端 status
            log::info!("[mesh-model] OnOff Status (client recv)");
        }
        _ => {
            log::debug!("[mesh-model] opcode=0x{:x}", p.opcode);
        }
    }
}

/// 应用 OnOff 状态到总线 bit0
fn apply_onoff(value: bool) {
    if let Some(mut b) = bus::lock_timeout() {
        if value { b.do_.bits |= 0x01u64; } else { b.do_.bits &= !0x01u64; }
        log::info!("[mesh-model] DO[0] <- {}", value);
    } else {
        log::error!("[mesh-model] bus lock timeout");
    }
    // TODO: 若需立即驱动 GPIO, 在此调用 hal.gpio.write_do(0, value)
    //       (需将 Arc<Hal> 存入全局 OnceCell, 因 mesh 回调为 C fn 无法捕获环境)
}

/// 上报 OnOff Status (Server 模型回复)
///
/// 通过 `esp_ble_mesh_server_model_send_msg` 发送 (带 ctx, 用于回复 Get/Set)。
///
/// # 参数
/// - `model`: Server 模型指针 (来自回调参数)
/// - `ctx`: 接收消息的上下文 (从中复制 net_idx/app_idx, addr 用 recv_dst 回复)
///
/// # 安全性
/// `esp_ble_mesh_server_model_send_msg` 是 C API, model 必须来自回调参数。
/// ESP-IDF 保证回调期间这些指针有效。
fn send_status(model: *mut EspBleMeshModel, ctx: &EspBleMeshMsgCtx) {
    let on = bus::lock_timeout().map(|b| b.do_.bits & 0x01u64 != 0).unwrap_or(false);
    let mut msg: [u8; 1] = [on as u8];

    // 构造发送上下文 (从接收上下文复制 net_idx/app_idx, addr 用 recv_dst 回复到原目标)
    let send_ctx = EspBleMeshMsgCtx::for_send(ctx.net_idx, ctx.app_idx, ctx.recv_dst);

    // esp_ble_mesh_server_model_send_msg: 5 参数 (model, ctx, opcode, length, data)
    let ret = unsafe {
        super::bindings::esp_ble_mesh_server_model_send_msg(
            model,
            &send_ctx,
            OP_GEN_ONOFF_STATUS,
            msg.len() as u16,
            msg.as_mut_ptr(),
        )
    };

    if ret == 0 {
        log::info!("[mesh-model] sent OnOff Status: on={}", on);
    } else {
        log::warn!("[mesh-model] send OnOff Status failed: esp_err=0x{:x}", ret);
    }
}

/// 主动发送 OnOff Set (client 侧)
///
/// 通过 `esp_ble_mesh_client_model_send_msg` 发送 (带 ctx + 超时 + 是否需要响应)。
///
/// # 参数
/// - `value`: 目标 OnOff 状态
/// - `dst_addr`: 目标节点 unicast 地址
/// - `net_idx`: 网络密钥索引
/// - `app_idx`: 应用密钥索引
///
/// # 示例
/// ```no_run
/// models::send_onoff_set(true, 0x0002, 0, 0);
/// ```
pub fn send_onoff_set(value: bool, dst_addr: u16, net_idx: u16, app_idx: u16) {
    // msg[0]=onoff, msg[1]=tid (每次发送递增, 0-255 循环)
    // tid 用于接收端去重, 同一源地址 6s 时间窗内不重复
    let tid = TID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut msg: [u8; 2] = [value as u8, tid];

    let ctx = EspBleMeshMsgCtx::for_send(net_idx, app_idx, dst_addr);

    // 使用 client 模型 (SIG_MODELS[1])
    let client_model = &SIG_MODELS[1] as *const EspBleMeshModel as *mut EspBleMeshModel;

    // esp_ble_mesh_client_model_send_msg: 8 参数
    // (model, ctx, opcode, length, data, msg_timeout, need_rsp, device_role)
    let ret = unsafe {
        super::bindings::esp_ble_mesh_client_model_send_msg(
            client_model,
            &ctx,
            OP_GEN_ONOFF_SET,
            msg.len() as u16,
            msg.as_mut_ptr(),
            0,       // msg_timeout=0 使用默认超时
            true,    // need_rsp=true (OnOff Set 需要 Status 响应)
            ROLE_NODE,
        )
    };

    if ret == 0 {
        log::info!(
            "[mesh-model] sent OnOff Set: value={} dst=0x{:04x} tid={}",
            value,
            dst_addr,
            tid
        );
    } else {
        log::warn!("[mesh-model] send OnOff Set failed: esp_err=0x{:x}", ret);
    }
}
