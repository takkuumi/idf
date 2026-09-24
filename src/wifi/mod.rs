//! Wi-Fi 模块 (ESP32-S3 内置, 作为以太网冗余或 AP 配置入口)
//!
//! ESP32-S3R2 内置 Wi-Fi 802.11 b/g/n, 与 BLE 共用 2.4GHz 射频 (硬件分时复用)。
//! 本模块作为**以太网冗余链路**或**AP 配置入口**:
//!
//! - **Station 模式** (默认): 连接到上游 AP, 作为以太网故障时的备份链路
//! - **AP 模式**：当前未启用；手机配置入口使用 BLE GATT。
//!
//! # 与以太网的协作
//!
//! - 默认走以太网 (W5500); Wi-Fi 仅作备份, 不主动启用 TCP/IP 转发
//! - 未来可加入链路优先级: eth_up 时关 Wi-Fi, eth_down 时启用 Wi-Fi (省电)
//! - 与 BLE 共存: ESP32-S3 内置 coexistence, sdkconfig 已配置
//!
//! 当前构建默认不启用 Wi-Fi；该模块只在 `wifi` feature 显式启用时启动固定
//! Station 配置。它不参与以太网故障切换，也不提供 AP 配网入口。

use std::sync::Arc;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::ipv4::Ipv4Addr;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

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

    // 1. 创建 EspWifi（内部创建 STA/AP netif）并包装成阻塞式状态机。
    let modem = hal.take_wifi_modem()?;
    let esp_wifi = EspWifi::new(modem, sys_loop.clone(), None)
        .map_err(|e| AppError::Sys(format!("wifi init: {e:?}")))?;
    let mut wifi = BlockingWifi::wrap(esp_wifi, sys_loop.clone())
        .map_err(|e| AppError::Sys(format!("wifi wrap: {e:?}")))?;

    // 3. 配置 Station (SSID/password 从 config::wifi 读取)
    let auth = if cfg::PASSWORD.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };
    let client_cfg = ClientConfiguration {
        ssid: cfg::SSID
            .try_into()
            .map_err(|_| AppError::Config("wifi SSID exceeds 32 bytes".into()))?,
        password: cfg::PASSWORD
            .try_into()
            .map_err(|_| AppError::Config("wifi password exceeds 64 bytes".into()))?,
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

    let ip: Ipv4Addr = wifi
        .wifi()
        .sta_netif()
        .get_ip_info()
        .map_err(|e| AppError::Sys(format!("wifi get_ip_info: {e:?}")))?
        .ip;
    log::info!("[wifi] connected, got IP: {}", ip);

    // 5. 心跳任务必须持有 BlockingWifi。若 start() 返回时直接 drop(wifi)，
    // esp-idf-svc 会析构驱动/netif，之前建立的连接随即失效。
    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("wifi-heartbeat".into())
        .stack_size(crate::safety::stack_budget::WIFI_HEARTBEAT)
        .spawn(move || {
            health::subscribe_wdt();
            let mut was_up = true;
            loop {
                WIFI_HB.tick();
                health::feed_wdt();
                let is_up = wifi.is_connected().unwrap_or(false)
                    && wifi
                        .wifi()
                        .sta_netif()
                        .get_ip_info()
                        .map(|info| !info.ip.is_unspecified())
                        .unwrap_or(false);
                if is_up != was_up {
                    if is_up {
                        log::info!("[wifi] link restored");
                    } else {
                        log::warn!("[wifi] link lost; keeping other transports online");
                    }
                    was_up = is_up;
                }

                // 任务 WDT=10s，禁止一次 sleep 30s；分段睡眠并喂狗。
                let mut remaining = cfg::HEARTBEAT_PERIOD_S;
                while remaining > 0 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    WIFI_HB.tick();
                    health::feed_wdt();
                    remaining -= 1;
                }
            }
        });
    health::reset_thread_core();
    result.map_err(|e| AppError::Sys(format!("spawn wifi-heartbeat: {e}")))?;
    health::register_with_stack(&WIFI_HB, crate::safety::stack_budget::WIFI_HEARTBEAT);
    log::info!(
        "[wifi] heartbeat task started, period={}s",
        cfg::HEARTBEAT_PERIOD_S
    );

    log::info!("[wifi] Station startup complete");
    Ok(())
}
