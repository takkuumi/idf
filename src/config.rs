//! 全局硬件/应用配置常量
//!
//! 所有引脚分配、外设编号、采样周期统一在此处定义，
//! 便于硬件改版时一次修改、全局生效。
//!
//! 硬件平台：ESP32-S3R8 (Xtensa LX7 双核 240MHz, 512KB SRAM, 8MB Octal PSRAM)
//!   - 内置 Wi-Fi 802.11 b/g/n + BLE 5.0 + Bluetooth Mesh
//!   - 3 个 UART (UART0/1/2), 4 个 SPI (SPI0/1 Flash/PSRAM, SPI2/3 外设)
//!   - 2 个 ADC (ADC1: 10 通道, ADC2: 10 通道, 12-bit)
//!   - 8 通道 LEDC PWM
//!   - 45 个 GPIO (GPIO0~GPIO48), GPIO26~32 被 Octal SPI Flash/PSRAM 占用
//!
//! 实际硬件改版只需修改本文件。

// ----------------------------------------------------------------------------
// 系统参数
// ----------------------------------------------------------------------------
pub const APP_NAME: &str = "esp32s3-iot-gateway";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 主循环周期 (ms)
pub const MAIN_LOOP_PERIOD_MS: u64 = 100;

// ----------------------------------------------------------------------------
// GPIO 引脚分配 (ESP32-S3R8)
// ESP32-S3 共 45 个 GPIO (GPIO0~GPIO48)
// GPIO26~32: 被 Octal SPI Flash/PSRAM 占用 (8MB PSRAM), 不可用
// GPIO0:    strapping (boot mode, 需外部上拉)
// GPIO3:    strapping (JTAG source)
// GPIO45/46: strapping (VDD_SPI / system freq)
// GPIO43/44: UART0 默认 TX/RX (下载/日志)
// ----------------------------------------------------------------------------
pub mod pins {
    // ---- 以太网 W5500 (SPI2_HOST) ----
    // W5500: 硬件 TCP/IP 以太网控制器, SPI 接口, 内置 32KB 缓冲, 8 socket
    // SPI mode 0, 最高 80MHz (实际用 20MHz 保证稳定性)
    pub const ETH_SPI_HOST: u8 = 2; // SPI2_HOST (ESP32-S3 有 SPI2/3 可用于外设)
    pub const ETH_SPI_MOSI: u8 = 11;
    pub const ETH_SPI_MISO: u8 = 13;
    pub const ETH_SPI_SCLK: u8 = 12;
    pub const ETH_SPI_CS: u8 = 10;
    pub const ETH_INT: u8 = 14;  // W5500 INT 引脚, 低有效
    pub const ETH_RST: u8 = 15;  // W5500 RST 引脚, 低有效

    // ---- RS485 #0 (用作 Modbus RTU Master, UART1) ----
    // UART1 映射到高位 GPIO, 避开 ADC1 引脚 (GPIO1-6 对应 ADC1_CH0-5)
    pub const RS485_0_UART: u8 = 1; // UART1
    pub const RS485_0_TX: u8 = 40;
    pub const RS485_0_RX: u8 = 41;
    pub const RS485_0_DE: u8 = 42; // DE/RE 共控 (高=发送, 低=接收)

    // ---- RS485 #1 (用作 Modbus RTU Slave, UART2) ----
    // ESP32-S3 有 3 个 UART: UART0(下载/日志) + UART1(主站) + UART2(从站)
    // 不再需要与下载串口复用, 稳定性更好
    pub const RS485_1_UART: u8 = 2; // UART2
    pub const RS485_1_TX: u8 = 17;
    pub const RS485_1_RX: u8 = 18;
    pub const RS485_1_DE: u8 = 7;

    // ---- 数字输入 DI (8 路, 光耦隔离) ----
    // 使用 GPIO19-21 + GPIO33-37, 避开 Flash/PSRAM 和 strapping 引脚
    pub const DI_PINS: [u8; 8] = [19, 20, 21, 33, 34, 35, 36, 37];

    // ---- 数字输出 DO (8 路, OC 输出) ----
    // 使用 GPIO8/9/16/38/39/45/46/48, 避开 ADC1 和 SPI 引脚
    pub const DO_PINS: [u8; 8] = [8, 9, 16, 38, 39, 45, 46, 48];

    // ---- AI 模拟输入 (ADC1, 6 通道, 12-bit SAR ADC) ----
    // ADC1_CH0-5 = GPIO1-6, 不与 UART1/UART2 冲突
    pub const AI_ADC_UNIT: u8 = 1; // ADC1
    pub const AI_CHANNELS: [u8; 6] = [0, 1, 2, 3, 4, 5];

    // ---- AO 模拟输出 (LEDC PWM, 4 通道) ----
    // 与 DO 部分复用 (硬件设计上互斥, 同一引脚不可同时使用)
    pub const AO_CHANNELS: [(u8, u8); 4] = [
        (0, 8),  // (ledc_channel, gpio)
        (1, 9),
        (2, 16),
        (3, 38),
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
    #[cfg(feature_f3)]
    pub const NAME: &str = "F3";
    #[cfg(feature_f4)]
    pub const NAME: &str = "F4";
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const NAME: &str = "Default";

    /// DI 通道数
    #[cfg(feature_f3)]
    pub const DI_COUNT: usize = 16;
    #[cfg(feature_f4)]
    pub const DI_COUNT: usize = 48;
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const DI_COUNT: usize = 8;

    /// DO 通道数
    #[cfg(feature_f3)]
    pub const DO_COUNT: usize = 16;
    #[cfg(feature_f4)]
    pub const DO_COUNT: usize = 16;
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const DO_COUNT: usize = 8;

    /// 是否使用 I2C IO 扩展 (F3/F4)
    #[cfg(any(feature_f3, feature_f4))]
    pub const USE_IO_EXT: bool = true;
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const USE_IO_EXT: bool = false;

    /// DI 扩展芯片数量 (MCP23017, 每片 16 通道)
    #[cfg(feature_f3)]
    pub const DI_EXT_CHIPS: usize = 1; // 16 DI / 16 per chip = 1
    #[cfg(feature_f4)]
    pub const DI_EXT_CHIPS: usize = 3; // 48 DI / 16 per chip = 3
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const DI_EXT_CHIPS: usize = 0;

    /// DO 扩展芯片数量 (F3/F4 都是 16 DO = 1 片)
    #[cfg(any(feature_f3, feature_f4))]
    pub const DO_EXT_CHIPS: usize = 1;
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const DO_EXT_CHIPS: usize = 0;
}

// ----------------------------------------------------------------------------
// MCP23017 IO 扩展芯片配置 (仅 F3/F4 使用)
// ----------------------------------------------------------------------------
// MCP23017: 16 通道 I2C IO 扩展, 地址 0x20-0x27 (A0/A1/A2 接地/接高)
// 每片 2 个端口 (PORTA + PORTB), 各 8 位, 共 16 位
// ----------------------------------------------------------------------------
pub mod io_ext {
    /// MCP23017 DI 芯片 I2C 地址列表 (7-bit, 不含 R/W 位)
    #[cfg(feature_f3)]
    pub const DI_ADDRS: &[u8] = &[0x20]; // 1 片, 16 DI
    #[cfg(feature_f4)]
    pub const DI_ADDRS: &[u8] = &[0x20, 0x21, 0x22]; // 3 片, 48 DI
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const DI_ADDRS: &[u8] = &[];

    /// MCP23017 DO 芯片 I2C 地址
    #[cfg(feature_f3)]
    pub const DO_ADDR: u8 = 0x21; // F3: 1 片 DO
    #[cfg(feature_f4)]
    pub const DO_ADDR: u8 = 0x23; // F4: 1 片 DO
    #[cfg(not(any(feature_f3, feature_f4)))]
    pub const DO_ADDR: u8 = 0;

    // ---- MCP23017 寄存器地址 (Byte mode, IOCON.BANK=0) ----
    pub const REG_IODIRA: u8 = 0x00; // PORTA 方向 (1=输入, 0=输出)
    pub const REG_IODIRB: u8 = 0x01; // PORTB 方向
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
        pub const POLL_INTERVAL_MS: u64 = 200;
        pub const TIMEOUT_MS: u64 = 500;
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

    /// TCP Server 参数
    pub mod tcp {
        pub const PORT: u16 = 502;
        pub const MAX_CONNECTIONS: usize = 4;
        pub const RX_TIMEOUT_MS: u64 = 2000;
        pub const TX_TIMEOUT_MS: u64 = 2000;
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
pub mod ble_mesh {
    /// 自身设备名
    pub const DEVICE_NAME: &str = "ESP32S3-GW";
    /// Mesh 网络 ID (用于区分不同网络)
    pub const NET_KEY_IDX: u16 = 0;
    pub const APP_KEY_IDX: u16 = 0;
    /// Generic OnOff Server model
    pub const MODEL_ID_ONOFF_SRV: u16 = 0x1000;
    /// Generic OnOff Client model
    pub const MODEL_ID_ONOFF_CLI: u16 = 0x1001;
    /// 节点配网 OOB 数量
    pub const OOB_SIZE: u8 = 4;
    /// 心跳周期 (秒)
    pub const HEARTBEAT_PERIOD_S: u32 = 60;
}

// ----------------------------------------------------------------------------
// 应用层寄存器布局
// ----------------------------------------------------------------------------
pub mod regs {
    // 线圈 (Coil, 1-bit, 可读写) - DO 输出
    // 通道数随硬件版本变化: Default=8, F3=16, F4=16
    pub const COIL_DO_BASE: u16 = 0x0000;
    pub const COIL_DO_COUNT: u16 = crate::config::hw_version::DO_COUNT as u16;

    // 离散输入 (Discrete Input, 1-bit, 只读) - DI 输入
    // 通道数随硬件版本变化: Default=8, F3=16, F4=48
    pub const DISC_DI_BASE: u16 = 0x0000;
    pub const DISC_DI_COUNT: u16 = crate::config::hw_version::DI_COUNT as u16;

    // 输入寄存器 (Input Register, 16-bit, 只读) - AI 输入
    pub const INREG_AI_BASE: u16 = 0x0000; // 6 路 AI, 原始 ADC 值
    pub const INREG_AI_COUNT: u16 = 6;
    pub const INREG_AI_SCALED_BASE: u16 = 0x0010; // 6 路 AI, 工程量 (放大 1000 倍)

    // 保持寄存器 (Holding Register, 16-bit, 可读写) - AO 输出 + 系统参数
    pub const HOLD_AO_BASE: u16 = 0x0000; // 4 路 AO, 工程量 (放大 1000 倍)
    pub const HOLD_AO_COUNT: u16 = 4;
    pub const HOLD_SYS_BASE: u16 = 0x0100; // 系统寄存器
    pub const HOLD_SYS_FW_VER: u16 = 0x0100; // 固件版本 (BCD: 0x0102 = v1.02)
    pub const HOLD_SYS_UPTIME_S: u16 = 0x0101; // 运行时长 (秒)
    pub const HOLD_SYS_RESET_CNT: u16 = 0x0102; // 复位计数
    pub const HOLD_SYS_RESET: u16 = 0x0103; // 写 0xA5A5 触发复位
    pub const HOLD_SYS_RESET_REASON: u16 = 0x0104; // 复位原因 (RO, esp_reset_reason_t)
    pub const HOLD_SYS_TASK_HEALTH: u16 = 0x0105; // 任务健康位图 (RO, bit=1 表示该任务停滞)
    pub const HOLD_SYS_LOG_LEVEL: u16 = 0x0106;    // 日志级别 (0=Err 1=Warn 2=Info 3=Debug 4=Trace, RW)

    // ============ OTA 升级区 (OTA UPGRADE) ============
    // 通过 BLE AT 命令 +OTA 触发升级, Modbus 只读状态 + 触发重启
    pub const HOLD_OTA_STATUS: u16 = 0x0107;        // (RO) 0=空闲 1=接收中 2=完成待重启 3=校验失败 4=空间不足 5=已中止
    pub const HOLD_OTA_TOTAL_LO: u16 = 0x0108;     // (RW) 升级包总大小 低 16 位 (字节数)
    pub const HOLD_OTA_TOTAL_HI: u16 = 0x0109;      // (RW) 升级包总大小 高 16 位
    pub const HOLD_OTA_WRITTEN_LO: u16 = 0x010A;   // (RO) 已写入字节数 低 16 位
    pub const HOLD_OTA_WRITTEN_HI: u16 = 0x010B;   // (RO) 已写入字节数 高 16 位
    pub const HOLD_OTA_BEGIN: u16 = 0x010C;         // (WO) 写 0x0B0A → 开始升级 (使用 TOTAL_* 字段值)
    pub const HOLD_OTA_END: u16 = 0x010D;           // (WO) 写 0x0E0D → 结束升级 + 设置启动分区
    pub const HOLD_OTA_ABORT: u16 = 0x010E;         // (WO) 写 0x0AB0 → 中止升级
    pub const HOLD_OTA_REBOOT: u16 = 0x010F;        // (WO) 写 0x0F0E → 重启应用新固件

    // ============ 系统配置区 (SYSTEM CONFIG) ============
    // 用户可读写, 通过 Modbus 或 BLE AT 修改
    // 修改后写 CFG_APPLY=0xB5B5 触发应用 (持久化 + 运行时生效)
    // 写 CFG_RESET=0xD5D5 恢复默认配置
    pub const CFG_BASE: u16 = 0x0200;
    pub const CFG_END: u16 = 0x0260; // 不含

    // 设备信息区 (0x0200-0x021F)
    pub const CFG_SN_BASE: u16 = 0x0200;        // SN 号 (16 字, ASCII 32 字符)
    pub const CFG_SN_COUNT: u16 = 16;
    pub const CFG_NAME_BASE: u16 = 0x0210;      // 设备名称 (8 字, ASCII 16 字符)
    pub const CFG_NAME_COUNT: u16 = 8;
    pub const CFG_HW_VER: u16 = 0x0218;         // 硬件版本 (BCD)
    pub const CFG_FW_VER: u16 = 0x0219;         // 固件版本 (BCD)
    pub const CFG_CFG_VER: u16 = 0x021A;        // 配置版本 (每次修改自增)
    pub const CFG_APPLY: u16 = 0x021B;           // 写 0xB5B5 → 应用配置 (持久化+生效)
    pub const CFG_RESET_DEFAULT: u16 = 0x021C;  // 写 0xD5D5 → 恢复默认

    // 网络配置 (0x0220-0x022F)
    pub const CFG_ETH_MAC_BASE: u16 = 0x0220;   // 以太网 MAC (3 字 = 6 字节)
    pub const CFG_DHCP: u16 = 0x0223;            // 0=静态, 1=DHCP
    pub const CFG_IP_BASE: u16 = 0x0224;         // IP 地址 (2 字 = 4 字节)
    pub const CFG_MASK_BASE: u16 = 0x0226;       // 子网掩码 (2 字)
    pub const CFG_GW_BASE: u16 = 0x0228;         // 网关 (2 字)
    pub const CFG_DNS_BASE: u16 = 0x022A;        // DNS (2 字)

    // 蓝牙配置 (0x0230-0x023F)
    pub const CFG_BLE_MAC_BASE: u16 = 0x0230;    // BLE MAC (3 字 = 6 字节)
    pub const CFG_BLE_NAME_BASE: u16 = 0x0233;   // BLE 名称 (4 字 = 8 字符)
    pub const CFG_BLE_MESH_EN: u16 = 0x0237;     // 0=禁用, 1=启用

    // RS485 配置 (0x0240-0x025F), 每通道 16 字
    pub const CFG_RS485_BASE: u16 = 0x0240;
    pub const CFG_RS485_STRIDE: u16 = 16;
    pub const CFG_RS485_COUNT: u16 = 2;
    // 通道内偏移:
    //   +0  波特率 (÷100, 9600 → 96, 115200 → 1152)
    //   +1  数据位 (7/8)
    //   +2  停止位 (1/2)
    //   +3  校验 (0=None, 1=Odd, 2=Even)
    //   +4  从站地址 (0=主站模式)
    //   +5  模式 (0=Master, 1=Slave, 2=Gateway)
    //   +6..+15  保留

    // ============ 协议存储区 (PROTOCOL STORE) ============
    // 用户自定义协议存储区，单段连续 1500 个 U16 (3000 字节)
    // 通过 Modbus FC=03/06/10 或 BLE AT 命令访问
    pub const PROTO_BASE: u16 = 0x4000;       // 协议数据区起始
    pub const PROTO_COUNT: u16 = 1500;        // 协议数据长度 (U16 单位)
    pub const PROTO_END: u16 = 0x4000 + 1500; // = 0x45DC (exclusive)
    pub const PROTO_COMMIT: u16 = 0x45DC;    // 写 0xC5C5 → 触发持久化到 NVS
    pub const PROTO_RELOAD: u16 = 0x45DD;    // 写 0xA5A5 → 从 NVS 重载到 RAM
    pub const PROTO_VERSION: u16 = 0x45DE;   // 用户自定义协议版本 (RW)
    pub const PROTO_LENGTH: u16 = 0x45DF;    // 用户写入的有效协议长度 (U16 数)
    pub const PROTO_STATUS: u16 = 0x45E0;    // 状态 (RO): 0=空闲, 1=写入中, 2=加载中, 3=校验失败
    pub const PROTO_MAGIC: u16 = 0x45E1;     // NVS 存储魔数 (RO): 0x4757 ("GW")
}
