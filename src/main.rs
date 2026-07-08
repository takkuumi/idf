//! ESP32-S3R8 嵌入式网关主入口
//!
//! 系统组成：
//! - 以太网 (W5500 over SPI2) + 应用层简单冗余
//! - BLE Mesh (Proxy + Node + Generic OnOff 模型) — ESP32-S3R8 内置
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

mod error;
mod config;
mod bus;
mod device;
mod ble_at;
mod hal;
mod ethernet;
mod rs485;
mod modbus;
mod io;
mod channel;
mod blemesh;
mod health;
mod ota;
mod protocol;
#[cfg(feature_wifi)]
mod wifi;

use std::sync::Arc;
use std::time::Duration;

use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_sys::{self as _};

use crate::config::MAIN_LOOP_PERIOD_MS;
use crate::error::AppResult;

fn main() -> AppResult<()> {
    // 1. 初始化日志
    init_logger();

    log::info!("================================================");
    log::info!("{} v{}", config::APP_NAME, config::APP_VERSION);
    log::info!("ESP32-S3R8 IoT Gateway starting...");
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

    // 3. 硬件抽象层初始化
    let hal = hal::Hal::init(peripherals)?;
    let hal = Arc::new(hal);

    // 4. 设备协议存储初始化 (含 NVS 加载 + 监听线程)
    // 失败时回退到空 ProtoStore (bus 已有默认值), 避免设备无法启动
    log::info!("[main] starting device protocol store...");
    match device::init() {
        Ok(()) => log::info!("[main] device init ok"),
        Err(e) => log::error!(
            "[main] device init failed, falling back to empty ProtoStore: {}",
            e
        ),
    }

    // 4.1 复位原因记录 + 复位计数持久化 (工业可靠性)
    // esp_reset_reason_t: 1=POWERON 2=EXT 3=SW 4=PANIC 5=INT_WDT 6=TASK_WDT 7=WDT 15=BROWNOUT
    let reset_reason = unsafe { esp_idf_sys::esp_reset_reason() } as u8;
    let mut reset_count = device::load_reset_count();
    reset_count = reset_count.wrapping_add(1);
    if let Err(e) = device::save_reset_count(reset_count) {
        log::warn!("[main] save reset count failed: {}", e);
    }
    if let Some(mut b) = bus::lock_timeout() {
        b.sys.reset_count = reset_count;
        b.sys.reset_reason = reset_reason;
    }
    log::info!(
        "[main] reset reason={}, count={}",
        reset_reason, reset_count
    );

    // 5. 启动以太网 (W5500)
    #[cfg(feature_ethernet)]
    {
        log::info!("[main] starting ethernet (W5500)...");
        ethernet::start(hal.clone(), sys_loop.clone())?;
    }

    // 5.1 启动 Wi-Fi (ESP32-S3 内置, 作为以太网冗余链路, 默认不启用)
    // 用 `--features wifi` 启用, 与 BLE 共存 (sdkconfig 已配 COEX)
    #[cfg(feature_wifi)]
    {
        log::info!("[main] starting Wi-Fi (Station mode)...");
        // Wi-Fi 启动失败仅记日志, 不阻断主流程 (作为备份链路)
        if let Err(e) = wifi::start(hal.clone(), sys_loop.clone()) {
            log::warn!("[main] Wi-Fi startup failed (continuing): {}", e);
        }
    }

    // 6. 通信协议注册表 (插件化管理 Modbus RTU/TCP + BLE Mesh)
    let mut protocols = protocol::ProtocolRegistry::new();

    // 6.1 启动 BLE Mesh (需在 ble_at 之前, ble_at 依赖 BLE 协议栈已初始化)
    #[cfg(feature_ble_mesh)]
    {
        log::info!("[main] starting ble mesh...");
        protocols.register(Box::new(protocol::BleMeshProtocol::new(hal.clone())));
        if let Some(p) = protocols.find("ble-mesh") {
            p.start()?;
        }
    }

    // 6.2 BLE AT 命令通道 (依赖 BLE 协议栈, 独立于协议注册表)
    log::info!("[main] starting ble at command channel...");
    ble_at::start()?;

    // 7. 启动 IO 扫描 (DI/DO)
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
    #[cfg(feature_modbus_rtu)]
    {
        log::info!("[main] registering modbus rtu...");
        protocols.register(Box::new(protocol::ModbusRtuProtocol::new(hal.clone())));
    }
    #[cfg(feature_modbus_tcp)]
    {
        log::info!("[main] registering modbus tcp...");
        protocols.register(Box::new(protocol::ModbusTcpProtocol::new()));
    }
    protocols.start_all()?;

    // 10. 主循环
    log::info!("[main] entering main loop (period={}ms)", MAIN_LOOP_PERIOD_MS);
    main_loop(timer_svc)?;

    log::warn!("[main] main loop exited, rebooting");
    unsafe { esp_idf_sys::esp_restart() };
    Ok(())
}

// ----------------------------------------------------------------------------
// 主循环：周期性更新系统状态、复位计数、喂狗、健康检查
// ----------------------------------------------------------------------------
fn main_loop(_timer_svc: EspTaskTimerService) -> AppResult<()> {
    crate::health::pin_current_to_core(crate::health::CORE_NET);
    let mut tick: u32 = 0;
    let period = Duration::from_millis(MAIN_LOOP_PERIOD_MS);
    let start = std::time::Instant::now();

    // 把 main 任务加入 ESP-IDF Task Watchdog (10s 超时)
    health::subscribe_wdt();

    loop {
        tick = tick.wrapping_add(1);

        // 每个周期喂狗 (100ms), 远小于 WDT 超时 10s
        health::feed_wdt();

        // 每 1s 更新 uptime + 检查任务心跳
        if tick % (1000 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            let uptime = start.elapsed().as_secs() as u32;
            if let Some(mut b) = bus::lock_timeout() {
                b.sys.uptime_s = uptime;
                // 复位请求
                if b.sys.reset_request {
                    log::warn!("[main] reset requested via modbus");
                    std::thread::sleep(Duration::from_millis(100));
                    unsafe { esp_idf_sys::esp_restart() };
                }
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
