//! ESP32-S3R2 嵌入式网关主入口
//!
//! 系统组成：
//! - 以太网 (W5500 over SPI2) + 应用层简单冗余
//! - BLE Mesh (Proxy + Node + Generic OnOff 模型) — ESP32-S3R2 内置
//! - 8 路 DI / 8 路 DO
//! - 6 路 AI (ADC1) / 4 路 AO (LEDC PWM)
//! - 2 路 RS485 (UART1/UART2)
//! - Modbus RTU Master + Slave + TCP Server
//!
//! 启动顺序：
//! 1. 日志/NVS/事件循环
//! 2. 硬件抽象 (HAL) 初始化
//! 3. 应用层总线初始化
//! 4. 启动以太网任务
//! 5. 启动 BLE Mesh 任务
//! 6. 启动 RS485 + Modbus 任务
//! 7. 启动 IO/AI/AO 采样任务
//! 8. 进入主循环：复位计数、喂狗、状态上报

#![allow(dead_code)]
// edition 2024 配套: 显式要求 unsafe fn 内的 unsafe 操作需 unsafe block
#![warn(unsafe_op_in_unsafe_fn)]

mod actor;
mod sync;
mod error;
mod config;
mod bus;
mod device;
mod device_config;
#[cfg(feature = "ble-at")]
mod ble_at;
mod hal;
mod ethernet;
mod rs485;
mod modbus;
mod io;
mod channel;
mod health;
mod ota;
mod protocol;
#[cfg(feature = "wifi")]
mod wifi;

use std::sync::Arc;
use std::time::Duration;

use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_sys::{self as _};

use crate::hal::Hal;
use crate::config::MAIN_LOOP_PERIOD_MS;
use crate::error::AppResult;

fn main() -> AppResult<()> {
    // 1. 初始化日志
    init_logger();

    log::info!("================================================");
    log::info!("{} v{}", config::APP_NAME, config::APP_VERSION);
    log::info!("ESP32-S3R2 IoT Gateway starting...");
    log::info!("================================================");

    // 1.1 OTA 固件确认 (取消回滚)
    // 如果当前是 OTA 升级后首次启动 (状态=PENDING_VERIFY),
    // 标记新固件有效, 防止 CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE 超时后自动回滚
    // 详见 ESP-IDF OTA 文档:  app 需在首次启动时主动确认运行正常
    confirm_new_firmware();

    // 2. ESP-IDF 基础设施
    let peripherals = Peripherals::take()
        .map_err(|e| crate::error::AppError::Sys(format!("take peripherals: {e:?}")))?;
    let sys_loop = EspSystemEventLoop::take()
        .map_err(|e| crate::error::AppError::Sys(format!("take eventloop: {e:?}")))?;
    let timer_svc = EspTaskTimerService::new()
        .map_err(|e| crate::error::AppError::Sys(format!("timer svc: {e:?}")))?;

    // 2.1 初始化 TCP/IP 协议栈 (LwIP)
    // 必须在创建任何网络接口 (esp_netif_new) 之前调用, 否则 LwIP 的 tcpip 线程 mailbox
    // 未初始化, 后续 socket 操作会触发 "Invalid mbox" 断言
    // (esp_idf_svc 的 wifi/eth 封装内部不会自动调用)
    unsafe {
        let ret = esp_idf_sys::esp_netif_init();
        if ret != esp_idf_sys::ESP_OK {
            return Err(crate::error::AppError::Sys(format!(
                "esp_netif_init failed: esp_err=0x{:08X}", ret
            )));
        }
    }
    log::debug!("[main] esp_netif_init ok (LwIP tcpip thread started)");

    // 3. 硬件抽象层初始化
    let hal = hal::Hal::init(peripherals)?;
    // 4. 设备协议存储初始化 (含 NVS 加载 + 监听线程)
    // 显式初始化 NVS 分区 (解决 LoadProhibited: BLE 代码加入后启动顺序改变)
    log::info!("[main] initializing NVS flash...");
    unsafe {
        let ret = esp_idf_sys::nvs_flash_init();
        if ret != esp_idf_sys::ESP_OK && ret != esp_idf_sys::ESP_ERR_NVS_NO_FREE_PAGES {
            log::warn!("[main] nvs_flash_init: 0x{:x}", ret);
        }
    }
    let hal = Arc::new(hal);

    // 4. 复位原因记录 (在 device::init 之前, 避免线程竞争)
    let reset_reason = unsafe { esp_idf_sys::esp_reset_reason() as u32 as u8 };
    log::info!("[main] reset reason={}", reset_reason);

    // 5. 设备协议存储初始化 (含 NVS 加载 + 监听线程)
    log::info!("[main] starting device protocol store...");
    match device::init() {
        Ok(()) => log::info!("[main] device init ok"),
        Err(e) => log::error!(
            "[main] device init failed, falling back to empty ProtoStore: {}",
            e
        ),
    }

    // 6. 复位计数持久化 (NVS 已就绪, 直接读写, 不用 catch_unwind)
    let mut reset_count = device::load_reset_count();
    log::warn!("[MAIN-P2] after load_reset_count = {}", reset_count);
    reset_count = reset_count.wrapping_add(1);
    match device::save_reset_count(reset_count) {
        Ok(()) => log::info!("[main] reset count={}", reset_count),
        Err(e) => log::warn!("[main] save reset count failed: {}", e),
    }
    // 复位计数/原因直接写 IO.sys 原子, 不再经 legacy Bus (阶段 D)
    bus::IO.sys.set_reset_count(reset_count);
    bus::IO.sys.set_reset_reason(reset_reason);

    // 5. 启动以太网 (W5500)
    #[cfg(feature = "ethernet-w5500")]
    {
        log::info!("[main] starting ethernet (W5500)...");
       
        ethernet::start(hal.clone(), sys_loop.clone())?;
    }

    // 5.1 启动 Wi-Fi (ESP32-S3 内置, 作为以太网冗余链路, 默认不启用)
    // 用 `--features wifi` 启用, 与 BLE 共存 (sdkconfig 已配 COEX)
    #[cfg(feature = "wifi")]
    {
        log::info!("[main] starting Wi-Fi (Station mode)...");
        // Wi-Fi 启动失败仅记日志, 不阻断主流程 (作为备份链路)
        if let Err(e) = wifi::start(hal.clone(), sys_loop.clone()) {
            log::warn!("[main] Wi-Fi startup failed (continuing): {}", e);
        }
    }

    // 6. 通信协议注册表 (插件化管理 Modbus RTU/TCP + BLE Mesh)
    let mut protocols = protocol::ProtocolRegistry::new();

    // 6. 启动 BLE GATT Server (标准 BLE)
    #[cfg(feature = "ble-at")]
    {
        log::info!("[main] starting ble gatt server...");
        ble_at::start()?;
    }
    #[cfg(feature = "io-di-do")]
    {
        log::info!("[main] starting io scan task...");
        io::start(hal.clone())?;
    }

    // 8. 启动 AI/AO 通道
    #[cfg(feature = "ai-ao")]
    {
        log::info!("[main] starting ai/ao task...");
        channel::start(hal.clone(), timer_svc.clone())?;
    }

    // 9. 注册 + 启动 Modbus 通信协议 (通过 ProtocolRegistry 插件化管理)
    #[cfg(feature = "modbus-rtu")]
    {
        log::info!("[main] registering modbus rtu...");
        protocols.register(Box::new(protocol::ModbusRtuProtocol::new(hal.clone())));
    }
    #[cfg(feature = "modbus-tcp")]
    {
        log::info!("[main] registering modbus tcp...");
        protocols.register(Box::new(protocol::ModbusTcpProtocol::new()));
    }
    protocols.start_all()?;

    // 10. 主循环
    log::info!("[main] entering main loop (period={}ms)", MAIN_LOOP_PERIOD_MS);
    // 打印任务-核心分配 (便于验证双核优化)
    health::print_core_assignment();
    if let Err(e) = main_loop(timer_svc, hal.clone()) {
        log::error!("[main] main loop returned error: {e}");
        // 不再立即重启, 尝试降级运行
        crate::error::recovery::record_failure(
            crate::error::recovery::Severity::Severe,
            "main",
            &format!("main loop exited: {e}"),
        );
        // 进入降级模式
        crate::error::recovery::enter_mode(
            crate::error::recovery::DegradedMode::Minimal
        );
        // 短暂等待, 给系统机会继续响应
        std::thread::sleep(Duration::from_secs(1));
        // 注: 不直接重启, 让系统尝试在降级模式下继续
        loop {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// 主循环：周期性更新系统状态、复位计数、喂狗、健康检查
// ----------------------------------------------------------------------------
fn main_loop(_timer_svc: EspTaskTimerService, hal: Arc<Hal>) -> AppResult<()> {
    let mut tick: u32 = 0;
    let period = Duration::from_millis(MAIN_LOOP_PERIOD_MS);
    let start = std::time::Instant::now();

    // 把 main 任务加入 ESP-IDF Task Watchdog (10s 超时)
    health::subscribe_wdt();

    loop {
        tick = tick.wrapping_add(1);

        // 每个周期喂狗 (100ms), 远小于 WDT 超时 10s
        health::feed_wdt();

        // AI/AO/DI/DO tick (架构合并 Phase 2: 取消独立 pthread)
        #[cfg(feature = "ai-ao")]
        {
            crate::channel::ai::tick_ai_sample(&hal);
            crate::channel::ao::tick_ao_output(&hal);
        }
        // DI/DO 每 5 tick (20ms) 调用一次, 保留去抖逻辑
        if tick % 5 == 0 {
            #[cfg(feature = "io-di-do")]
            crate::io::di::tick_di_scan(&hal);
        }
        // DO notify 由 modbus 写入触发 (见 bus::backends::write_coil), tick_do_output 在 100ms poll 中调用
        #[cfg(feature = "io-di-do")]
        crate::io::do_::tick_do_output(&hal);

        // BLE 通知发送 (每 100ms, 替代独立线程)
        #[cfg(feature = "ble-at")]
        ble_at::process_tick();

        // 消费 IO 事件 (避免事件队列满, 触发重置丢失关键状态变化)
        // 阶段 3: DI 变化触发 BLE notify (REPORT_COM_INPUT_IO_STATUS 0x94, Android tx_id=0x01)
        while let Some(event) = crate::bus::event_bus::recv_event() {
            match event {
                crate::bus::IoEvent::DiChanged => {
                    // DI 变化: 发 BLE 通知给 Android, 走 BINARY_TX 队列异步发送
                    #[cfg(feature = "ble-at")]
                    {
                        // conn_id 取 0 (send_ble_frame 内会尝试锁定 GATTS_IF/CONN_ID 读)
                        crate::ble_at::send_di_status_report(0);
                    }
                }
                crate::bus::IoEvent::DoChanged => {
                    // DO 变化: 暂不主动上报 (Modbus TCP 客户端可轮询)
                    log::debug!("[main] DO changed event received");
                }
                crate::bus::IoEvent::AiSampled => {
                    // AI 采样完成: 暂不主动上报
                }
                crate::bus::IoEvent::AoUpdated => {
                    // AO 输出更新: 暂不主动上报
                }
                crate::bus::IoEvent::ResetRequested => {
                    // 已在 1s tick 中处理, 忽略
                }
                crate::bus::IoEvent::IpAssigned(ip_b, mask_b, gw_b) => {
                    // DHCP 完成, 把 IP/mask/gw 写入 CONFIG RCU
                    log::info!("[eth] main loop: IP assigned {}.{}.{}.{}",
                        ip_b[0], ip_b[1], ip_b[2], ip_b[3]);
                    crate::bus::backends::config_modify(|c| {
                        c.ip = ip_b;
                        c.mask = mask_b;
                        c.gateway = gw_b;
                        c.dhcp = true;
                    });
                }
            }
        }

        // 每 1s 更新 uptime + 检查任务心跳
        if tick % (1000 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            let uptime = start.elapsed().as_secs() as u32;
            // 阶段 A: uptime 写 + reset-request 读 全过 bus::IO.sys (原子), 无锁
            crate::bus::IO.sys.set_uptime(uptime);
            if crate::bus::IO.sys.is_reset_requested() {
                log::warn!("[main] reset requested via modbus (user-initiated)");
                crate::error::recovery::record_failure(
                    crate::error::recovery::Severity::Fatal,
                    "main",
                    "user-requested reset via modbus",
                );
                std::thread::sleep(Duration::from_millis(100));
                unsafe { esp_idf_sys::esp_restart() };
            }

            // 任务健康检查: 检查所有注册任务的心跳
            // check_all 内部已更新 last_check 快照, 无需额外 snapshot
            let stalled = health::check_all();
            if !stalled.is_empty() {
                log::warn!("[main] stalled tasks: {:?}", stalled.as_slice());
            }

            log::info!("[main] uptime={}s tick={}", uptime, tick);
        }

        std::thread::sleep(period);
    }
}

// ----------------------------------------------------------------------------
// 日志初始化
// ----------------------------------------------------------------------------
fn init_logger() {
    esp_idf_svc::log::EspLogger::initialize_default();
    // 默认 INFO 级别 (运行时可通过 Modbus 寄存器 0x0106 调节)
    log::set_max_level(log::LevelFilter::Info);
    // 安装 Rust panic hook, 打印 panic 位置 + backtrace
    // 注意: release 用 panic=abort, hook 仍会被调用 (在 abort 之前)
    install_panic_hook();
}

/// 安装 Rust panic hook, 打印 panic 位置 + backtrace
///
/// ESP-IDF 自身的 panic handler (CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT) 已会打印
/// CPU 寄存器 + Xtensa backtrace; 此 Rust hook 用于补充 Rust 层 backtrace。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 打印 panic 位置
        log::error!("========== RUST PANIC ==========");
        log::error!("panic: {}", info);
        if let Some(loc) = info.location() {
            log::error!(
                "  at {}:{}:{}",
                loc.file(),
                loc.line(),
                loc.column()
            );
        }
        // 打印 backtrace (std::backtrace::Backtrace::force_capture)
        let bt = std::backtrace::Backtrace::force_capture();
        log::error!("backtrace:\n{}", bt);
        log::error!("================================");
        // 调用默认 hook (会触发 ESP-IDF panic handler → 复位)
        default_hook(info);
    }));
    log::debug!("[main] Rust panic hook installed (with backtrace)");
}

/// 确认 OTA 新固件运行正常, 取消自动回滚
///
/// ESP-IDF 启用 `CONFIG_APP_ROLLBACK_ENABLE` 后, OTA 升级后的新固件首次启动
/// 状态为 `ESP_OTA_IMG_PENDING_VERIFY`。若 app 不主动调用
/// `esp_ota_mark_app_valid_cancel_rollback` 标记有效, 下次重启会自动回滚到旧固件。
///
/// 本函数在 main 入口调用, 表示"启动到此处即认为新固件可运行"。
/// 如需更严格的确认 (如启动所有任务 + 网络连通后才确认), 可改为延后到主循环中调用。
fn confirm_new_firmware() {
    // 获取当前运行的 OTA 分区
    let partition = unsafe { esp_idf_sys::esp_ota_get_running_partition() };
    if partition.is_null() {
        log::warn!("[main] OTA: get running partition failed (null), skip confirm");
        return;
    }

    // 查询分区状态
    let mut state: esp_idf_sys::esp_ota_img_states_t = 0;
    let ret = unsafe { esp_idf_sys::esp_ota_get_state_partition(partition, &mut state) };
    if ret != esp_idf_sys::ESP_OK {
        log::warn!("[main] OTA: get state failed (err={}), skip confirm", ret);
        return;
    }

    // ESP_OTA_IMG_PENDING_VERIFY = 2 (新固件待确认)
    // 如状态不是 PENDING_VERIFY (如 factory 启动 / 已确认), 直接返回
    const ESP_OTA_IMG_PENDING_VERIFY: esp_idf_sys::esp_ota_img_states_t = 2;
    if state != ESP_OTA_IMG_PENDING_VERIFY {
        log::debug!("[main] OTA: state={} (not pending verify), skip confirm", state);
        return;
    }

    // 标记新固件有效, 取消回滚
    let ret = unsafe { esp_idf_sys::esp_ota_mark_app_valid_cancel_rollback() };
    if ret == esp_idf_sys::ESP_OK {
        log::info!("[main] OTA: new firmware confirmed valid (rollback cancelled)");
    } else {
        log::error!("[main] OTA: mark valid failed (err={}), firmware may rollback next reboot", ret);
    }
}
