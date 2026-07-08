//! Wi-Fi 模块 (ESP32-S3 内置, 作为以太网冗余或 AP 配置入口)
//!
//! ESP32-S3R8 内置 Wi-Fi 802.11 b/g/n, 与 BLE 共用 2.4GHz 射频 (硬件分时复用)。
//! 本模块作为**以太网冗余链路**或**AP 配置入口**:
//!
//! - **Station 模式** (默认): 连接到上游 AP, 作为以太网故障时的备份链路
//! - **AP 模式** (TODO): 自身作为 AP, 提供手机直连配置入口 (与 BLE AT 命令并行)
//!
//! # 与以太网的协作
//!
//! - 默认走以太网 (W5500); Wi-Fi 仅作备份, 不主动启用 TCP/IP 转发
//! - 未来可加入链路优先级: eth_up 时关 Wi-Fi, eth_down 时启用 Wi-Fi (省电)
//! - 与 BLE 共存: ESP32-S3 内置 coexistence, sdkconfig 已配置
//!
//! # TODO
//!
//! - SSID/password 从 SystemConfig 加载 (运行时可配)
//! - AP 模式实现 (提供手机 APP 配置入口)
//! - 链路故障切换: 检测 eth link down → 启用 Wi-Fi; eth up → 关闭 Wi-Fi
//! - 路由表管理: eth/wifi 优先级, 避免双接口路由冲突

use std::sync::Arc;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::ipv4::Ipv4Addr;
use esp_idf_svc::netif::{EspNetif, NetifStack};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration};

use crate::config::wifi as cfg;
use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// Wi-Fi 心跳任务 (链路状态监测)
static WIFI_HB: TaskHb = TaskHb::new_with_stall("wifi-heartbeat", 30);

/// 启动 Wi-Fi Station 模式
///
/// 流程:
/// 1. 创建默认 Wi-Fi netif (Station)
/// 2. 创建 BlockingWifi (内部订阅 Wi-Fi/IP 事件)
/// 3. 配置 SSID/password
/// 4. 启动并阻塞等待连接 + 获取 IP
/// 5. 启动心跳任务 (后台监测链路状态)
///
/// 注意: 本函数会阻塞直到连接成功或超时 (30s)。
/// 调用方应在主线程之外的子线程中调用, 或自行 spawn。
pub fn start(hal: Arc<Hal>, sys_loop: EspSystemEventLoop) -> AppResult<()> {
    // hal 当前未直接使用 (Wi-Fi 全内置, 无外部引脚), 保留参数以备将来 AP LED 指示等
    let _ = &hal;

    log::info!("[wifi] initializing ESP32-S3 Wi-Fi (Station mode)...");

    // 1. 创建 Wi-Fi netif (Station)
    let netif = EspNetif::new(NetifStack::Wifi)
        .map_err(|e| AppError::Sys(format!("wifi netif: {e:?}")))?;

    // 2. 创建 BlockingWifi
    let mut wifi = BlockingWifi::wrap(
        esp_idf_svc::wifi::Wifi::new(netif, sys_loop.clone()),
        std::time::Duration::from_secs(30),
    )
    .map_err(|e| AppError::Sys(format!("wifi wrap: {e:?}")))?;

    // 3. 配置 Station (SSID/password 从 config::wifi 读取)
    let auth = if cfg::PASSWORD.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };
    let client_cfg = ClientConfiguration {
        ssid: cfg::SSID.into(),
        password: cfg::PASSWORD.into(),
        auth_method: auth,
        ..Default::default()
    };
    wifi.set_configuration(&Configuration::Client(client_cfg))
        .map_err(|e| AppError::Sys(format!("wifi config: {e:?}")))?;

    // 4. 启动 + 阻塞等待连接 + 获取 IP
    log::info!("[wifi] connecting to SSID={}...", cfg::SSID);
    wifi.start()
        .map_err(|e| AppError::Sys(format!("wifi start: {e:?}")))?;
    wifi.connect()
        .map_err(|e| AppError::Sys(format!("wifi connect: {e:?}")))?;
    wifi.wait_netif_up()
        .map_err(|e| AppError::Sys(format!("wifi wait_netif_up: {e:?}")))?;

    let ip: Ipv4Addr = wifi.sta_netif().get_ip_info().ip;
    log::info!("[wifi] connected, got IP: {}", ip);

    // 5. 心跳任务
    spawn_heartbeat()?;

    log::info!("[wifi] Station startup complete");
    Ok(())
}

/// Wi-Fi 心跳任务 (后台监测链路状态)
///
/// 周期性检查 Wi-Fi 链路 IP 是否仍存在, 失败时记日志。
/// 不触发系统复位 (与 eth 不同, Wi-Fi 作为备份链路, 失败可接受)。
fn spawn_heartbeat() -> AppResult<()> {
    health::register(&WIFI_HB);
    std::thread::Builder::new()
        .name("wifi-heartbeat".into())
        .spawn(move || {
            crate::health::pin_current_to_core(crate::health::CORE_NET);
            let period = std::time::Duration::from_secs(cfg::HEARTBEAT_PERIOD_S);
            loop {
                WIFI_HB.tick();
                // TODO: 检查 wifi 链路状态 (netif_is_up / ip 是否丢失)
                // 当前仅周期上报心跳, 实际链路检测待实现
                std::thread::sleep(period);
            }
        })
        .map_err(|e| AppError::Sys(format!("spawn wifi-heartbeat: {e}")))?;
    log::info!("[wifi] heartbeat task started, period={}s", cfg::HEARTBEAT_PERIOD_S);
    Ok(())
}
