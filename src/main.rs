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
#[cfg(feature = "ble-at")]
mod ble_at;
mod bus;
mod channel;
mod config;
mod device;
mod error;
mod ethernet;
mod hal;
mod health;
mod io;
mod modbus;
mod nfc;
mod ota;
mod protocol;
mod rs485;
mod safety;
mod sync;
mod udp_multicast;
mod web;
#[cfg(feature = "wifi")]
mod wifi;

use std::sync::Arc;
use std::time::Duration;

use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_sys::{self as _};

use crate::config::{BLE_PROCESS_PERIOD_MS, MAIN_LOOP_PERIOD_MS};
use crate::error::AppResult;
use crate::hal::Hal;

static MAIN_HB: health::TaskHb = health::TaskHb::new("main");
const BUILD_ID: &str = env!("GATEWAY_BUILD_ID");

struct MainLoopPerf {
    #[cfg(debug_assertions)]
    phase_peaks_us: [u64; 9],
}

impl MainLoopPerf {
    const fn new() -> Self {
        Self {
            #[cfg(debug_assertions)]
            phase_peaks_us: [0; 9],
        }
    }

    #[inline]
    fn measure<F>(&mut self, phase: usize, operation: F)
    where
        F: FnOnce(),
    {
        #[cfg(debug_assertions)]
        let started = std::time::Instant::now();
        operation();
        #[cfg(debug_assertions)]
        {
            let elapsed = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            self.phase_peaks_us[phase] = self.phase_peaks_us[phase].max(elapsed);
        }
        #[cfg(not(debug_assertions))]
        let _ = phase;
    }

    #[cfg(debug_assertions)]
    fn record_elapsed(&mut self, phase: usize, started: std::time::Instant) {
        let elapsed = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.phase_peaks_us[phase] = self.phase_peaks_us[phase].max(elapsed);
    }

    fn log_and_reset(&mut self) {
        #[cfg(debug_assertions)]
        {
            let p = &self.phase_peaks_us;
            log::info!(
                "[perf] phase_peak_us tcp={} ai_ao={} di={} do={} persist={} eth={} ble={} events={} housekeeping={}",
                p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8]
            );
            self.phase_peaks_us.fill(0);
        }
    }
}

fn main() -> AppResult<()> {
    // 1. 初始化日志
    init_logger();

    log::info!("================================================");
    log::info!("{} v{}", config::APP_NAME, config::APP_VERSION);
    log::info!("[build] id={BUILD_ID}");
    log::info!("ESP32-S3R2 IoT Gateway starting...");
    log::info!("================================================");

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
                "esp_netif_init failed: esp_err=0x{:08X}",
                ret
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

    // 4.1 RS485 拨码开关读取 (对齐参考固件 RS485_ADDRESS)
    // 使用 ESP-IDF 原生 GPIO API, 不消费 Peripherals 句柄, 可在 Hal::init 后随时调用.
    let dip_addr = rs485::dip::read_dip_address();
    if dip_addr.address > 0 {
        log::warn!(
            "[main] DIP switch: RS485-1 forced to slave addr={} (overrides register config)",
            dip_addr.address
        );
    }

    // 5. 设备协议存储初始化 (含 NVS 加载 + 监听线程)
    log::info!("[main] starting device protocol store...");
    match device::init() {
        Ok(()) => log::info!("[main] device init ok"),
        Err(e) => log::error!(
            "[main] device init failed, falling back to empty ProtoStore: {}",
            e
        ),
    }

    // 5.1 应用 DIP 拨码覆盖 (必须在 device::init 之后, CONFIG RCU 已就绪)
    if dip_addr.address > 0 {
        crate::bus::backends::config_modify(|c| {
            crate::rs485::dip::apply_to_config(dip_addr, c);
        });
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

    // 8.1 ADC 自动校准 (开机 8 秒窗口, 对齐参考固件)
    // 在 IO 任务启动后立即执行, 校准期间阻塞主循环;
    // 校准窗口结束前 IO/AO 任务使用默认标定值 (固定 4-20mA 范围).
    // LOOP9: 校准期间 calib.rs 调用 feed_wdt(), 必须先订阅 WDT,
    //        否则高频报 "task not found" 刷屏 (~150 条/8s)
    health::subscribe_wdt();
    #[cfg(feature = "ai-ao")]
    {
        use crate::channel::calib;
        if let Err(e) = calib::run_auto_calibration(&hal) {
            log::warn!("[main] ADC auto-calibration failed: {}", e);
            // LOOP9: 失败也标记完成, 避免心跳停滞触发强制重启
            calib::mark_task_completed();
        }
    }

    // 8.2 启动 UDP 组播接收 (MCA 一体机分布式资源)
    // 加入保持寄存器 2190-2194 配置的组播组, 接收 32 字节状态数据
    // 写入 INREG_SWITCH_STATUS_BASE (0x0090) 段, 供 Modbus FC=04 读取.
    // 启动失败仅记日志, 不阻断主流程 (与 NTP 等辅助服务一致).
    {
        if let Err(e) = udp_multicast::start() {
            log::warn!("[main] UDP multicast start failed: {}", e);
        }
    }

    // 8.3 启动 NFC ST25DV64KC 配置备份/恢复 (对齐参考固件 RFID_Init)
    // 后台线程每 5s 检测标签, 检测到即执行一次同步 (备份或恢复).
    // 启动失败仅记日志, 不阻断主流程.
    {
        if let Err(e) = nfc::start() {
            log::warn!("[main] NFC ST25DV64KC start failed: {}", e);
        }
    }

    // 8.4 启动 HTTP Web 配置服务器 (端口 80, JSON API)
    // 提供 13 个 REST 路由 (login / getsysteminfo / getiodata / updateota 等).
    // 随机 Cookie 会话认证，密码存储在 NVS (默认 admin/admin123).
    // 启动失败仅记日志, 不阻断主流程.
    {
        if let Err(e) = web::start() {
            log::warn!("[main] HTTP web server start failed: {}", e);
        }
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
    if let Err(e) = protocols.start_all() {
        log::error!(
            "[main] protocol startup incomplete; supervisor will retry: {}",
            e
        );
    }

    // 10. 主循环
    health::register_with_stack(&MAIN_HB, safety::stack_budget::MAIN);
    log::info!(
        "[main] entering main loop (period={}ms)",
        MAIN_LOOP_PERIOD_MS
    );
    // 打印任务-核心分配 (便于验证双核优化)
    health::print_core_assignment();
    if let Err(e) = main_loop(timer_svc, hal.clone(), protocols) {
        log::error!("[main] main loop returned error: {e}");
        // 不再立即重启, 尝试降级运行
        crate::error::recovery::record_failure(
            crate::error::recovery::Severity::Severe,
            "main",
            &format!("main loop exited: {e}"),
        );
        // 进入降级模式
        crate::error::recovery::enter_mode(crate::error::recovery::DegradedMode::Minimal);
        // 短暂等待, 给系统机会继续响应
        std::thread::sleep(Duration::from_secs(1));
        // LOOP14: 喂狗 + 健康检查, 防止 WDT 10s 超时导致系统卡死
        // (原代码不喂狗 → WDT panic → 与注释 "降级运行" 矛盾)
        loop {
            health::feed_wdt();
            let _ = health::check_all();
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// 主循环：周期性更新系统状态、复位计数、喂狗、健康检查
// ----------------------------------------------------------------------------
fn main_loop(
    _timer_svc: EspTaskTimerService,
    hal: Arc<Hal>,
    protocols: protocol::ProtocolRegistry,
) -> AppResult<()> {
    let mut tick: u32 = 0;
    let mut ota_validation_done = false;
    let period = Duration::from_millis(MAIN_LOOP_PERIOD_MS);
    let start = std::time::Instant::now();
    let mut next_tick = start;
    let mut max_work_us = 0u64;
    let mut deadline_miss_count = 0u32;
    let mut perf = MainLoopPerf::new();

    // 把 main 任务加入 ESP-IDF Task Watchdog (10s 超时)
    // LOOP9: main() 中已订阅 (calib 前), 此处不再重复订阅 (避免 "task is already subscribed")

    loop {
        let loop_started = std::time::Instant::now();
        tick = tick.wrapping_add(1);
        MAIN_HB.tick();

        // 每个周期喂狗 (5ms), 远小于 WDT 超时 10s
        health::feed_wdt();

        // 网络请求先于慢速 I2C/ADC 路径执行。这样即使外设异常接近
        // 交易超时边界，已到达的 TCP 请求也不会被压在本轮硬件访问之后。
        #[cfg(feature = "modbus-tcp")]
        perf.measure(0, crate::modbus::tcp_server::tick_tcp_server);

        // 5ms 网络基准调度；AI/AO 分频保持 100ms 周期。
        if tick % (100 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            #[cfg(feature = "ai-ao")]
            perf.measure(1, || {
                crate::channel::ai::tick_ai_sample(&hal);
                crate::channel::ao::tick_ao_output(&hal);
            });
        }

        // DI 仍按 20ms 去抖，避免网络提速后把 I2C 轮询负载放大 4 倍。
        if tick % (20 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            #[cfg(feature = "io-di-do")]
            perf.measure(2, || crate::io::di::tick_di_scan(&hal));
        }

        // DO dirty 检查是单原子操作，每 5ms 运行；只有值变化才访问 I2C。
        // 这使 TCP/RTU/BLE/Web 点位控制到物理输出的额外调度延迟不超过 5ms。
        #[cfg(feature = "io-di-do")]
        perf.measure(3, || crate::io::do_::tick_do_output(&hal));

        // LOOP13: DO NVS 持久化 (1s 节流 + 值去重), 重启后继电器恢复
        #[cfg(feature = "io-di-do")]
        perf.measure(4, || {
            let now_ms = unsafe { esp_idf_sys::esp_timer_get_time() } as u64 / 1000;
            crate::device::persist_do_bits_throttled(now_ms as u32);
        });

        // ETH 心跳 5s，取消独立 pthread。
        if tick % (5000 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            perf.measure(5, crate::ethernet::w5500::tick_eth_heartbeat);
        }

        // BLE 请求与通知按 10ms 调度，避免手持机配置命令额外等待 100ms。
        if tick % (BLE_PROCESS_PERIOD_MS as u32 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            #[cfg(feature = "ble-at")]
            perf.measure(6, ble_at::process_tick);
        }

        // 消费 IO 事件 (避免事件队列满, 触发重置丢失关键状态变化)
        // 阶段 3: DI 变化触发 BLE notify (REPORT_COM_INPUT_IO_STATUS 0x94, Android tx_id=0x01)
        perf.measure(7, || {
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
                    log::info!(
                        "[eth] main loop: IP assigned {}.{}.{}.{}",
                        ip_b[0],
                        ip_b[1],
                        ip_b[2],
                        ip_b[3]
                    );
                    crate::bus::backends::config_modify(|c| {
                        c.ip = ip_b;
                        c.mask = mask_b;
                        c.gateway = gw_b;
                        c.dhcp = true;
                    });
                }
                }
            }
        });

        // 每 1s 更新 uptime + 检查任务心跳
        if tick % (1000 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            #[cfg(debug_assertions)]
            let housekeeping_started = std::time::Instant::now();
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

            // 单个业务任务异常不能中断仍可工作的 IO、BLE 或其他协议。
            let stalled = health::check_all();
            if !stalled.is_empty() {
                log::error!(
                    "[health] stalled tasks: {:?}; keeping gateway online",
                    stalled.as_slice()
                );
                crate::error::recovery::record_failure(
                    crate::error::recovery::Severity::Degradable,
                    "health",
                    "registered task heartbeat stalled",
                );
            }

            // 启动期 pthread/资源短缺不应永久丢失协议。只重试尚未标记运行的
            // 协议；成功任务不会重复创建。每 5 秒一次，避免资源压力下忙重试。
            if uptime % 5 == 0 && !protocols.all_running() {
                if let Err(e) = protocols.start_all() {
                    log::warn!("[supervisor] protocol retry incomplete: {}", e);
                }
            }
            if uptime % 5 == 0 {
                if !udp_multicast::is_started() {
                    if let Err(e) = udp_multicast::start() {
                        log::warn!("[supervisor] UDP task retry failed: {}", e);
                    }
                }
                if !nfc::is_started() {
                    if let Err(e) = nfc::start() {
                        log::warn!("[supervisor] NFC task retry failed: {}", e);
                    }
                }
                if !web::is_started() {
                    if let Err(e) = web::start() {
                        log::warn!("[supervisor] HTTP task retry failed: {}", e);
                    }
                }
            }

            // OTA 镜像必须先完成全部服务启动并稳定运行 30 秒，且没有任务停滞，
            // 才取消 bootloader 回滚。不能在 main 入口就确认，否则“能进 main、
            // 但 TCP/BLE/IO 服务起不来”的坏镜像会失去自动回滚保护。
            if !ota_validation_done
                && uptime >= 30
                && stalled.is_empty()
                && protocols.all_running()
                && udp_multicast::is_started()
                && nfc::is_started()
                && web::is_started()
            {
                ota_validation_done = confirm_new_firmware();
            }

            // LOOP8: 每 60s 打印内存/运行时间。完整栈表每 10 分钟打印，
            // 避免 7 行串口 INFO 每分钟阻塞 main-loop 并制造端口延迟尖峰。
            if tick % (60000 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
                let free_heap = unsafe { esp_idf_sys::esp_get_free_heap_size() };
                let min_heap = unsafe { esp_idf_sys::esp_get_minimum_free_heap_size() };
                let internal_caps =
                    esp_idf_sys::MALLOC_CAP_INTERNAL | esp_idf_sys::MALLOC_CAP_8BIT;
                let internal_free =
                    unsafe { esp_idf_sys::heap_caps_get_free_size(internal_caps) };
                let internal_min =
                    unsafe { esp_idf_sys::heap_caps_get_minimum_free_size(internal_caps) };
                log::info!(
                    "[main] uptime={}s heap={}KB min={}KB internal={}KB internal_min={}KB tick={}",
                    uptime,
                    free_heap / 1024,
                    min_heap / 1024,
                    internal_free / 1024,
                    internal_min / 1024,
                    tick
                );
                if internal_free < 32 * 1024 || internal_min < 32 * 1024 {
                    log::error!(
                        "[mem] LOW INTERNAL SRAM: free={}B min={}B",
                        internal_free,
                        internal_min
                    );
                }
            }
            #[cfg(debug_assertions)]
            let stack_report_period_ms = 60_000;
            #[cfg(not(debug_assertions))]
            let stack_report_period_ms = 600_000;
            if tick % (stack_report_period_ms / MAIN_LOOP_PERIOD_MS as u32) == 0 {
                health::print_stack_watermarks();
            }
            #[cfg(debug_assertions)]
            perf.record_elapsed(8, housekeeping_started);
        }

        let work_us = loop_started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        max_work_us = max_work_us.max(work_us);
        if work_us > MAIN_LOOP_PERIOD_MS * 1_000 {
            deadline_miss_count = deadline_miss_count.saturating_add(1);
        }
        if tick % (60_000 / MAIN_LOOP_PERIOD_MS as u32) == 0 {
            log::info!(
                "[perf] main_loop period={}ms max_work={}us deadline_miss={}",
                MAIN_LOOP_PERIOD_MS,
                max_work_us,
                deadline_miss_count
            );
            perf.log_and_reset();
            max_work_us = 0;
            deadline_miss_count = 0;
        }

        // 以绝对 deadline 调度，避免“业务耗时 + 固定 sleep”使 5ms 周期持续漂移。
        next_tick += period;
        let now = std::time::Instant::now();
        if let Some(remaining) = next_tick.checked_duration_since(now) {
            std::thread::sleep(remaining);
        } else {
            // 本轮超时后从当前时刻重新对齐，不连续追赶导致其它任务饥饿。
            next_tick = now;
        }
    }
}

// ----------------------------------------------------------------------------
// 日志初始化
// ----------------------------------------------------------------------------
fn init_logger() {
    esp_idf_svc::log::EspLogger::initialize_default();
    // 默认 INFO 级别 (运行时可通过 Modbus 寄存器 0x0106 调节)
    log::set_max_level(log::LevelFilter::Info);
    // 安装 Rust panic hook，记录位置；ESP-IDF panic handler/coredump 负责回溯。
    install_panic_hook();
}

/// 安装 Rust panic hook，打印 panic 位置。
///
/// 不在 panic 路径调用 `Backtrace::force_capture`：panic 可能正由栈/堆损坏触发，
/// 此时再次深度走栈和分配内存会掩盖首个故障。ESP-IDF coredump 保留完整上下文。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 打印 panic 位置
        log::error!("========== RUST PANIC ==========");
        log::error!("panic: {}", info);
        if let Some(loc) = info.location() {
            log::error!("  at {}:{}:{}", loc.file(), loc.line(), loc.column());
        }
        log::error!("================================");
        // 调用默认 hook (会触发 ESP-IDF panic handler → 复位)
        default_hook(info);
    }));
    log::debug!("[main] Rust panic hook installed");
}

/// 确认 OTA 新固件运行正常, 取消自动回滚
///
/// ESP-IDF 启用 `CONFIG_APP_ROLLBACK_ENABLE` 后, OTA 升级后的新固件首次启动
/// 状态为 `ESP_OTA_IMG_PENDING_VERIFY`。若 app 不主动调用
/// `esp_ota_mark_app_valid_cancel_rollback` 标记有效, 下次重启会自动回滚到旧固件。
///
/// 本函数仅在主循环稳定运行 30 秒、服务全部启动且无任务停滞后调用。
fn confirm_new_firmware() -> bool {
    // 获取当前运行的 OTA 分区
    let partition = unsafe { esp_idf_sys::esp_ota_get_running_partition() };
    if partition.is_null() {
        log::warn!("[main] OTA: get running partition failed (null), skip confirm");
        return false;
    }

    // ESP-IDF 明确规定 factory 分区不支持 esp_ota_get_state_partition，
    // 返回 ESP_ERR_NOT_SUPPORTED。factory 本身也没有待确认/回滚状态，直接完成检查，
    // 避免健康窗口后每秒重复查询并产生永久告警。
    let subtype = unsafe { (*partition).subtype };
    if !is_ota_app_subtype(subtype) {
        log::debug!("[main] OTA: running factory app, confirmation not required");
        return true;
    }

    // 查询分区状态
    let mut state: esp_idf_sys::esp_ota_img_states_t = 0;
    let ret = unsafe { esp_idf_sys::esp_ota_get_state_partition(partition, &mut state) };
    if ret != esp_idf_sys::ESP_OK {
        log::warn!("[main] OTA: get state failed (err={}), skip confirm", ret);
        return false;
    }

    // 必须使用当前 ESP-IDF 绑定常量，禁止手写枚举数值。IDF v5.5 中
    // PENDING_VERIFY=1、VALID=2；曾误写为 2，导致新镜像永远得不到确认，
    // 下一次复位必然被 bootloader 回滚。
    if !ota_state_needs_confirmation(state) {
        log::debug!(
            "[main] OTA: state={} (not pending verify), skip confirm",
            state
        );
        return true;
    }

    // 标记新固件有效, 取消回滚
    let ret = unsafe { esp_idf_sys::esp_ota_mark_app_valid_cancel_rollback() };
    if ret == esp_idf_sys::ESP_OK {
        log::info!("[main] OTA: 30s stability window passed, firmware confirmed valid");
        true
    } else {
        log::error!(
            "[main] OTA: mark valid failed (err={}), firmware may rollback next reboot",
            ret
        );
        false
    }
}

#[inline]
fn ota_state_needs_confirmation(state: esp_idf_sys::esp_ota_img_states_t) -> bool {
    state == esp_idf_sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY
}

#[inline]
fn is_ota_app_subtype(subtype: esp_idf_sys::esp_partition_subtype_t) -> bool {
    (esp_idf_sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_MIN
        ..esp_idf_sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_MAX)
        .contains(&subtype)
}

#[cfg(test)]
mod tests {
    use super::{is_ota_app_subtype, ota_state_needs_confirmation};

    #[test]
    fn test_ota_pending_verify_uses_esp_idf_enum() {
        assert!(ota_state_needs_confirmation(
            esp_idf_sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY
        ));
        assert!(!ota_state_needs_confirmation(
            esp_idf_sys::esp_ota_img_states_t_ESP_OTA_IMG_VALID
        ));
        assert!(!ota_state_needs_confirmation(
            esp_idf_sys::esp_ota_img_states_t_ESP_OTA_IMG_ABORTED
        ));
    }

    #[test]
    fn test_only_ota_app_subtypes_need_state_lookup() {
        assert!(!is_ota_app_subtype(
            esp_idf_sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_FACTORY
        ));
        assert!(is_ota_app_subtype(
            esp_idf_sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_0
        ));
        assert!(is_ota_app_subtype(
            esp_idf_sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_1
        ));
    }
}
