//! W5500 SPI 以太网驱动接入
//!
//! ESP32-S3 无内置以太网 MAC，外接 WIZnet W5500 (硬wired TCP/IP + 10/100 MAC/PHY)
//! 通过 SPI 接入。W5500 内置 32KB 缓冲区, 8 个硬件 socket, 支持 SPI 最高 80MHz。
//!
//! ESP-IDF 中 W5500 驱动通过 esp_eth 组件接入:
//!   esp_eth_mac_new_w5500 → esp_eth_phy_new_w5500 → esp_eth_driver_install
//! → esp_netif_new(ESP_NETIF_DEFAULT_ETH) + glue → esp_eth_start → IP 事件 watch → 心跳任务
//!
//! 注意: 需在 sdkconfig.defaults 中启用 CONFIG_ETH_SPI_ETHERNET_W5500=y

use core::fmt::Write as _;
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
const HEARTBEAT_MAX_FAIL: u32 = 3;

/// 以太网心跳任务记录 (静态分配, main_loop 监控)
static ETH_HB: TaskHb = TaskHb::new_with_stall("eth-heartbeat", 10);

/// 启动 W5500 以太网。
///
/// SPI2_HOST 总线由本模块独占管理 (W5500 是 SPI 总线上唯一外设)。
/// ETH_RST 引脚复用 `hal.gpio.eth_reset()`，避免与 GPIO 模块重复初始化。
pub fn start(hal: Arc<Hal>, sys_loop: EspSystemEventLoop) -> AppResult<()> {
    let _ = sys_loop;

    log::info!("[eth] initializing W5500 over SPI{}...", pins::ETH_SPI_HOST);

    // 1) 初始化 SPI 总线 (W5500 独占 SPI2_HOST)
    let spi_host = pins::ETH_SPI_HOST as esp_idf_sys::spi_host_device_t;
    let bus_cfg = spi_bus_config_default(pins::ETH_SPI_MOSI, pins::ETH_SPI_MISO, pins::ETH_SPI_SCLK);
    let dma_chan = 1; // ESP32-S3 SPI DMA
    check(unsafe { esp_idf_sys::spi_bus_initialize(spi_host, &bus_cfg, dma_chan) }, "spi_bus_initialize")?;

    // 2) W5500 SPI 设备配置 (ESP-IDF v5.5.2: MAC 驱动内部调用 spi_bus_add_device, 无需手动添加)
    let dev_cfg = spi_device_config_default(pins::ETH_SPI_CS);

    // 3) 硬件复位 W5500 (复用 Hal.gpio 的 ETH_RST 引脚, 避免重复 gpio_config)
    log::info!("[eth] resetting W5500 via GPIO{}...", pins::ETH_RST);
    hal.gpio.eth_reset();

    // 4) 创建 W5500 MAC
    let mac_cfg = eth_mac_config_default();
    let w5500_cfg = eth_w5500_config_default(spi_host, &dev_cfg, pins::ETH_INT);
    let mac = unsafe { esp_idf_sys::esp_eth_mac_new_w5500(&w5500_cfg, &mac_cfg) };
    if mac.is_null() {
        return Err(AppError::Ethernet("esp_eth_mac_new_w5500 returned null".into()));
    }

    // 5) 创建 PHY
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

    // 7) 默认 eth netif + attach glue
    // ESP-IDF v5.5.2: esp_netif_create_default_eth_mac 已移除,
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

    // 8) 启动
    check(unsafe { esp_idf_sys::esp_eth_start(eth_handle) }, "esp_eth_start")?;
    log::info!("[eth] W5500 driver installed and started, eth_handle={:p}", eth_handle);

    // 9) IP 事件 watch
    spawn_ip_watch(eth_handle)?;

    // 10) 心跳任务
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
        max_transfer_sz: 0,
        flags: 0,
        isr_cpu_id: esp_idf_sys::esp_intr_cpu_affinity_t_ESP_INTR_CPU_AFFINITY_AUTO,
        intr_flags: 0,
    }
}

fn spi_device_config_default(cs: u8) -> esp_idf_sys::spi_device_interface_config_t {
    esp_idf_sys::spi_device_interface_config_t {
        // W5500 SPI 帧: 地址段(2B) + 控制段(1B, 可选) + 数据段
        // ESP-IDF W5500 驱动内部用 spi_device_polling_transmit 处理帧格式
        command_bits: 0, address_bits: 0, dummy_bits: 0,
        mode: 0,
        clock_source: esp_idf_sys::soc_periph_spi_clk_src_t_SPI_CLK_SRC_DEFAULT,
        duty_cycle_pos: 0,
        cs_ena_pretrans: 0, cs_ena_posttrans: 0,  // SPI mode 0
        clock_speed_hz: 20_000_000,  // 20MHz (W5500 最高 80MHz, 用 20MHz 保证稳定性)
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
/// ESP-IDF v5.5.2: eth_w5500_config_t 不再接受 spi_device_handle_t,
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
        // W5500 PHY 地址固定为 0 (内部 PHY), -1 表示自动探测
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
    if event_id != esp_idf_sys::ip_event_t_IP_EVENT_ETH_GOT_IP as i32 || event_data.is_null() {
        return;
    }
    unsafe {
        let data = &*(event_data as *const esp_idf_sys::ip_event_got_ip_t);
        let ip = data.ip_info.ip;
        let gw = data.ip_info.gw;
        let mask = data.ip_info.netmask;
        log::info!("[eth] got IP: {} / {} gw {}",
                   fmt_ip(&ip.addr), fmt_ip(&mask.addr), fmt_ip(&gw.addr));
    }
}

fn fmt_ip(addr: &u32) -> heapless::String<15> {
    let mut s = heapless::String::new();
    let _ = write!(s, "{}.{}.{}.{}",
        addr & 0xFF, (addr >> 8) & 0xFF, (addr >> 16) & 0xFF, (addr >> 24) & 0xFF);
    s
}

// ---- 心跳任务：每 5s 检测网关连通性；连续失败 >= 3 次触发 esp_restart ----
fn spawn_heartbeat() -> AppResult<()> {
    health::register(&ETH_HB);
    std::thread::Builder::new()
        .name("eth-heartbeat".into())
        .spawn(move || {
            crate::health::pin_current_to_core(crate::health::CORE_NET);
            let mut fail_count: u32 = 0;
            let period = std::time::Duration::from_secs(HEARTBEAT_PERIOD_S);
            loop {
                // 心跳: 每 5s 上报一次
                ETH_HB.tick();
                if heartbeat_once() {
                    if fail_count > 0 {
                        log::info!("[eth-heartbeat] link restored");
                    }
                    fail_count = 0;
                } else {
                    fail_count = fail_count.saturating_add(1);
                    log::warn!("[eth-heartbeat] gateway unreachable (count={})", fail_count);
                    if fail_count >= HEARTBEAT_MAX_FAIL {
                        log::error!("[eth-heartbeat] max fail reached, restarting");
                        unsafe { esp_idf_sys::esp_restart(); }
                    }
                }
                std::thread::sleep(period);
            }
        })
        .map_err(|e| AppError::Ethernet(format!("spawn heartbeat: {e}")))?;
    log::info!("[eth] heartbeat task started, period={}s", HEARTBEAT_PERIOD_S);
    Ok(())
}

fn heartbeat_once() -> bool {
    // TODO: 实现真正的心跳检测
    true
}

fn check(ret: esp_idf_sys::esp_err_t, ctx: &str) -> AppResult<()> {
    if ret != 0 {
        return Err(AppError::Ethernet(format!("{ctx} failed: esp_err=0x{ret:08X}")));
    }
    Ok(())
}
