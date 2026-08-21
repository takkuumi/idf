//! 全局硬件/应用配置常量
//!
//! 所有引脚分配、外设编号、采样周期统一在此处定义，
//! 便于硬件改版时一次修改、全局生效。
//!
//! 硬件平台：ESP32-S3R2 (Xtensa LX7 双核 240MHz, 512KB SRAM, 2MB Quad PSRAM)
//!   - 内置 Wi-Fi 802.11 b/g/n + BLE 5.0 + Bluetooth Mesh
//!   - 3 个 UART (UART0/1/2), 4 个 SPI (SPI0/1 Flash/PSRAM, SPI2/3 外设)
//!   - 2 个 ADC (ADC1: 10 通道, ADC2: 10 通道, 12-bit)
//!   - 8 通道 LEDC PWM
//!   - 45 个 GPIO (GPIO0~GPIO48), GPIO26~31 被 Quad SPI Flash/PSRAM 占用
//!
//! 实际硬件改版只需修改本文件。

// ----------------------------------------------------------------------------
// 系统参数
// ----------------------------------------------------------------------------
pub const APP_NAME: &str = "esp32s3-iot-gateway";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 主循环网络调度周期 (ms)。
///
/// TCP socket 是非阻塞状态机，5ms tick 把单请求额外调度延迟从
/// 0..20ms 收紧到 0..5ms。DI/DO、AI/AO 和 BLE 在 main_loop 中继续按各自
/// 分频周期执行，不因网络提速而增加硬件总线负载。
pub const MAIN_LOOP_PERIOD_MS: u64 = 5;
/// BLE 请求处理和 notification 发送周期。贴近常见 7.5ms BLE 连接间隔，
/// 同时保留 main_loop 的其他实时任务预算。
pub const BLE_PROCESS_PERIOD_MS: u64 = 10;

// ----------------------------------------------------------------------------
// GPIO 引脚分配 (ESP32-S3R2)
// ESP32-S3 共 45 个 GPIO (GPIO0~GPIO48)
// GPIO26~31: 被 Quad SPI Flash/PSRAM 占用 (2MB PSRAM), 不可用
// GPIO0:    strapping (boot mode, 需外部上拉)
// GPIO3:    strapping (JTAG source)
// GPIO45/46: strapping (VDD_SPI / system freq)
// GPIO43/44: UART0 默认 TX/RX (下载/日志)
//
// 引脚分配来源: LILYGO T-ETH-Lite-ESP32-S3 原理图 + 参考固件 utilities.h
// ----------------------------------------------------------------------------
pub mod pins {
    // ---- 以太网 W5500 (SPI3_HOST) ----
    // W5500: 硬件 TCP/IP 以太网控制器, SPI 接口, 内置 32KB 缓冲, 8 socket
    // SPI mode 0, 最高 80MHz (实际用 20MHz 保证稳定性)
    // 引脚来自 LILYGO T-ETH-Lite-ESP32-S3 utilities.h (LILYGO_T_ETH_LITE_ESP32S3)
    pub const ETH_SPI_HOST: u8 = 2; // SPI3_HOST (ESP-IDF v5.x: SPI2_HOST=1, SPI3_HOST=2)
    pub const ETH_SPI_MISO: u8 = 11;
    pub const ETH_SPI_MOSI: u8 = 12;
    pub const ETH_SPI_SCLK: u8 = 10;
    pub const ETH_SPI_CS: u8 = 9;
    pub const ETH_INT: u8 = 13; // W5500 INT 引脚, 低有效
    pub const ETH_RST: u8 = 14; // W5500 RST 引脚, 低有效

    // ---- RS485 #0 (Modbus RTU, UART1) ----
    // 引脚来自参考固件 MCA_F16V2_1_F48_BLE.ino: rs485_1.begin(baud, cfg, 46, 45)
    // Arduino begin 参数顺序: (baud, config, RX, TX) → RX=46, TX=45
    pub const RS485_0_UART: u8 = 1; // UART1
    pub const RS485_0_TX: u8 = 45;
    pub const RS485_0_RX: u8 = 46;
    pub const RS485_0_DE: u8 = 7; // DE/RE 共控 (高=发送, 低=接收)

    // ---- RS485 #1 (Modbus RTU, UART2) ----
    // 引脚来自参考固件: rs485_2.begin(baud, cfg, 41, 42) → RX=41, TX=42
    pub const RS485_1_UART: u8 = 2; // UART2
    pub const RS485_1_TX: u8 = 42;
    pub const RS485_1_RX: u8 = 41;
    pub const RS485_1_DE: u8 = 8;

    // ---- RS485 #2 (第 3 端口, UART0, 对齐参考固件 RS485-3) ----
    // 参考固件: rs485_3 使用 Serial (UART0), 默认 9600 8N1.
    // 注意: UART0 与 USB CDC/JTAG 控制台复用, 启用 RS485-2 将失去调试串口.
    // 默认不启动; 如需使用, 在 config::modbus::rtu_port2::ENABLED 设为 true.
    // 引脚: UART0 默认 TX=GPIO43, RX=GPIO44, 无硬件 DE 控制 (软件模拟或外接 MAX485)
    pub const RS485_2_UART: u8 = 0; // UART0 (与 USB CDC 复用)
    pub const RS485_2_TX: u8 = 43;
    pub const RS485_2_RX: u8 = 44;
    pub const RS485_2_DE: u8 = 255; // 255 = 无 DE 引脚 (不使用 RTS 自动控制)

    // ---- 电源使能引脚 (开机时需拉高) ----
    // GPIO21: 灯板电源使能 (HIGH=上电)
    // GPIO33: 继电器板 JDQ_24V_EN (HIGH=上电)
    pub const POWER_LED_EN: u8 = 21;
    pub const POWER_RELAY_EN: u8 = 33;

    // ---- RS485 地址码输入 (拨码开关, 启动时读取) ----
    // 参考固件: RS485_AD0=digitalRead(19), AD1=20, AD2=48, AD3=47, ESP_STOP=34
    pub const RS485_ADDR_PINS: [u8; 4] = [19, 20, 48, 47];
    pub const ESP_STOP_PIN: u8 = 34;

    // ---- NCA9555 (PCA9555) IO 扩展芯片 (软件 I2C) ----
    // 参考固件 nca9555.h: DI/DO 全部通过 PCA9555 扩展, 不使用 ESP32 GPIO 直驱
    // IIC_SCL=36, IIC_SDA=35, IIC_LED_SCL=37, IIC_LED_SDA=38, INT=34
    pub const NCA9555_IIC_SCL: u8 = 36;
    pub const NCA9555_IIC_SDA: u8 = 35;
    pub const NCA9555_LED_SCL: u8 = 37;
    pub const NCA9555_LED_SDA: u8 = 38;
    pub const NCA9555_INT: u8 = 34; // 与 ESP_STOP_PIN 共用 GPIO34

    // ---- 光纤检测输入 ----
    // 参考固件: FIB1=digitalRead(39), FIB2=digitalRead(40)
    pub const FIB1_PIN: u8 = 39;
    pub const FIB2_PIN: u8 = 40;

    // ---- 数字输入 DI (8 路) ----
    // TODO: 实际硬件 DI 通过 PCA9555 (NCA9555) I2C 扩展, 不使用 ESP32 GPIO 直驱
    // 当前为占位, 避免与 W5500/RS485/电源/NCA9555 引脚冲突
    // 可用空闲 GPIO: 7(DE0), 8(DE1), 15, 16, 17, 18 (仅 6 个, 不足 8DI)
    pub const DI_PINS: [u8; 8] = [15, 16, 17, 18, 7, 8, 15, 16];

    // ---- 数字输出 DO (8 路) ----
    // TODO: 实际硬件 DO 通过 PCA9555 (NCA9555) I2C 扩展
    // 当前为占位, 实际不可用 (ESP32-S3 上无足够空闲 GPIO 给 8DO)
    pub const DO_PINS: [u8; 8] = [15, 16, 17, 18, 7, 8, 15, 16];

    // ---- AI 模拟输入 (ADC1, 6 通道, 12-bit SAR ADC) ----
    // ADC1_CH0-5 = GPIO1-6, 不与 UART1/UART2 冲突
    pub const AI_ADC_UNIT: u8 = 1; // ADC1
    pub const AI_CHANNELS: [u8; 6] = [0, 1, 2, 3, 4, 5];

    // ---- AO 模拟输出 (LEDC PWM, 4 通道) ----
    // 使用空闲 GPIO: 15, 16, 17, 18 (不与 W5500/RS485/NCA9555 冲突)
    pub const AO_CHANNELS: [(u8, u8); 4] = [
        (0, 15), // (ledc_channel, gpio)
        (1, 16),
        (2, 17),
        (3, 18),
    ];
    pub const AO_FREQ_HZ: u32 = 5000; // PWM 频率，0-10V 模拟输出经 RC 滤波
    pub const AO_RESOLUTION_BITS: u8 = 12; // 12-bit 与 ADC 对齐

    // ---- I2C 总线 (F3/F4 用, 接 MCP23017 IO 扩展) ----
    // 默认版本不用 I2C, DI/DO 走 GPIO 直驱
    pub const I2C_PORT: u8 = 0; // I2C0
    pub const I2C_SDA: u8 = 21; // 默认版本下也是普通 GPIO, 无副作用
    pub const I2C_SCL: u8 = 33;
    pub const I2C_FREQ_HZ: u32 = 400_000; // 400kHz Fast Mode (MCP23017 支持到 1.7MHz)
}

// ----------------------------------------------------------------------------
// 硬件版本 (F3 / F4 / Default)
// ----------------------------------------------------------------------------
// 编译期 feature flag 切换, 不支持运行时切换
// - Default: 8 DI + 8 DO (GPIO 直驱)
// - F3:      16 DI + 16 DO (2x MCP23017, I2C 扩展)
// - F4:      48 DI + 16 DO (4x MCP23017, I2C 扩展)
// ----------------------------------------------------------------------------
pub mod hw_version {
    /// 版本名 (用于日志和 AT+VERSION 响应)
    #[cfg(feature = "f3")]
    pub const NAME: &str = "F3";
    #[cfg(feature = "f4")]
    pub const NAME: &str = "F4";
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const NAME: &str = "F16";

    /// DI 通道数
    #[cfg(feature = "f3")]
    pub const DI_COUNT: usize = 16;
    #[cfg(feature = "f4")]
    pub const DI_COUNT: usize = 48;
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const DI_COUNT: usize = 16;

    /// DO 通道数
    /// - F3: 16 DO (1 片 MCP23017)
    /// - F4: 48 DO (3 片 MCP23017, 每片 16 DO)
    /// - 默认: 16 DO (GPIO 直驱或 1 片 PCA9555)
    #[cfg(feature = "f3")]
    pub const DO_COUNT: usize = 16;
    #[cfg(feature = "f4")]
    pub const DO_COUNT: usize = 48;
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const DO_COUNT: usize = 16;

    /// 是否使用 I2C IO 扩展 (F3/F4)
    #[cfg(any(feature = "f3", feature = "f4"))]
    pub const USE_IO_EXT: bool = true;
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const USE_IO_EXT: bool = false;

    /// DI 扩展芯片数量 (MCP23017, 每片 16 通道)
    #[cfg(feature = "f3")]
    pub const DI_EXT_CHIPS: usize = 1; // 16 DI / 16 per chip = 1
    #[cfg(feature = "f4")]
    pub const DI_EXT_CHIPS: usize = 3; // 48 DI / 16 per chip = 3
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const DI_EXT_CHIPS: usize = 0;

    /// DO 扩展芯片数量 (F3: 1 片 16 DO; F4: 3 片 48 DO)
    #[cfg(feature = "f3")]
    pub const DO_EXT_CHIPS: usize = 1;
    #[cfg(feature = "f4")]
    pub const DO_EXT_CHIPS: usize = 3;
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const DO_EXT_CHIPS: usize = 0;

    /// AI 通道数 (Modbus 报告值, 与 MCA F16/F48 一致).
    /// 物理仍由 hal::adc 提供 6 路 (ADC1_CH0..5); 越界寄存器返回 0 对齐 MCA 无硬件时的 0 行为.
    /// - F16 (默认): 4 路 (MCA `REG_AMAX = REG_A04`)
    /// - F4 (MCA F48): 8 路 (MCA `MCA_F48_HARDWARE_RESOURCE` 时 `REG_AMAX = REG_A08`)
    /// - F3: 4 路 (无 8 路 AI 子型号)
    #[cfg(feature = "f4")]
    pub const AI_COUNT: u16 = 8;
    #[cfg(not(feature = "f4"))]
    pub const AI_COUNT: u16 = 4;
}

// ----------------------------------------------------------------------------
// MCP23017 IO 扩展芯片配置 (仅 F3/F4 使用)
// ----------------------------------------------------------------------------
// MCP23017: 16 通道 I2C IO 扩展, 地址 0x20-0x27 (A0/A1/A2 接地/接高)
// 每片 2 个端口 (PORTA + PORTB), 各 8 位, 共 16 位
// ----------------------------------------------------------------------------
pub mod io_ext {
    /// MCP23017 DI 芯片 I2C 地址列表 (7-bit, 不含 R/W 位)
    #[cfg(feature = "f3")]
    pub const DI_ADDRS: &[u8] = &[0x20]; // 1 片, 16 DI
    #[cfg(feature = "f4")]
    pub const DI_ADDRS: &[u8] = &[0x20, 0x21, 0x22]; // 3 片, 48 DI
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const DI_ADDRS: &[u8] = &[];

    /// MCP23017 DO 芯片 I2C 地址列表 (F3: 1 片; F4: 3 片)
    /// F4: DI 已占用 0x20/0x21/0x22, DO 用 0x23/0x24/0x25
    #[cfg(feature = "f3")]
    pub const DO_ADDRS: &[u8] = &[0x21]; // F3: 1 片 DO @ 0x21
    #[cfg(feature = "f4")]
    pub const DO_ADDRS: &[u8] = &[0x23, 0x24, 0x25]; // F4: 3 片 DO @ 0x23/0x24/0x25
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const DO_ADDRS: &[u8] = &[];

    /// 单 DO 芯片地址 (向后兼容, 取第一个地址)
    #[cfg(feature = "f3")]
    pub const DO_ADDR: u8 = 0x21;
    #[cfg(feature = "f4")]
    pub const DO_ADDR: u8 = 0x23;
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    pub const DO_ADDR: u8 = 0;

    // ---- MCP23017 寄存器地址 (Byte mode, IOCON.BANK=0) ----
    pub const REG_IODIRA: u8 = 0x00; // PORTA 方向 (1=输入, 0=输出)
    pub const REG_IODIRB: u8 = 0x01; // PORTB 方向
    pub const REG_IOCON: u8 = 0x0A; // 配置寄存器 (BANK=0 模式, LOOP13: 强制写 0x00)
    pub const REG_GPPUA: u8 = 0x0C; // PORTA 上拉 (1=使能)
    pub const REG_GPPUB: u8 = 0x0D; // PORTB 上拉
    pub const REG_GPIOA: u8 = 0x12; // PORTA 数据 (读=输入电平, 写=输出锁存)
    pub const REG_GPIOB: u8 = 0x13; // PORTB 数据
    pub const REG_OLATA: u8 = 0x14; // PORTA 输出锁存
    pub const REG_OLATB: u8 = 0x15; // PORTB 输出锁存
}

// ----------------------------------------------------------------------------
// AI 通道 4-20mA 标定参数
// ----------------------------------------------------------------------------
// 假设 ADC 0-3.3V (0..=ADC_MAX) 对应 4-20mA
// scaled = MA_MIN + avg * (MA_MAX - MA_MIN) / ADC_MAX, 单位 mA*1000
pub mod ai_calib {
    /// ADC 满量程 (12-bit)
    pub const ADC_MAX: u32 = 4095;
    /// 4mA 对应的 scaled 值 (mA*1000)
    pub const MA_MIN: u32 = 4000;
    /// 20mA 对应的 scaled 值 (mA*1000)
    pub const MA_MAX: u32 = 20000;

    /// ADC 自动校准参考值 (对齐参考固件 SENSOR_MIN/SENSOR_MAX)
    /// 4mA 对应的 ADC 原始值 (理论值, 校准窗口判定基准)
    pub const SENSOR_MIN: u16 = 605;
    /// 20mA 对应的 ADC 原始值 (理论值, 校准窗口判定基准)
    pub const SENSOR_MAX: u16 = 3016;
    /// 校准窗口时长 (ms) — 对齐参考固件 millis() < 8000
    pub const CALIB_WINDOW_MS: u64 = 8000;
    /// 校准采样间隔 (ms)
    pub const CALIB_SAMPLE_INTERVAL_MS: u64 = 50;
}

// ----------------------------------------------------------------------------
// Modbus 参数
// ----------------------------------------------------------------------------
pub mod modbus {
    /// RTU 主站参数
    pub mod rtu_master {
        pub const UART_PORT: u8 = 1;
        pub const BAUD: u32 = 9600;
        pub const ADDR: u8 = 1; // 本机作为主站，从站 1~247
        pub const PARITY: char = 'N';
        pub const STOP_BITS: u8 = 1;
        pub const DATA_BITS: u8 = 8;
        pub const POLL_INTERVAL_MS: u64 = 1000; // 无实际从站时降低轮询频率
        pub const TIMEOUT_MS: u64 = 100; // 缩短超时减少等待
    }

    /// RTU 从站参数
    pub mod rtu_slave {
        pub const UART_PORT: u8 = 2; // UART2 (ESP32-S3 有 3 个 UART)
        pub const BAUD: u32 = 9600;
        pub const ADDR: u8 = 1; // 本机从站地址
        pub const PARITY: char = 'N';
        pub const STOP_BITS: u8 = 1;
        pub const DATA_BITS: u8 = 8;
    }

    /// RTU 第 3 端口参数 (RS485-3, UART0, 对齐参考固件)
    /// 默认仅从站监听模式; UART0 与 USB CDC/JTAG 复用, 启用将失去调试串口.
    /// 由 `ENABLED` 控制是否启动 (默认 false, 需手动启用).
    pub mod rtu_port2 {
        pub const ENABLED: bool = false; // 默认禁用 (UART0 与 USB 串口复用)
        pub const BAUD: u32 = 9600;
        pub const ADDR: u8 = 1;
        pub const PARITY: char = 'N';
        pub const STOP_BITS: u8 = 1;
        pub const DATA_BITS: u8 = 8;
    }

    /// TCP Server 参数
    ///
    /// 4 端口设计对齐参考固件 MCA (server_wifi/1/2/3 = SLAVE_REG_TCP_COM1..4):
    ///   - 502  标准 Modbus TCP
    ///   - 503/504  扩展 (与 502 同协议, 供不同上位机接入)
    ///   - 5002 备用通道
    ///
    /// 内存优化: 由 `tcp_server::tick_tcp_server` 在 main-loop 多路复用所有端口，
    /// 而非每端口一个监听线程 (LOOP8 曾因第 5 个 pthread 触发 ENOMEM 而裁剪到 1 端口).
    /// 5ms tick + 非阻塞 accept 不再需要额外任务栈。
    pub mod tcp {
        /// 默认 4 端口 (对齐参考固件 ext_tcp_port1..4 = {502,503,504,5002}).
        /// 运行时端口可由 Modbus 写 HOLD_TCP_COM_BASE..3 (2243-2246) 动态修改.
        pub const PORTS: &[u16] = &[502, 503, 504, 5002];
        pub const MAX_CONNECTIONS: usize = 8;
        /// 已建立连接的空闲回收时间。原 2 秒“读超时”不能直接当连接空闲超时，
        /// 否则常见 PLC/SCADA 长连接在两次轮询间就被服务端主动断开。
        pub const IDLE_TIMEOUT_MS: u64 = 5 * 60 * 1000;
        /// 弱网下完整 MBAP 帧的接收上限。计时从首字节开始，后续零散字节不续期，
        /// 防止 slowloris 客户端永久占用固定的 8 个连接槽。
        pub const PARTIAL_FRAME_TIMEOUT_MS: u64 = 30_000;
        pub const TX_TIMEOUT_MS: u64 = 5000;
        /// TCP keepalive 用于回收拔线、客户端断电等未发送 FIN/RST 的半开连接。
        pub const KEEPALIVE_IDLE_S: i32 = 30;
        pub const KEEPALIVE_INTERVAL_S: i32 = 10;
        pub const KEEPALIVE_COUNT: i32 = 3;
    }
}

// ----------------------------------------------------------------------------
// Wi-Fi 参数 (ESP32-S3 内置, 作为以太网冗余或 AP 配置入口)
// ----------------------------------------------------------------------------
// TODO: 从 SystemConfig 动态加载 (运行时可通过 BLE AT 修改)
pub mod wifi {
    /// 默认 Station SSID (TODO: 从 SystemConfig 加载)
    pub const SSID: &str = "iot-gateway";
    /// 默认 Station 密码 (空字符串 = 开放网络)
    pub const PASSWORD: &str = "";
    /// Wi-Fi 心跳周期 (s)
    pub const HEARTBEAT_PERIOD_S: u64 = 30;
    /// 连接超时 (s, BlockingWifi 内部使用)
    pub const CONNECT_TIMEOUT_S: u64 = 30;
}

// ----------------------------------------------------------------------------
// BLE Mesh 参数
// ----------------------------------------------------------------------------

pub mod regs {
    use crate::config::hw_version;

    // ---- PC 配置工具兼容窗口 ----
    // tauri-app 固定批量读取 X0..X47 / Y0..Y47 / A0..A7，再根据设备能力裁剪显示。
    // 即使 F16 只有 16 DI/DO、4 AI，也必须对未安装点返回 0，不能让整笔 Modbus
    // 请求因跨越实际硬件点数而失败。
    pub const LEGACY_POINT_WINDOW_COUNT: u16 = 48;
    pub const LEGACY_AI_WINDOW_COUNT: u16 = 8;
    pub const INREG_PC_META_BASE: u16 = 0x0800;
    pub const INREG_PC_META_COUNT: u16 = 6;

    // ---- 线圈 (Coil, FC=01/05/15) - DO 输出 ----
    // 参考固件: REG_D01 = 0x200, 编号 1-16 (F16) 或 1-48 (F48)
    pub const COIL_DO_BASE: u16 = 0x0200;
    pub const COIL_DO_COUNT: u16 = hw_version::DO_COUNT as u16;
    pub const COIL_DO_END: u16 = COIL_DO_BASE + COIL_DO_COUNT;
    // 原 C++/PC 工具内部控制线圈。
    pub const COIL_INTERNAL_START: u16 = 0x0400;
    pub const COIL_INTERNAL_STOP: u16 = 0x0401;
    pub const COIL_RESTART: u16 = 0x0402;
    pub const COIL_LOGIC_RESTART: u16 = 0x0403;
    /// 旧 MCA 扩展线圈窗口: DRegBuf 覆盖 REG_D01..REG_DXX (512..2047).
    pub const LEGACY_COIL_BASE: u16 = 0x0200;
    pub const LEGACY_COIL_END: u16 = 0x07FF;
    pub const LEGACY_COIL_COUNT: u16 = LEGACY_COIL_END - LEGACY_COIL_BASE + 1;

    // ---- 离散输入 (Discrete Input, FC=02) - DI 输入 ----
    // 参考固件: REG_T01 = 0x0000, 编号 1-16 (F16)
    pub const DISC_DI_BASE: u16 = 0x0000;
    pub const DISC_DI_COUNT: u16 = hw_version::DI_COUNT as u16;

    // ---- 输入寄存器 (Input Register, FC=04, RO) ----
    // 参考固件: REG_A01 = 0x0080 (AI), REG_STATU_A01 = 0x0088 (AI状态)
    pub const INREG_AI_BASE: u16 = 0x0080;
    pub const INREG_AI_COUNT: u16 = hw_version::AI_COUNT; // F16=4 / F48=8, 与 MCA 对齐
    pub const INREG_AI_STATUS_BASE: u16 = 0x0088;
    // 系统信息 (RO)
    pub const INREG_QI_COUNT: u16 = 0x087C; // Q和I点数 (高字节=Q, 低字节=I)
    pub const INREG_ADC485: u16 = 0x087D; // 模拟量+485通道数
    pub const INREG_FW_VER: u16 = 0x087E; // 固件版本号
    pub const INREG_FW_DATE: u16 = 0x087F; // 固件版本日期
    pub const INREG_LEGACY_END: u16 = 0x087F;

    // ---- 故障恢复状态 (RO, FC=04) ----
    // 暴露给 Modbus Master 用于远程监控设备健康
    pub const INREG_RECOV_RECOVERABLE: u16 = 0x0880; // 可恢复故障计数
    pub const INREG_RECOV_DEGRADABLE: u16 = 0x0881; // 降级故障计数
    pub const INREG_RECOV_SEVERE: u16 = 0x0882; // 严重故障计数
    pub const INREG_RECOV_MODE: u16 = 0x0883; // 当前降级模式 (0=Normal, 1=BleOnly, 2=LocalOnly, 3=Minimal)
    pub const INREG_RECOV_BLE_DROPS: u16 = 0x0884; // BLE notify 丢弃帧计数

    // ---- 错误环日志 (RO, FC=04) ----
    // 用于远程诊断: 拉取最近 8 条错误记录
    // 每条 4 个 U16: timestamp_low, timestamp_high+level+module, code, context_low, context_high
    // ---- BLE Android 兼容寄存器 (RO, FC=04) ----
    // 0x08A5-0x08E2 是 metuory-wireless-management-app-1.0.78 通过 BLE 读取的寄存器,
    // 也通过 Modbus TCP 暴露给远程 Master (用于调试)
    pub const INREG_HW_VER: u16 = 0x08A5; // 硬件版本号 (CONFIG.cfg.hw_version)
    pub const INREG_IP_BASE: u16 = 0x08C7; // IP/Mask/GW (12 regs = 24 bytes)
    pub const INREG_MAC_BASE: u16 = 0x08D7; // MAC (6 regs = 12 bytes)
    pub const INREG_BLE_ID_BASE: u16 = 0x08E2; // BLE 名称 (4 regs = 8 bytes)

    pub const INREG_RINGLOG_COUNT: u16 = 0x0885; // 当前环日志条目数 (0-100)
    pub const INREG_RINGLOG_WRITES: u16 = 0x0886; // 总写入次数 (mod 2^32)
    pub const INREG_RINGLOG_BASE: u16 = 0x0887; // 8 条最近日志基地址
    // 0x0887-0x08A6: 8 条 × 4 U16 = 32 个寄存器

    // ---- MCA 一体机分布式组播同步状态 (RO, FC=04) ----
    // 移植自参考固件 REG_STATU_SWITCH_START..END (0x0090-0x0100, 113 个 U16)
    // 接收的 32 字节组播数据填入 [0x0090..0x00AF], 余下保留.
    pub const INREG_SWITCH_STATUS_BASE: u16 = 0x0090;
    pub const INREG_SWITCH_STATUS_END: u16 = 0x0100;
    pub const INREG_SWITCH_STATUS_COUNT: u16 = INREG_SWITCH_STATUS_END - INREG_SWITCH_STATUS_BASE;
    pub const INREG_MULTICAST_IP1_2: u16 = 2190; // 组播 IP (octet1<<8 | octet2)
    pub const INREG_MULTICAST_IP3_4: u16 = 2191; // 组播 IP (octet3<<8 | octet4)
    pub const INREG_MULTICAST_PORT: u16 = 2192; // 组播端口 (默认 5003)
    pub const INREG_SWITCH_IP1_2: u16 = 2193; // 源 IP 过滤 (octet1<<8 | octet2)
    pub const INREG_SWITCH_IP3_4: u16 = 2194; // 源 IP 过滤 (octet3<<8 | octet4)
    /// 组播接收缓冲区大小 (对齐参考固件 RECEIVE_MULTICAST_BUF_SIZE)
    pub const MULTICAST_BUF_SIZE: usize = 32;

    // ---- 保持寄存器 (Holding Register, FC=03/06/16, RW) ----
    // 参考固件: SLAVE_REG_P01 = 0x0880 (2176)
    pub const HOLD_CFG_BASE: u16 = 0x0880;
    // 485 通信状态 (2176-2179, RO)
    pub const HOLD_485_1_COMERR: u16 = 0x0880;
    pub const HOLD_485_1_APPERR: u16 = 0x0881;
    pub const HOLD_485_2_COMERR: u16 = 0x0882;
    pub const HOLD_485_2_APPERR: u16 = 0x0883;
    pub const HOLD_485_3_COMERR: u16 = 0x0884;
    pub const HOLD_485_3_APPERR: u16 = 0x0885;
    // SN 序列号 (2196-2204 = 9 words = 18 ASCII chars)
    pub const HOLD_SN_BASE: u16 = 2196;
    pub const HOLD_SN_COUNT: u16 = 9;
    // 位置/桩号 (2205-2212 = 8 words = 16 ASCII chars)
    pub const HOLD_PLACE_BASE: u16 = 2205;
    pub const HOLD_PLACE_COUNT: u16 = 8;
    // 硬件版本 (2213)
    pub const HOLD_HW_VER: u16 = 2213;
    // 串口配置 (2214-2238, 5 ports × 5 words): 前 3 组是物理 RS485，
    // 后 2 组是原 C++/PC 工具保留的 BT/NET 逻辑端口。
    pub const HOLD_RS485_BASE: u16 = 2214;
    pub const HOLD_RS485_STRIDE: u16 = 5;
    // 保留/未知 (2239-2242, 参考固件默认 5500-5503)
    pub const HOLD_UNKNOWN_BASE: u16 = 2239;
    pub const HOLD_UNKNOWN_COUNT: u16 = 4;
    // TCP COM 端口 (2243-2246, 参考固件 SLAVE_REG_TCP_COM1..4)
    pub const HOLD_TCP_COM_BASE: u16 = 2243;
    pub const HOLD_TCP_COM_COUNT: u16 = 4;
    // IP 地址 (2247-2250 = 2 words)
    pub const HOLD_IP_BASE: u16 = 2247;
    // 子网掩码 (2251-2254)
    pub const HOLD_MASK_BASE: u16 = 2251;
    // 网关 (2255-2258)
    pub const HOLD_GW_BASE: u16 = 2255;
    // DNS (2259-2262)
    pub const HOLD_DNS_BASE: u16 = 2259;
    // MAC 地址 (2263-2268 = 6 bytes in 3 words)
    pub const HOLD_MAC_BASE: u16 = 2263;
    // 主站 COM 数量 + IP (2269-2273)
    pub const HOLD_MASTER_COM: u16 = 2269;
    pub const HOLD_MASTER_IP_BASE: u16 = 2270;
    // 旧 MCA 的 2274..2277 是 8 字节蓝牙地址 (SLAVE_REG_BT_ARRD1..4)，
    // 属于通用 PRegBuf；蓝牙名称不占用 Modbus holding 地址。
    pub const HOLD_BLE_ADDR_BASE: u16 = 2274;
    pub const HOLD_BLE_ADDR_COUNT: u16 = 4;
    /// Deprecated name retained for source/API compatibility; value is the MCA BLE address.
    pub const HOLD_BLE_NAME_BASE: u16 = HOLD_BLE_ADDR_BASE;
    pub const HOLD_BLE_NAME_COUNT: u16 = HOLD_BLE_ADDR_COUNT;
    // 传感器标定 (2280-2295, 8 sensors × 2 values)
    pub const HOLD_SENSOR_MIN_BASE: u16 = 2280;
    pub const HOLD_SENSOR_MAX_BASE: u16 = 2288;
    // 设备功能配置 (2300+)
    pub const HOLD_DEVICE_CONFIG: u16 = 2300;
    // 用户自定义区 (4000-4223 = 224 words)
    pub const HOLD_USER_BASE: u16 = 4000;
    pub const HOLD_USER_COUNT: u16 = 224;
    pub const HOLD_PROTECT_WORD: u16 = 4222;
    // P区结束地址
    pub const HOLD_CFG_END: u16 = 4223;
    // 通用 P区缓冲 (0x0880..0x107F = 2048 字) — 用于未映射字段的通用读写
    pub const HOLD_PXX_BASE: u16 = HOLD_CFG_BASE; // = 0x0880
    pub const HOLD_PXX_END: u16 = HOLD_CFG_END; // = 0x107F
    pub const HOLD_PXX_COUNT: usize = (HOLD_PXX_END - HOLD_PXX_BASE + 1) as usize;
    /// PC DeviceMMP 固定从 2196 开始读写 83 words（最后一个地址 2278）。
    pub const HOLD_PC_DEVICE_BASE: u16 = HOLD_SN_BASE;
    pub const HOLD_PC_DEVICE_COUNT: u16 = 83;
    pub const HOLD_PC_DEVICE_END: u16 = HOLD_PC_DEVICE_BASE + HOLD_PC_DEVICE_COUNT - 1;

    // ---- 设备文本区 (5000-6999, 2000 字) ----
    pub const DEVICE_TEXT_BASE: u16 = 5000;
    pub const DEVICE_TEXT_COUNT: u16 = 2000;
    pub const DEVICE_TEXT_END: u16 = 6999;

    // ---- 协议存储区 (与用户区连续, 兼容原有 PROTO 区) ----
    pub const PROTO_BASE: u16 = 0x4000;
    pub const PROTO_COUNT: u16 = 1500;
    pub const PROTO_END: u16 = PROTO_BASE + PROTO_COUNT;
    pub const PROTO_COMMIT: u16 = PROTO_END;
    pub const PROTO_RELOAD: u16 = PROTO_END + 1;
    pub const PROTO_VERSION: u16 = PROTO_END + 2;
    pub const PROTO_LENGTH: u16 = PROTO_END + 3;
    pub const PROTO_STATUS: u16 = PROTO_END + 4;
    pub const PROTO_MAGIC: u16 = PROTO_END + 5;

    // ---- 设备功能区 (Android 1.0.78: 0x08FC = count, 0x08FE+ = config) ----
    // 0x08FC: 设备功能条目计数；后续变长配置与 PC 逻辑区共用 PRegBuf。
    pub const FUNC_COUNT: u16 = 0x08FC;

    // ---- 设备文本区 (Android 1.0.78: 0x1388 = meta, 0x138A+ = data) ----
    // 0x1388: 文本条目数 (1 reg)
    // 0x1389: 文本数据总长度 字节 (1 reg)
    // 0x138A+: UTF-16LE 文本数据, 每条 [len:2 BE][data:N]
    pub const TEXT_META_BASE: u16 = 0x1388;
    pub const TEXT_META_COUNT: u16 = 2;
    pub const TEXT_DATA_BASE: u16 = 0x138A;
    // TEXT_DATA 占用到 DEVICE_TEXT_END (6999)

    // ---- Internal-only registers (not exposed via Modbus, for backward compat) ----
    pub const CFG_BASE: u16 = HOLD_CFG_BASE;
    pub const CFG_FW_VER: u16 = INREG_FW_VER;
    pub const CFG_CFG_VER: u16 = 0xFF00;
    pub const CFG_APPLY: u16 = 0xFF01;
    pub const CFG_RESET_DEFAULT: u16 = 0xFF02;
    pub const CFG_DHCP: u16 = 0xFF03;
    pub const CFG_BLE_MAC_BASE: u16 = 0;
    pub const CFG_END: u16 = HOLD_CFG_END + 1;
    // Backward compat aliases
    pub const CFG_BLE_NAME_BASE: u16 = HOLD_BLE_ADDR_BASE;
    pub const CFG_NAME_BASE: u16 = HOLD_PLACE_BASE;
    pub const CFG_NAME_COUNT: u16 = HOLD_PLACE_COUNT;
    pub const CFG_RS485_BASE: u16 = HOLD_RS485_BASE;
    pub const CFG_RS485_STRIDE: u16 = HOLD_RS485_STRIDE;
    pub const CFG_RS485_COUNT: u16 = 5;
    // Master config defaults
    pub const TCP_PORTS_DEFAULT: [u16; 4] = [502, 503, 504, 5002];
    pub const UNKNOWN_DEFAULTS: [u16; 4] = [5500, 5501, 5502, 5503];

    // ---- PLC 别名区 (LOOP12: 老 SCADA 5 位地址兼容) ----
    // Modbus 5 位地址约定: 3xxxx → 输入寄存器 (FC=04 RO), 4xxxx → 保持寄存器 (FC=03/06/10 RW).
    // 本设备把这两个窗口作为现有 DI/AI/DO/holding_buf 区的镜像, 避免新存储.
    // 30001-30128 = 0x7531-0x75B0 (128 regs): 映射到 DI 状态 + AI scaled.
    // 40001-40300 = 0x9C41-0x9D6C (300 regs): 映射到 DO 状态 + HOLD_USER_BASE 区.
    pub const MONITOR_PLC_BASE: u16 = 0x7531; // 30001
    pub const MONITOR_PLC_END: u16 = 0x75B0; // 30128
    pub const MONITOR_PLC_COUNT: u16 = 128;
    pub const CONTROL_PLC_BASE: u16 = 0x9C41; // 40001
    pub const CONTROL_PLC_END: u16 = 0x9D6C; // 40300
    pub const CONTROL_PLC_COUNT: u16 = 300;
    pub const MONITOR_WORD_COUNT: u16 = 128;
    pub const CONTROL_WORD_COUNT: u16 = 300;
}

// ============================================================================
// 单元测试 — 验证寄存器布局与参考固件对齐
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mca_register_layout_aligned() {
        // 验证关键寄存器地址与 MCA_F16V2_1_F48_BLE.ino 一致
        // REG_D01 = 0x0200
        assert_eq!(regs::COIL_DO_BASE, 0x0200);
        assert_eq!(regs::COIL_RESTART, 0x0402);
        assert_eq!(regs::COIL_LOGIC_RESTART, 0x0403);
        // REG_T01 = 0x0000 (离散输入起点)
        assert_eq!(regs::DISC_DI_BASE, 0x0000);
        // REG_A01 = 0x0080 (AI 起点)
        assert_eq!(regs::INREG_AI_BASE, 0x0080);
        // SLAVE_REG_P01 = 0x0880 (保持寄存器起点)
        assert_eq!(regs::HOLD_CFG_BASE, 0x0880);
        // SLAVE_REG_SN1 = 2196
        assert_eq!(regs::HOLD_SN_BASE, 2196);
        // SLAVE_REG_PLACE1 = 2205
        assert_eq!(regs::HOLD_PLACE_BASE, 2205);
        // SLAVE_REG_HW_VER = 2213
        assert_eq!(regs::HOLD_HW_VER, 2213);
        // SLAVE_REG_485_1_1 = 2214
        assert_eq!(regs::HOLD_RS485_BASE, 2214);
        // SLAVE_REG_TCP_COM1 = 2243
        assert_eq!(regs::HOLD_TCP_COM_BASE, 2243);
        // SLAVE_REG_PIP1 = 2247
        assert_eq!(regs::HOLD_IP_BASE, 2247);
        // SLAVE_REG_PNTEMASK1 = 2251
        assert_eq!(regs::HOLD_MASK_BASE, 2251);
        // SLAVE_REG_PGW1 = 2255
        assert_eq!(regs::HOLD_GW_BASE, 2255);
        // SLAVE_REG_DNS1 = 2259
        assert_eq!(regs::HOLD_DNS_BASE, 2259);
        // SLAVE_REG_MAC1 = 2263
        assert_eq!(regs::HOLD_MAC_BASE, 2263);
        // SLAVE_REG_MASTER_COM = 2269
        assert_eq!(regs::HOLD_MASTER_COM, 2269);
        // 旧 MCA 蓝牙地址起点
        assert_eq!(regs::HOLD_BLE_ADDR_BASE, 2274);
        // SLAVE_SERSOR_MIN = 2280
        assert_eq!(regs::HOLD_SENSOR_MIN_BASE, 2280);
        // SLAVE_SERSOR_MAX = 2288
        assert_eq!(regs::HOLD_SENSOR_MAX_BASE, 2288);
        // SLAVE_DEVICE_CONFIG = 2300
        assert_eq!(regs::HOLD_DEVICE_CONFIG, 2300);
        // SLAVE_USER_START = 4000
        assert_eq!(regs::HOLD_USER_BASE, 4000);
    }

    #[test]
    fn test_register_address_invariants() {
        // P 区连续
        assert!(regs::HOLD_RS485_BASE < regs::HOLD_TCP_COM_BASE);
        assert!(regs::HOLD_TCP_COM_BASE < regs::HOLD_IP_BASE);
        assert!(regs::HOLD_IP_BASE < regs::HOLD_MASK_BASE);
        assert!(regs::HOLD_MASK_BASE < regs::HOLD_GW_BASE);
        assert!(regs::HOLD_GW_BASE < regs::HOLD_DNS_BASE);
        assert!(regs::HOLD_DNS_BASE < regs::HOLD_MAC_BASE);
        assert!(regs::HOLD_MAC_BASE < regs::HOLD_MASTER_COM);
        assert!(regs::HOLD_MASTER_COM < regs::HOLD_BLE_ADDR_BASE);
        assert_eq!(regs::HOLD_PC_DEVICE_BASE, 2196);
        assert_eq!(regs::HOLD_PC_DEVICE_END, 2278);
    }

    #[test]
    #[cfg(feature = "f3")]
    fn test_f3_di_count() {
        assert_eq!(hw_version::DI_COUNT, 16);
        assert_eq!(hw_version::DO_COUNT, 16);
        assert_eq!(hw_version::NAME, "F3");
    }

    #[test]
    #[cfg(feature = "f4")]
    fn test_f4_di_do_count() {
        // F4: 48 DI + 48 DO
        assert_eq!(hw_version::DI_COUNT, 48);
        assert_eq!(hw_version::DO_COUNT, 48);
        assert_eq!(hw_version::NAME, "F4");
        assert_eq!(hw_version::DO_EXT_CHIPS, 3);
        assert_eq!(crate::config::io_ext::DO_ADDRS.len(), 3);
    }

    #[test]
    #[cfg(not(any(feature = "f3", feature = "f4")))]
    fn test_default_di_do_count() {
        assert_eq!(hw_version::NAME, "F16");
        assert_eq!(hw_version::DI_COUNT, 16);
        assert_eq!(hw_version::DO_COUNT, 16);
    }

    #[test]
    fn test_protocol_area() {
        // 协议区 (与用户区连续)
        assert_eq!(regs::PROTO_BASE, 0x4000);
        assert_eq!(regs::PROTO_COUNT, 1500);
        assert_eq!(regs::PROTO_END, 0x4000 + 1500);
    }

    #[test]
    fn test_legacy_text_and_logic_storage_sizes() {
        assert_eq!(regs::DEVICE_TEXT_BASE, 5000);
        assert_eq!(regs::DEVICE_TEXT_END, 6999);
        assert_eq!(regs::DEVICE_TEXT_COUNT, 2000);
        assert_eq!(
            (regs::DEVICE_TEXT_END - regs::DEVICE_TEXT_BASE + 1) as usize,
            2000
        );
        assert_eq!(regs::HOLD_PXX_BASE, 0x0880);
        assert_eq!(regs::HOLD_PXX_END, 0x107F);
        assert_eq!(regs::HOLD_PXX_COUNT, 2048);
        assert_eq!(regs::HOLD_DEVICE_CONFIG, 2300);
    }

    #[test]
    fn test_app_metadata() {
        assert_eq!(APP_NAME, "esp32s3-iot-gateway");
        assert_eq!(MAIN_LOOP_PERIOD_MS, 5);
        assert_eq!(BLE_PROCESS_PERIOD_MS, 10);
        assert_eq!(BLE_PROCESS_PERIOD_MS % MAIN_LOOP_PERIOD_MS, 0);
    }
}
