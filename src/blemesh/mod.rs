//! BLE Mesh 模块
//!
//! 角色：Proxy + Node
//! 模型：Generic OnOff Server + Generic OnOff Client
//!
//! 与总线的交互：
//! - 收到 OnOff Set → 写 `BUS.do_` 第 0 路 DO
//! - DO 状态变化 → 通过 OnOff Status 上报
//!
//! 注意：ESP-IDF 的 BLE Mesh 主要以 C API 暴露，
//! esp-idf-svc 没有完整封装，需通过 `esp_idf_sys` 直接调用 C 接口。

use std::sync::Arc;

use crate::error::AppResult;
use crate::hal::Hal;

pub mod provisioning;
pub mod models;
pub mod bindings;

/// 启动 BLE Mesh 任务
pub fn start(_hal: Arc<Hal>, _nvs: esp_idf_svc::nvs::EspDefaultNvsPartition) -> AppResult<()> {
    bindings::init_ble_controller()?;
    bindings::init_bluedroid()?;
    bindings::init_mesh_stack(_nvs)?;
    bindings::register_models()?;
    bindings::enable_proxy()?;
    bindings::start_advertising()?;

    // 启动心跳任务
    crate::health::set_next_thread_core(crate::health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("mesh-heartbeat".into())
        .spawn(|| {
            if let Err(e) = bindings::heartbeat_loop() {
                log::error!("[blemesh] heartbeat task error: {e}");
            }
        });
    crate::health::reset_thread_core();
    result.map_err(|e| crate::error::AppError::BleMesh(format!("spawn heartbeat: {e}")))?;

    log::info!("[blemesh] started (Proxy+Node, OnOff models)");
    Ok(())
}
