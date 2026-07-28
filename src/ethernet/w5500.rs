//! W5500 SPI 以太网驱动接入
//!
//! ESP32-S3 无内置以太网 MAC，外接 WIZnet W5500 (硬wired TCP/IP + 10/100 MAC/PHY)
//! 通过 SPI 接入。W5500 内置 32KB 缓冲区, 8 个硬件 socket, 支持 SPI 最高 80MHz。
//!
//! ESP-IDF 中 W5500 驱动通过 esp_eth 组件接入:
//!   esp_eth_mac_new_w5500 → esp_eth_phy_new_w5500 → esp_eth_driver_install
//! → esp_netif_new(ESP_NETIF_DEFAULT_ETH) + glue → esp_eth_start → IP 事件 watch → 心跳任务
//!
//! 引脚分配 (来源: LILYGO T-ETH-Lite-ESP32-S3 utilities.h):
//!   MISO=GPIO11, MOSI=GPIO12, SCLK=GPIO10, CS=GPIO9, INT=GPIO13, RST=GPIO14
//!   SPI host = SPI3_HOST (ESP-IDF v5.x: SPI3_HOST=2)
//!
//! W5500 SPI 帧格式 (VDM 模式):
//!   [addr_hi, addr_lo, ctrl_byte, data...]
//!   ctrl = (BSB<<3) | (RWB<<2) | OM
//! ESP-IDF SPI 驱动通过 command_bits=16 (地址段) + address_bits=8 (控制段) 硬件处理帧格式
//!
//! 注意: 需在 sdkconfig.defaults 中启用 CONFIG_ETH_SPI_ETHERNET_W5500=y

use core::fmt::Write as _;
use crate::sync::MainLoopCell;
use std::ffi::c_void;
use std::sync::Arc;

use esp_idf_svc::eventloop::EspSystemEventLoop;

use crate::config::pins;
use crate::error::{AppError, AppResult};
use crate::hal::Hal;
use crate::health::{self, TaskHb};

/// 心跳周期 (s)
const HEARTBEAT_PERIOD_S: u64 = 5;
/// 心跳失败上限 (>= 即触发复位)
const HEARTBEAT_MAX_FAIL: u32 = 3; // 旧值, 实际由 recovery 模块分级处理

/// 以太网心跳任务记录 (静态分配, main_loop 监控)
/// 阈值 = 8: eth 每 5s tick 一次, main_loop 每 1s 检查; 允许 8 个检查周期 (8s) 未变化
/// 才报停滞, 留足 3s 余量应对 5s 周期 + 调度抖动, 避免健康检查先于 fail_count 降级逻辑触发重启.
static ETH_HB: TaskHb = TaskHb::new_with_stall("eth-heartbeat", 8);

/// 启动 W5500 以太网。
///
/// SPI3_HOST 总线由本模块独占管理 (W5500 是 SPI 总线上唯一外设)。
/// ETH_RST 引脚复用 `hal.gpio.eth_reset_pulse()`，避免与 GPIO 模块重复初始化。
pub fn start(hal: Arc<Hal>, sys_loop: EspSystemEventLoop) -> AppResult<()> {
    let _ = sys_loop;

    log::info!("[eth] initializing W5500 over SPI{} (MISO={},MOSI={},SCLK={},CS={},INT={},RST={})...",
               pins::ETH_SPI_HOST, pins::ETH_SPI_MISO, pins::ETH_SPI_MOSI,
               pins::ETH_SPI_SCLK, pins::ETH_SPI_CS, pins::ETH_INT, pins::ETH_RST);

    // 0) 安装 GPIO ISR 服务 (W5500 驱动内部调用 gpio_isr_handler_add 注册 INT 引脚中断)
    //    必须在 esp_eth_driver_install 之前调用, 否则 W5500 复位会超时
    //    若已安装返回 ESP_ERR_INVALID_STATE (0x103), 忽略即可
    let isr_ret = unsafe { esp_idf_sys::gpio_install_isr_service(0) };
    if isr_ret != esp_idf_sys::ESP_OK && isr_ret != esp_idf_sys::ESP_ERR_INVALID_STATE {
        return Err(AppError::Ethernet(format!(
            "gpio_install_isr_service failed: esp_err=0x{:08X}", isr_ret
        )));
    }
    log::debug!("[eth] GPIO ISR service ready (ret={})", isr_ret);

    // 1) 硬件复位 W5500 (复用 Hal.gpio 的 ETH_RST 引脚, 避免重复 gpio_config)
    //    序列: HIGH(250ms) → LOW(50ms) → HIGH(350ms), 来自参考固件 ETHClass.cpp
    log::info!("[eth] resetting W5500 via GPIO{}...", pins::ETH_RST);
    hal.gpio.eth_reset_pulse();

    // 2) 初始化 SPI 总线 (W5500 独占 SPI3_HOST)
    //    DMA: SPI_DMA_CH_AUTO (ESP-IDF 自动分配 DMA 通道)
    let spi_host = pins::ETH_SPI_HOST as esp_idf_sys::spi_host_device_t;
    let bus_cfg = spi_bus_config_default(pins::ETH_SPI_MOSI, pins::ETH_SPI_MISO, pins::ETH_SPI_SCLK);
    let dma_chan = esp_idf_sys::spi_common_dma_t_SPI_DMA_CH_AUTO;
    check(unsafe { esp_idf_sys::spi_bus_initialize(spi_host, &bus_cfg, dma_chan) }, "spi_bus_initialize")?;
    log::info!("[eth] SPI bus initialized (host={}, DMA=auto)", spi_host);

    // 3) W5500 SPI 设备配置
    //    ESP-IDF v5.5.4: MAC 驱动内部调用 spi_bus_add_device, 无需手动添加
    //    command_bits=16: W5500 SPI 帧的地址段 (2 字节)
    //    address_bits=8:  W5500 SPI 帧的控制段 (1 字节)
    //    这样 ESP-IDF SPI 驱动硬件处理 W5500 帧格式, MAC 驱动只需发送数据
    let dev_cfg = spi_device_config_default(pins::ETH_SPI_CS);

    // 4) 创建 W5500 MAC
    let mac_cfg = eth_mac_config_default();
    let w5500_cfg = eth_w5500_config_default(spi_host, &dev_cfg, pins::ETH_INT);
    let mac = unsafe { esp_idf_sys::esp_eth_mac_new_w5500(&w5500_cfg, &mac_cfg) };
    if mac.is_null() {
        return Err(AppError::Ethernet("esp_eth_mac_new_w5500 returned null".into()));
    }

    // 5) 创建 PHY
    //    reset_gpio_num=-1: RST 由 hal.gpio.eth_reset_pulse() 手动管理, 不让 PHY 驱动重复控制
    let phy_cfg = eth_phy_config_default();
    let phy = unsafe { esp_idf_sys::esp_eth_phy_new_w5500(&phy_cfg) };
    if phy.is_null() {
        return Err(AppError::Ethernet("esp_eth_phy_new_w5500 returned null".into()));
    }

    // 6) 安装驱动
    let eth_cfg = eth_config_default(mac, phy);
    let mut eth_handle: esp_idf_sys::esp_eth_handle_t = std::ptr::null_mut();
    check(unsafe { esp_idf_sys::esp_eth_driver_install(&eth_cfg, &mut eth_handle as *mut _) },
          "esp_eth_driver_install")?;

    // 6.5) 设置 MAC 地址 (W5500 无预置 MAC, 需从 ESP32 eFuse 读取后写入)
    {
        // 阶段 A: 从 CONFIG RCU 无锁读 eth_mac; 缺省 DHCP fallback
        let mac: [u8; 6] = crate::bus::config_read()
            .map(|cs| cs.cfg.eth_mac)
            .unwrap_or([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        let mac_bytes = mac;
        if mac_bytes != [0u8; 6] && mac_bytes != [0x02, 0x00, 0x00, 0x00, 0x00, 0x01] {
            log::info!(
                "[eth] setting MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                mac_bytes[0], mac_bytes[1], mac_bytes[2],
                mac_bytes[3], mac_bytes[4], mac_bytes[5]
            );
        }
        check(
            unsafe {
                esp_idf_sys::esp_eth_ioctl(
                    eth_handle,
                    esp_idf_sys::esp_eth_io_cmd_t_ETH_CMD_S_MAC_ADDR,
                    mac_bytes.as_ptr() as *mut _,
                )
            },
            "esp_eth_ioctl(S_MAC_ADDR)",
        )?;
    }

    // 7) 默认 eth netif + attach glue
    // ESP-IDF v5.5.4: esp_netif_create_default_eth_mac 已移除,
    // 改用 esp_netif_new + ESP_NETIF_DEFAULT_ETH 模式 (base+stack, driver=NULL)
    let netif_cfg = esp_idf_sys::esp_netif_config {
        base: unsafe { &esp_idf_sys::_g_esp_netif_inherent_eth_config } as *const _,
        driver: std::ptr::null(),
        stack: unsafe { esp_idf_sys::_g_esp_netif_netstack_default_eth },
    };
    let netif = unsafe { esp_idf_sys::esp_netif_new(&netif_cfg) };
    if netif.is_null() {
        return Err(AppError::Ethernet("esp_netif_new returned null".into()));
    }
    let glue = unsafe { esp_idf_sys::esp_eth_new_netif_glue(eth_handle) };
    check(unsafe { esp_idf_sys::esp_netif_attach(netif, glue as *mut c_void) }, "esp_netif_attach")?;

    // 7.5) 配置网络: 读取 SystemConfig, 禁用 DHCP → 设置静态 IP
    apply_netif_config(netif)?;

    // 8) 启动
    check(unsafe { esp_idf_sys::esp_eth_start(eth_handle) }, "esp_eth_start")?;
    log::info!("[eth] W5500 driver installed and started, eth_handle={:p}", eth_handle);

    // 9) IP 事件 watch
    spawn_ip_watch(eth_handle)?;

    // 10) 链路事件 watch — LOOP12: 实时检测拔线 (DHCP lease 过期前数小时内 IP 不归零)
    spawn_eth_link_watch(eth_handle)?;

    // 11) 心跳任务
    spawn_heartbeat()?;

    log::info!("[eth] W5500 startup complete");
    Ok(())
}

// ---- 默认配置构造 ----
fn spi_bus_config_default(mosi: u8, miso: u8, sclk: u8) -> esp_idf_sys::spi_bus_config_t {
    esp_idf_sys::spi_bus_config_t {
        __bindgen_anon_1: esp_idf_sys::spi_bus_config_t__bindgen_ty_1 {
            mosi_io_num: mosi as i32,
        },
        __bindgen_anon_2: esp_idf_sys::spi_bus_config_t__bindgen_ty_2 {
            miso_io_num: miso as i32,
        },
        sclk_io_num: sclk as i32,
        __bindgen_anon_3: esp_idf_sys::spi_bus_config_t__bindgen_ty_3 {
            quadwp_io_num: -1,
        },
        __bindgen_anon_4: esp_idf_sys::spi_bus_config_t__bindgen_ty_4 {
            quadhd_io_num: -1,
        },
        data4_io_num: -1, data5_io_num: -1, data6_io_num: -1, data7_io_num: -1,
        data_io_default_level: false,
        // W5500 最大以太网帧约 1536B, 加 3B SPI header 留 1600B 足够.
        // LOOP20: max_transfer_sz 决定 SPI DMA descriptor pool 大小
        // (dma_desc_ct = ceil(max_transfer_sz / 4092)). 1 个 descriptor 可传 4092B,
        // 但 ESP-IDF W5500 驱动每次 w5500_spi_write/read 的 trans.length = 8*len,
        // 其中 len = 实际帧字节数. 1536B 帧 → 1 个 DMA desc 足够.
        // 设为 1600 时: 1 个 DMA desc = 4092B, descriptor pool 仅 1 组,
        // 每次 SPI 事务的 DMA priv buffer 分配压力最低 (~1.5KB 而非 ~8KB).
        // 之前 2048/4096 的 max_transfer_sz 导致 pool 中预分配 2 个 desc → 每个挂起
        // 事务的 priv buffer 依次翻倍, BLE/Bluedroid 同时运行时容易耗尽 internal SRAM.
        max_transfer_sz: 1600,
        flags: 0,
        isr_cpu_id: esp_idf_sys::esp_intr_cpu_affinity_t_ESP_INTR_CPU_AFFINITY_AUTO,
        intr_flags: 0,
    }
}

fn spi_device_config_default(cs: u8) -> esp_idf_sys::spi_device_interface_config_t {
    esp_idf_sys::spi_device_interface_config_t {
        // W5500 SPI 帧: 地址段(2B) + 控制段(1B) + 数据段
        // command_bits=16: SPI 硬件处理地址段 (2 字节)
        // address_bits=8:  SPI 硬件处理控制段 (1 字节)
        // MAC 驱动只需发送数据段, 帧格式由 SPI 硬件自动组装
        command_bits: 16,
        address_bits: 8,
        dummy_bits: 0,
        mode: 0,
        clock_source: esp_idf_sys::soc_periph_spi_clk_src_t_SPI_CLK_SRC_DEFAULT,
        duty_cycle_pos: 0,
        cs_ena_pretrans: 0, cs_ena_posttrans: 0,  // SPI mode 0
        clock_speed_hz: 40_000_000,  // 40MHz (W5500 最高 80MHz, 40MHz 兼顾速度与稳定性)
        input_delay_ns: 0,
        sample_point: 0,
        spics_io_num: cs as i32,
        flags: 0, queue_size: 20, pre_cb: None, post_cb: None,
    }
}

fn eth_mac_config_default() -> esp_idf_sys::eth_mac_config_t {
    esp_idf_sys::eth_mac_config_t {
        rx_task_stack_size: 4096, rx_task_prio: 15, sw_reset_timeout_ms: 100, flags: 0,
    }
}

/// W5500 配置: SPI 主机 + 设备配置 + 中断引脚
/// ESP-IDF v5.5.4: eth_w5500_config_t 不再接受 spi_device_handle_t,
/// 而是接受 spi_host_id + spi_devcfg 指针, MAC 驱动内部调用 spi_bus_add_device
fn eth_w5500_config_default(spi_host: esp_idf_sys::spi_host_device_t,
                             dev_cfg: &esp_idf_sys::spi_device_interface_config_t,
                             int_gpio: u8) -> esp_idf_sys::eth_w5500_config_t {
    esp_idf_sys::eth_w5500_config_t {
        int_gpio_num: int_gpio as i32,
        poll_period_ms: 0,
        spi_host_id: spi_host,
        spi_devcfg: dev_cfg as *const _ as *mut _,
        custom_spi_driver: esp_idf_sys::eth_spi_custom_driver_config_t {
            config: std::ptr::null_mut(),
            init: None,
            deinit: None,
            read: None,
            write: None,
        },
    }
}

fn eth_phy_config_default() -> esp_idf_sys::eth_phy_config_t {
    esp_idf_sys::eth_phy_config_t {
        // W5500 PHY 地址固定为 0 (内部 PHY)
        // reset_gpio_num=-1: RST 由 hal.gpio.eth_reset_pulse() 手动管理
        phy_addr: 0, reset_timeout_ms: 100, autonego_timeout_ms: 4000, reset_gpio_num: -1,
        hw_reset_assert_time_us: 0, post_hw_reset_delay_ms: 0,
    }
}

fn eth_config_default(mac: *mut esp_idf_sys::esp_eth_mac_t,
                      phy: *mut esp_idf_sys::esp_eth_phy_t) -> esp_idf_sys::esp_eth_config_t {
    esp_idf_sys::esp_eth_config_t {
        mac, phy, check_link_period_ms: 2000,
        stack_input: None, stack_input_info: None,
        on_lowlevel_init_done: None, on_lowlevel_deinit_done: None,
        read_phy_reg: None, write_phy_reg: None,
    }
}

// ---- IP 事件 watch ----
fn spawn_ip_watch(eth_handle: esp_idf_sys::esp_eth_handle_t) -> AppResult<()> {
    let base = unsafe { esp_idf_sys::IP_EVENT };
    check(unsafe {
        esp_idf_sys::esp_event_handler_register(
            base, esp_idf_sys::ip_event_t_IP_EVENT_ETH_GOT_IP as i32, Some(ip_event_cb),
            eth_handle as *mut c_void)
    }, "esp_event_handler_register(IP_EVENT_ETH_GOT_IP)")?;
    log::info!("[eth] IP_EVENT_ETH_GOT_IP handler registered");
    Ok(())
}

extern "C" fn ip_event_cb(
    _arg: *mut c_void,
    _event_base: esp_idf_sys::esp_event_base_t,
    event_id: i32,
    event_data: *mut c_void,
) {
    // LOOP18: 此回调运行在 sys_evt 任务栈 (sdkconfig: ESP_SYSTEM_EVENT_TASK_STACK_SIZE)
    // 1. 不要在这里调用 log::info! — 虽然 ESP-IDF log 实现使用静态缓冲,
    //    但 Rust log crate 的 log::Log::log() 方法会构造 core::fmt::Arguments
    //    (~200B) 与 log::Record (~150B), 在 2KB 任务栈上累积易触发 Stack canary.
    // 2. 不要 RCU 读 / 字节转换 — 推迟到 main_loop 处理.
    // 只做一件事: 把 3 个 IPv4 地址打包成 12 字节, 通过 MpscRing 无锁入队.
    if event_id != esp_idf_sys::ip_event_t_IP_EVENT_ETH_GOT_IP as i32 || event_data.is_null() {
        return;
    }
    unsafe {
        let data = &*(event_data as *const esp_idf_sys::ip_event_got_ip_t);
        let ip_raw = data.ip_info.ip.addr;
        let mask_raw = data.ip_info.netmask.addr;
        let gw_raw = data.ip_info.gw.addr;
        crate::bus::send_event(crate::bus::IoEvent::IpAssigned(
            [
                ip_raw as u8, (ip_raw >> 8) as u8, (ip_raw >> 16) as u8, (ip_raw >> 24) as u8,
            ],
            [
                mask_raw as u8, (mask_raw >> 8) as u8, (mask_raw >> 16) as u8, (mask_raw >> 24) as u8,
            ],
            [
                gw_raw as u8, (gw_raw >> 8) as u8, (gw_raw >> 16) as u8, (gw_raw >> 24) as u8,
            ],
        ));
    }
}

fn fmt_ip(addr: &u32) -> heapless::String<15> {
    let mut s = heapless::String::new();
    let _ = write!(s, "{}.{}.{}.{}",
        addr & 0xFF, (addr >> 8) & 0xFF, (addr >> 16) & 0xFF, (addr >> 24) & 0xFF);
    s
}

// ---- ETH 链路事件 watch ----
//
// LOOP12: 原 heartbeat_once() 仅查 `ip != 0`, 但 LwIP 在 DHCP lease 到期前 (默认数小时)
// 都保留非零 IP, 拔线后检测延迟可达数小时. 改为订阅 ETH_EVENT 的 CONNECTED/DISCONNECTED,
// 用 AtomicBool 缓存链路状态; heartbeat_once 顶部优先查 link, link down 直接 false.

use std::sync::atomic::{AtomicBool, Ordering};

/// 链路状态缓存 (true = 已连接). 由 eth_event_cb 维护, heartbeat_once 读取.
/// 启动时假定为 true, 避免首次 IP 分配前误报 fail (后续事件会修正).
static ETH_LINK_UP: AtomicBool = AtomicBool::new(true);

fn spawn_eth_link_watch(_eth_handle: esp_idf_sys::esp_eth_handle_t) -> AppResult<()> {
    let base = unsafe { esp_idf_sys::ETH_EVENT };
    check(unsafe {
        esp_idf_sys::esp_event_handler_register(
            base,
            esp_idf_sys::eth_event_t_ETHERNET_EVENT_CONNECTED as i32,
            Some(eth_event_cb),
            std::ptr::null_mut(),
        )
    }, "esp_event_handler_register(ETHERNET_EVENT_CONNECTED)")?;
    check(unsafe {
        esp_idf_sys::esp_event_handler_register(
            base,
            esp_idf_sys::eth_event_t_ETHERNET_EVENT_DISCONNECTED as i32,
            Some(eth_event_cb),
            std::ptr::null_mut(),
        )
    }, "esp_event_handler_register(ETHERNET_EVENT_DISCONNECTED)")?;
    log::info!("[eth] ETH_EVENT CONNECTED/DISCONNECTED handlers registered");
    Ok(())
}

extern "C" fn eth_event_cb(
    _arg: *mut c_void,
    _event_base: esp_idf_sys::esp_event_base_t,
    event_id: i32,
    _event_data: *mut c_void,
) {
    // LOOP18: 此回调运行在 sys_evt 任务栈 (sdkconfig: ESP_SYSTEM_EVENT_TASK_STACK_SIZE)
    // 只做一件事: 翻转 AtomicBool, 不调 log (log 会构造 ~200B Arguments 在栈上).
    // 链路状态变化在 main_loop 的 heartbeat tick 中记录日志.
    let connected = event_id == esp_idf_sys::eth_event_t_ETHERNET_EVENT_CONNECTED as i32;
    let disconnected = event_id == esp_idf_sys::eth_event_t_ETHERNET_EVENT_DISCONNECTED as i32;
    if connected {
        ETH_LINK_UP.store(true, Ordering::Release);
    } else if disconnected {
        ETH_LINK_UP.store(false, Ordering::Release);
    }
}

// ---- 心跳任务：每 5s 检测网关连通性；连续失败 >= 3 次触发 esp_restart ----
// 架构改造 Phase 2: eth-heartbeat 合并到 main_loop
// 状态用 MainLoopCell 保护 (单线程访问, 零开销)
struct EthHbState {
    fail_count: u32,
}

static ETH_HB_STATE: MainLoopCell<EthHbState> = MainLoopCell::new();

fn spawn_heartbeat() -> AppResult<()> {
    health::register(&ETH_HB);
    ETH_HB_STATE.init(EthHbState { fail_count: 0 });
    log::info!("[eth] heartbeat registered in main_loop (period={}s)", HEARTBEAT_PERIOD_S);
    Ok(())
}

/// 当前 PHY 链路状态，供 Web/诊断页面读取.
pub fn link_up() -> bool {
    ETH_LINK_UP.load(Ordering::Acquire)
}

/// main_loop 每 5s 调用一次
pub fn tick_eth_heartbeat() {
    let state = match ETH_HB_STATE.get_mut() {
        Some(s) => s,
        None => return,
    };

    ETH_HB.tick();
    if heartbeat_once() {
        if state.fail_count > 0 {
            log::info!("[eth-heartbeat] link restored");
            crate::error::recovery::enter_mode(
                crate::error::recovery::DegradedMode::Normal
            );
        }
        state.fail_count = 0;
    } else {
        state.fail_count = state.fail_count.saturating_add(1);
        log::warn!("[eth-heartbeat] gateway unreachable (count={})", state.fail_count);
        if state.fail_count >= 2 {
            let action = crate::error::recovery::decide_action(
                crate::error::recovery::Severity::Degradable,
                state.fail_count,
            );
            crate::error::recovery::apply_action(action, "eth-heartbeat");
        } else {
            crate::error::recovery::record_failure(
                crate::error::recovery::Severity::Recoverable,
                "eth-heartbeat",
                "gateway ping failed",
            );
        }
    }
}

/// LOOP8: 真实心跳检测 — 检查以太网 netif 是否有 IP 地址
///
/// LOOP12: 优先检查 PHY 链路状态 (ETH_EVENT_DISCONNECTED 触发后立即报 failure),
/// 再回退到 IP 检测; 两者任一为 false 均报告链路故障.
///
/// 注意: DHCP lease 过期前 (默认数小时) LwIP 仍返回非零 IP,
/// 故仅查 `ip != 0` 无法及时检测拔线; 现在 PHY 层事件才是主要信号.
fn heartbeat_once() -> bool {
    // 优先查 PHY 链路状态 — ETH_EVENT_DISCONNECTED → false (拔线秒级检测)
    if !ETH_LINK_UP.load(Ordering::Acquire) {
        log::debug!("[eth-heartbeat] link DOWN (PHY disconnected)");
        return false;
    }

    let netif = unsafe { esp_idf_sys::esp_netif_get_handle_from_ifkey(b"ETH_DEF\x00".as_ptr()) };
    if netif.is_null() {
        log::debug!("[eth-heartbeat] netif not found");
        return false;
    }
    let mut ip_info: esp_idf_sys::esp_netif_ip_info_t = unsafe { std::mem::zeroed() };
    let ret = unsafe { esp_idf_sys::esp_netif_get_ip_info(netif, &mut ip_info) };
    if ret != 0 {
        log::debug!("[eth-heartbeat] get_ip_info failed: 0x{:x}", ret);
        return false;
    }
    let ip = ip_info.ip.addr;
    if ip == 0 {
        log::debug!("[eth-heartbeat] no IP assigned (link up but DHCP pending)");
        return false;
    }
    true
}

/// 从 SystemConfig 读取网络配置并应用到 netif
///
/// - dhcp=false → 停止 DHCP client, 设置静态 IP/掩码/网关/DNS
/// - dhcp=true  → 保持 DHCP 客户端 (默认行为)
fn apply_netif_config(netif: *mut esp_idf_sys::esp_netif_obj) -> AppResult<()> {
    // 读取 SystemConfig
    let cfg = crate::bus::config_state::config_read()
        .map(|cs| cs.cfg.clone())
        .unwrap_or_else(|| {
            log::warn!("[eth] CONFIG RCU unavailable, using built-in defaults");
            let mut c = crate::device::SystemConfig::defaults();
            c.dhcp = false;
            c
        });

    log::info!(
        "[eth] network config: dhcp={} ip={} mask={} gw={} dns={}",
        cfg.dhcp,
        cfg.ip_str(),
        cfg.mask_str(),
        cfg.gw_str(),
        cfg.dns_str(),
    );

    if cfg.dhcp {
        log::info!("[eth] DHCP mode, waiting for lease...");
        return Ok(());
    }

    // 静态 IP 模式: 停止 DHCP client
    unsafe { esp_idf_sys::esp_netif_dhcpc_stop(netif) };

    // 构造 esp_netif_ip_info_t
    fn to_ip4(b: [u8; 4]) -> esp_idf_sys::esp_ip4_addr_t {
        esp_idf_sys::esp_ip4_addr_t {
            addr: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        }
    }
    let ip_info = esp_idf_sys::esp_netif_ip_info_t {
        ip: to_ip4(cfg.ip),
        netmask: to_ip4(cfg.mask),
        gw: to_ip4(cfg.gateway),
    };
    check(
        unsafe { esp_idf_sys::esp_netif_set_ip_info(netif, &ip_info) },
        "esp_netif_set_ip_info",
    )?;

    // 设置 DNS
    let mut dns_main: esp_idf_sys::esp_netif_dns_info_t = Default::default();
    dns_main.ip.u_addr.ip4 = to_ip4(cfg.dns);
    unsafe {
        esp_idf_sys::esp_netif_set_dns_info(
            netif,
            esp_idf_sys::esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
            &mut dns_main as *mut _,
        );
    }

    log::info!(
        "[eth] static IP configured: {} / {} gw {} dns {}",
        cfg.ip_str(),
        cfg.mask_str(),
        cfg.gw_str(),
        cfg.dns_str(),
    );

    Ok(())
}

fn check(ret: esp_idf_sys::esp_err_t, ctx: &str) -> AppResult<()> {
    if ret != 0 {
        return Err(AppError::Ethernet(format!("{ctx} failed: esp_err=0x{ret:08X}")));
    }
    Ok(())
}
