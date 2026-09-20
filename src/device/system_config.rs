//! 系统配置存储
//!
//! 集中存储所有可配置的系统参数: SN/MAC/IP/网关/RS485/BLE 等。
//! Modbus 寄存器映射见 `config::regs::CFG_*` (0x0200-0x025F)。
//!
//! 持久化:
//! - 整个 SystemConfig 序列化为 152 字节固定布局 blob，单 key 存 NVS
//! - 修改后写 CFG_APPLY=0xB5B5 触发持久化 + 运行时应用
//! - 写 CFG_RESET=0xD5D5 恢复默认

use crate::config::regs;
use crate::error::{AppError, AppResult};
use esp_idf_svc::nvs::EspDefaultNvs;

// ----------------------------------------------------------------------------
// 常量
// ----------------------------------------------------------------------------

const NVS_KEY_BLOB: &str = "sys_cfg";
const NVS_KEY_MAGIC: &str = "sys_cfg_mag";
const NVS_MAGIC: u32 = 0x4757_4346; // "GWCF"

/// 序列化后字节数 (固定布局)
/// SN(32) + name(16) + hw(2) + fw(2) + cfg_ver(2) = 54
/// eth_mac(6) + dhcp(1) + ip(4) + mask(4) + gw(4) + dns(4) = 23
/// ble_mac(6) + ble_name(8) = 14
/// rs485[0](15) + rs485[1](15) + rs485[2](15) = 45  (对齐参考固件 3 端口)
/// tcp_ports(8)；总使用 145 字节，取 152 留余量。
const CFG_BLOB_SIZE: usize = 152;
/// v2.2.1 及更早版本的 NVS blob 大小；升级时必须保留原配置并为新增字段补默认值。
const CFG_BLOB_LEGACY_SIZE: usize = 144;

// NVS 偏移
const OFF_SN: usize = 0;
const OFF_NAME: usize = 32;
const OFF_HW: usize = 48;
const OFF_FW: usize = 50;
const OFF_CFG_VER: usize = 52;
const OFF_ETH_MAC: usize = 54;
const OFF_DHCP: usize = 60;
const OFF_IP: usize = 61;
const OFF_MASK: usize = 65;
const OFF_GW: usize = 69;
const OFF_DNS: usize = 73;
const OFF_BLE_MAC: usize = 77;
const OFF_BLE_NAME: usize = 83;
const OFF_RS485_0: usize = 92;
const OFF_RS485_1: usize = 107; // LOOP5 修复: 原来是 101, 与 OFF_RS485_0+9 (retry_count) 重叠
const OFF_RS485_2: usize = 122; // RS485 第 3 端口 (对齐参考固件 3 端口)
const OFF_TCP_PORTS: usize = 137;

// RS485 配置结构: baudrate(4) + data_bits(1) + stop_bits(1) + parity(1) +
//                 slave_addr(1) + mode(1) + retry_count(2) + timeout_ms(2) + interval_ms(2) = 15 bytes
const RS485_ENTRY_SIZE: usize = 15;

// ----------------------------------------------------------------------------
// 数据结构
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Rs485Config {
    pub baudrate: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: u8,       // 0=None 1=Odd 2=Even
    pub slave_addr: u8,   // 0=主站
    pub mode: u8,         // 0=Master 1=Slave 2=Gateway 3=Transparent(LoRa透传)
    pub retry_count: u16, // LOOP5: 之前漏存, 现加上 (Word3 of RS485 config)
    pub timeout_ms: u16,  // LOOP5: Word4 of RS485 config
    pub interval_ms: u16, // LOOP5: Word5 of RS485 config
}

impl Default for Rs485Config {
    /// 与实际 UART 配置一致 (rs485::port 打开 9600 baud).
    /// 保持寄存器 Word1=0x4001 (9600bps/N/8/1/Master)，内部 mode: 0=Master。
    fn default() -> Self {
        Self {
            baudrate: 9600,
            data_bits: 8,
            stop_bits: 1,
            parity: 0,
            slave_addr: 1,
            mode: 0, // Master
            retry_count: 0,
            timeout_ms: 1000,
            interval_ms: 20,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SystemConfig {
    pub sn: [u8; 32],
    pub name: [u8; 16],
    pub hw_version: u16,
    pub fw_version: u16,
    /// 固件日期码 (Android 端 fwVersionBytesToStr 解析 dt 字段)
    /// 例如 0x0615 = 1557, 显示为 "2.2.1.1557"
    pub fw_date: u16,
    pub cfg_version: u16,

    pub eth_mac: [u8; 6],
    pub dhcp: bool,
    pub ip: [u8; 4],
    pub mask: [u8; 4],
    pub gateway: [u8; 4],
    pub dns: [u8; 4],

    pub ble_mac: [u8; 6],
    pub ble_name: [u8; 8],

    /// RS485 端口配置 (3 端口, 对齐参考固件 RS485RTU_NUM=3)
    /// - [0] = RS485-1 (UART1, 主站/从站可配, 支持 DIP 拨码覆盖地址)
    /// - [1] = RS485-2 (UART2)
    /// - [2] = RS485-3 (UART0, 与 USB 串口复用 — 仅从站监听模式)
    pub rs485: [Rs485Config; 3],
    /// 四路 Modbus TCP 监听端口，对应 HOLD_TCP_COM_BASE..+3。
    pub tcp_ports: [u16; 4],
}

/// 写入结果
///
/// `Ok` / `Persist` / `Apply` / `Reset` 都表示地址命中并已写入 cfg 字段.
/// 区别仅在调用方 (`bus::backends::write_hold_reg`) 应触发的副作用:
///
/// - `Ok`: 仅写入 CONFIG RCU, **不** 触发 NVS 持久化. 用于只读/诊断类寄存器
///   (FW_VER / CFG_VER / UNKNOWN 等).
/// - `Persist`: 写入 CONFIG RCU **且** 触发 NVS 持久化 (cfg_version 不变). 用于
///   用户可编辑但不需要重启的配置 (SN / PLACE / BLE_NAME / BLE_MESH_EN /
///   RS485 配置等). Android 1.0.78 直接 Modbus FC=10 写入, 不调 APPLY,
///   所以这里必须主动 persist 否则配置不落盘.
/// - `Apply`: 写入 + 持久化 + cfg_version++ (网络 / BLE MAC 等需要重新初始化外设
///   时使用; 网络配置不重启生效由各模块自行监听 cfg 变化).
/// - `Reset`: 恢复出厂默认 + 持久化 + apply_config (CFG_RESET_DEFAULT=0xD5D5 触发).
/// - `NotFound`: 地址不在本配置区, 调用方应尝试 holding_buf 兜底或返回 false.
#[derive(Debug, PartialEq)]
pub enum WriteResult {
    /// 普通字段写入成功 (不持久化, 用于诊断/只读字段)
    Ok,
    /// 写入 + NVS 持久化 (不重启, cfg_version 不变, 用于用户可编辑配置)
    Persist,
    /// 写入 + 持久化 + cfg_version++ (网络 / BLE 等需要重新初始化)
    Apply,
    /// 恢复出厂 + 持久化 + apply_config
    Reset,
    /// 地址不在配置区, 调用方应尝试 holding_buf 兜底
    NotFound,
}

pub const SENSOR_CHANNELS: usize = 8; // 校准寄存器通道数 (对齐参考固件 SENSOR_NUM=8)

impl SystemConfig {
    /// 默认配置 (出厂值)
    ///
    /// LOOP9: 字符串默认值改回 ASCII 编码 (与 MCA 参考固件 + metuory 1.0.78 一致).
    /// LOOP7/8 曾改为 UTF-16 BE, 但 Modbus write_reg 存储 [hi, lo] 大端字节,
    /// 与 UTF-16 BE 解码不兼容 → 手持机写入 SN/LOCATION 后读出乱码.
    /// ASCII 编码下 sn_str()/name_str()/ble_name_str() 直接按字节截取到第一个 0x00.
    pub fn defaults() -> Self {
        let mut sn = [0u8; 32];
        let sn_ascii: &[u8] = b"ESP32-001";
        sn[..sn_ascii.len()].copy_from_slice(sn_ascii);

        let mut name = [0u8; 16];
        let name_ascii: &[u8] = b"GW-ESP32";
        name[..name_ascii.len()].copy_from_slice(name_ascii);

        let mut ble_name = [0u8; 8];
        // 手持机 1.0.78 要求 BLE 名字以 "m" 开头 (忽略大小写) 才能在扫描列表中显示
        let ble_ascii: &[u8] = b"Mesh";
        ble_name[..ble_ascii.len()].copy_from_slice(ble_ascii);

        Self {
            sn,
            name,
            // MCA compatibility: F16/F3 = 0x00F3, F4 = 0x00F4.
            hw_version: crate::config::hw_version::MODEL_CODE,
            // 匹配 MCA F16 + NCA9555F16: MCA_FIRMWARE_VERSION=221, Date=0x0615
            // Android 端 fwVersionBytesToStr 解析: fw=221 → "2.2.1", dt=0x0615 → "1557"
            fw_version: 221,
            fw_date: 0x0615, // 0x0615 = 1557 (MCA 对齐)
            cfg_version: 0,
            eth_mac: [0; 6],
            dhcp: false,
            ip: [192, 168, 51, 221],
            mask: [255, 255, 255, 0],
            gateway: [192, 168, 51, 1],
            dns: [192, 168, 51, 1],
            ble_mac: [0; 6],
            ble_name,
            rs485: [
                Rs485Config::default(),
                Rs485Config::default(),
                // RS485-3 默认: 9600 baud, slave, addr=1
                Rs485Config {
                    baudrate: 9600,
                    data_bits: 8,
                    stop_bits: 1,
                    parity: 0,
                    slave_addr: 1,
                    mode: 1, // Slave
                    retry_count: 0,
                    timeout_ms: 1000,
                    interval_ms: 20,
                },
            ],
            tcp_ports: regs::TCP_PORTS_DEFAULT,
        }
    }

    /// 从 ESP-IDF 读取硬件 MAC (以太网)
    /// esp_mac_type_t: ESP_MAC_ETH=3
    pub fn read_hw_eth_mac() -> [u8; 6] {
        let mut mac = [0u8; 6];
        unsafe {
            // 第三参数为 esp_mac_type_t (c_uint), ESP_MAC_ETH=3
            esp_idf_sys::esp_read_mac(mac.as_mut_ptr(), 3);
        }
        mac
    }

    /// 从 ESP-IDF 读取硬件 MAC (蓝牙)
    /// esp_mac_type_t: ESP_MAC_BT=2
    pub fn read_hw_ble_mac() -> [u8; 6] {
        let mut mac = [0u8; 6];
        unsafe {
            esp_idf_sys::esp_read_mac(mac.as_mut_ptr(), 2);
        }
        mac
    }

    /// 从 Cargo.toml 解析固件版本 → u16
    /// Android 端 fwVersionBytesToStr 期望格式: fw = main*100 + sub*10 + tail
    ///   main = fw/100, sub = (fw%100)/10, tail = fw%10
    ///   "2.2.1" → 221, "3.3.1" → 331
    /// Cargo.toml "2.2.21" → 2*100 + 2*10 + 21 = 221 ✓
    ///           "0.1.0"  → 0*100 + 1*10 + 0 = 10  → 显示 "0.1.0"
    /// 注意: Cargo 版本最多 3 段数字, 这里只取前 3 段
    pub fn fw_version_from_cargo() -> u16 {
        let v = env!("CARGO_PKG_VERSION");
        let mut parts = v.split('.');
        let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        major
            .saturating_mul(100)
            .saturating_add(minor.saturating_mul(10))
            .saturating_add(patch)
            .min(u16::MAX as u32) as u16
    }

    /// 从 Cargo.toml 解析固件日期码 → u16 (默认 MM.DD 编码: MMDD)
    /// 例: 0x0615 = 1557 (年份隐含)
    pub fn fw_date_default() -> u16 {
        0x0615 // 与 MCA 参考固件对齐 (MCA_FIRMWARE_DATE = 0x0615)
    }

    /// 启动时从硬件读取 MAC 并填充 (仅当 NVS 中 MAC 为全 0 时)
    pub fn fill_hw_macs(&mut self) {
        if self.eth_mac == [0; 6] {
            self.eth_mac = Self::read_hw_eth_mac();
            log::info!("[cfg] eth_mac loaded from hw: {}", self.mac_str());
        }
        if self.ble_mac == [0; 6] {
            self.ble_mac = Self::read_hw_ble_mac();
            log::info!("[cfg] ble_mac loaded from hw: {}", self.ble_mac_str());
        }
    }

    // --------------------------------------------------------------------
    // 编解码
    // --------------------------------------------------------------------

    fn encode(&self) -> [u8; CFG_BLOB_SIZE] {
        let mut b = [0u8; CFG_BLOB_SIZE];
        b[OFF_SN..OFF_SN + 32].copy_from_slice(&self.sn);
        b[OFF_NAME..OFF_NAME + 16].copy_from_slice(&self.name);
        b[OFF_HW..OFF_HW + 2].copy_from_slice(&self.hw_version.to_le_bytes());
        b[OFF_FW..OFF_FW + 2].copy_from_slice(&self.fw_version.to_le_bytes());
        b[OFF_CFG_VER..OFF_CFG_VER + 2].copy_from_slice(&self.cfg_version.to_le_bytes());

        b[OFF_ETH_MAC..OFF_ETH_MAC + 6].copy_from_slice(&self.eth_mac);
        b[OFF_DHCP] = self.dhcp as u8;
        b[OFF_IP..OFF_IP + 4].copy_from_slice(&self.ip);
        b[OFF_MASK..OFF_MASK + 4].copy_from_slice(&self.mask);
        b[OFF_GW..OFF_GW + 4].copy_from_slice(&self.gateway);
        b[OFF_DNS..OFF_DNS + 4].copy_from_slice(&self.dns);

        b[OFF_BLE_MAC..OFF_BLE_MAC + 6].copy_from_slice(&self.ble_mac);
        b[OFF_BLE_NAME..OFF_BLE_NAME + 8].copy_from_slice(&self.ble_name);

        for i in 0..3 {
            let r = &self.rs485[i];
            let off = OFF_RS485_0 + i * RS485_ENTRY_SIZE;
            b[off..off + 4].copy_from_slice(&r.baudrate.to_le_bytes());
            b[off + 4] = r.data_bits;
            b[off + 5] = r.stop_bits;
            b[off + 6] = r.parity;
            b[off + 7] = r.slave_addr;
            b[off + 8] = r.mode;
            b[off + 9..off + 11].copy_from_slice(&r.retry_count.to_le_bytes());
            b[off + 11..off + 13].copy_from_slice(&r.timeout_ms.to_le_bytes());
            b[off + 13..off + 15].copy_from_slice(&r.interval_ms.to_le_bytes());
        }
        for (i, port) in self.tcp_ports.iter().enumerate() {
            let off = OFF_TCP_PORTS + i * 2;
            b[off..off + 2].copy_from_slice(&port.to_le_bytes());
        }
        b
    }

    fn decode(b: &[u8]) -> Self {
        if b.len() < CFG_BLOB_LEGACY_SIZE {
            return Self::defaults();
        }
        let mut s = Self::defaults();

        s.sn.copy_from_slice(&b[OFF_SN..OFF_SN + 32]);
        s.name.copy_from_slice(&b[OFF_NAME..OFF_NAME + 16]);
        s.hw_version = u16::from_le_bytes([b[OFF_HW], b[OFF_HW + 1]]);
        s.fw_version = u16::from_le_bytes([b[OFF_FW], b[OFF_FW + 1]]);
        s.cfg_version = u16::from_le_bytes([b[OFF_CFG_VER], b[OFF_CFG_VER + 1]]);

        s.eth_mac.copy_from_slice(&b[OFF_ETH_MAC..OFF_ETH_MAC + 6]);
        s.dhcp = b[OFF_DHCP] != 0;
        s.ip.copy_from_slice(&b[OFF_IP..OFF_IP + 4]);
        s.mask.copy_from_slice(&b[OFF_MASK..OFF_MASK + 4]);
        s.gateway.copy_from_slice(&b[OFF_GW..OFF_GW + 4]);
        s.dns.copy_from_slice(&b[OFF_DNS..OFF_DNS + 4]);

        s.ble_mac.copy_from_slice(&b[OFF_BLE_MAC..OFF_BLE_MAC + 6]);
        s.ble_name
            .copy_from_slice(&b[OFF_BLE_NAME..OFF_BLE_NAME + 8]);

        for i in 0..3 {
            let off = OFF_RS485_0 + i * RS485_ENTRY_SIZE;
            let baud = u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]);
            s.rs485[i] = Rs485Config {
                baudrate: if baud == 0 { 9600 } else { baud },
                data_bits: if b[off + 4] == 0 { 8 } else { b[off + 4] },
                stop_bits: if b[off + 5] == 0 { 1 } else { b[off + 5] },
                parity: b[off + 6],
                slave_addr: b[off + 7],
                mode: b[off + 8],
                retry_count: u16::from_le_bytes([b[off + 9], b[off + 10]]),
                timeout_ms: u16::from_le_bytes([b[off + 11], b[off + 12]]),
                interval_ms: u16::from_le_bytes([b[off + 13], b[off + 14]]),
            };
        }
        if b.len() >= OFF_TCP_PORTS + 8 {
            for i in 0..4 {
                let off = OFF_TCP_PORTS + i * 2;
                let port = u16::from_le_bytes([b[off], b[off + 1]]);
                s.tcp_ports[i] = if port == 0 {
                    regs::TCP_PORTS_DEFAULT[i]
                } else {
                    port
                };
            }
        }
        s
    }

    // --------------------------------------------------------------------
    // NVS 持久化
    // --------------------------------------------------------------------

    pub fn load_from_nvs(nvs: &EspDefaultNvs) -> AppResult<Self> {
        let magic = nvs
            .get_u32(NVS_KEY_MAGIC)
            .map_err(|e| AppError::Config(format!("nvs get magic: {e:?}")))?
            .unwrap_or(0);

        if magic != NVS_MAGIC {
            log::info!("[cfg] nvs empty, using defaults");
            return Ok(Self::defaults());
        }

        // esp_idf_svc 0.50: get_blob 返回 Option<&[u8]>
        let mut buf = [0u8; CFG_BLOB_SIZE];
        let blob = nvs
            .get_blob(NVS_KEY_BLOB, &mut buf)
            .map_err(|e| AppError::Config(format!("nvs get blob: {e:?}")))?;

        let data = match blob {
            Some(d) => d,
            None => {
                log::warn!("[cfg] nvs blob missing, using defaults");
                return Ok(Self::defaults());
            }
        };

        if data.len() < CFG_BLOB_LEGACY_SIZE {
            log::warn!(
                "[cfg] nvs blob truncated: {}/{}",
                data.len(),
                CFG_BLOB_LEGACY_SIZE
            );
            return Ok(Self::defaults());
        }

        Ok(Self::decode(data))
    }

    pub fn save_to_nvs(&self, nvs: &mut EspDefaultNvs) -> AppResult<()> {
        let buf = self.encode();
        nvs.set_blob(NVS_KEY_BLOB, &buf)
            .map_err(|e| AppError::Config(format!("nvs set blob: {e:?}")))?;
        nvs.set_u32(NVS_KEY_MAGIC, NVS_MAGIC)
            .map_err(|e| AppError::Config(format!("nvs set magic: {e:?}")))?;
        Ok(())
    }

    // --------------------------------------------------------------------
    // Modbus 寄存器读写
    // --------------------------------------------------------------------

    pub fn read_reg(&self, addr: u16) -> Option<u16> {
        // 485 通信错误计数 (0x0880-0x0883, RO) — MCA cold boot 初值 0; 一旦
        // Modbus master 写入会落入 `holding_buf` 兜底, 再读则照实返回 (与 MCA 等价)。
        if addr == regs::HOLD_485_1_COMERR
            || addr == regs::HOLD_485_1_APPERR
            || addr == regs::HOLD_485_2_COMERR
            || addr == regs::HOLD_485_2_APPERR
        {
            return Some(0);
        }
        // SN 0x0200-0x020F
        if (regs::HOLD_SN_BASE..regs::HOLD_SN_BASE + regs::HOLD_SN_COUNT).contains(&addr) {
            let idx = (addr - regs::HOLD_SN_BASE) as usize * 2;
            return Some(u16::from_be_bytes([self.sn[idx], self.sn[idx + 1]]));
        }
        // name 0x0210-0x0217
        if (regs::HOLD_PLACE_BASE..regs::HOLD_PLACE_BASE + regs::HOLD_PLACE_COUNT).contains(&addr) {
            let idx = (addr - regs::HOLD_PLACE_BASE) as usize * 2;
            return Some(u16::from_be_bytes([self.name[idx], self.name[idx + 1]]));
        }
        // 设备信息
        match addr {
            regs::HOLD_HW_VER => return Some(self.hw_version),
            regs::CFG_FW_VER => return Some(self.fw_version),
            regs::CFG_CFG_VER => return Some(self.cfg_version),
            regs::CFG_APPLY => return Some(0),
            regs::CFG_RESET_DEFAULT => return Some(0),
            _ => {}
        }
        // 网络
        if addr == regs::CFG_DHCP {
            return Some(self.dhcp as u16);
        }
        if let Some(v) = read_ipv4(&self.ip, regs::HOLD_IP_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_ipv4(&self.mask, regs::HOLD_MASK_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_ipv4(&self.gateway, regs::HOLD_GW_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_ipv4(&self.dns, regs::HOLD_DNS_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_mac(&self.eth_mac, regs::HOLD_MAC_BASE, addr) {
            return Some(v);
        }
        // BLE 节点名称兼容窗口。D98..D101 (2274..2277) 与 FC=04
        // 0x08E2..0x08E5 访问同一份 ble_name，避免 PC/手持机分别维护两份值。
        if (regs::HOLD_BLE_ADDR_BASE..regs::HOLD_BLE_ADDR_BASE + regs::HOLD_BLE_ADDR_COUNT)
            .contains(&addr)
        {
            let idx = (addr - regs::HOLD_BLE_ADDR_BASE) as usize * 2;
            return Some(u16::from_be_bytes([
                self.ble_name[idx],
                self.ble_name[idx + 1],
            ]));
        }
        // 前 3 个物理 RS485 端口。BT/NET 两个兼容块由通用 PRegBuf 保存，
        // 不能别名到 rs485[0]，否则 PC 写 NET 会覆盖 RS485-1。
        for i in 0..self.rs485.len() {
            let base = regs::HOLD_RS485_BASE + (i as u16) * regs::HOLD_RS485_STRIDE;
            if (base..base + 5).contains(&addr) {
                let off = (addr - base) as usize;
                let r = &self.rs485[i];
                return Some(match off {
                    // PC/C++ PReg 编码与 BLE packed nibble 不同：
                    // baud 0/4=9600, 9=115200；低字节 1=Master, 0=Slave。
                    0 => {
                        let baud_idx = match r.baudrate {
                            1200 => 1,
                            2400 => 2,
                            4800 => 3,
                            9600 => 4,
                            14400 => 5,
                            19200 => 6,
                            38400 => 7,
                            57600 => 8,
                            115200 => 9,
                            128000 => 10,
                            153600 => 11,
                            230400 => 12,
                            256000 => 13,
                            460800 => 14,
                            921600 => 15,
                            _ => 4,
                        };
                        let port_mode = if r.mode == 0 { 1u16 } else { 0u16 };
                        (baud_idx << 12)
                            | ((r.parity as u16 & 0x3) << 10)
                            | ((r.stop_bits.saturating_sub(1) as u16 & 0x1) << 9)
                            | ((if r.data_bits == 7 { 1u16 } else { 0u16 }) << 8)
                            | port_mode
                    }
                    1 => r.slave_addr as u16, // Word 2: Slave ID
                    2 => r.retry_count,       // Word 3: Retry count (LOOP5 修复)
                    3 => r.timeout_ms,        // Word 4: Response timeout (LOOP5 修复)
                    4 => r.interval_ms,       // Word 5: Delay between polls (LOOP5 修复)
                    _ => 0,
                });
            }
        }
        // 2239-2242 是 PC 工具 local_port1..4，走 PRegBuf 以支持读写回环。
        // TCP COM 端口 (2243-2246)
        if (regs::HOLD_TCP_COM_BASE..regs::HOLD_TCP_COM_BASE + regs::HOLD_TCP_COM_COUNT)
            .contains(&addr)
        {
            let idx = (addr - regs::HOLD_TCP_COM_BASE) as usize;
            return self.tcp_ports.get(idx).copied();
        }
        None
    }

    pub fn write_reg(&mut self, addr: u16, value: u16) -> WriteResult {
        // SN — 用户可编辑配置, 写入后立即持久化到 NVS.
        // Android 1.0.78 WRITE_SN (0x21) 不调 APPLY, 必须 Persist 才能落盘.
        if (regs::HOLD_SN_BASE..regs::HOLD_SN_BASE + regs::HOLD_SN_COUNT).contains(&addr) {
            let idx = (addr - regs::HOLD_SN_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.sn[idx] = hi;
            self.sn[idx + 1] = lo;
            return WriteResult::Persist;
        }
        // 位置信息 (Android LOCATION) — 用户可编辑, Persist.
        if (regs::HOLD_PLACE_BASE..regs::HOLD_PLACE_BASE + regs::HOLD_PLACE_COUNT).contains(&addr) {
            let idx = (addr - regs::HOLD_PLACE_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.name[idx] = hi;
            self.name[idx + 1] = lo;
            return WriteResult::Persist;
        }
        match addr {
            // LOOP10: HW_VER 写入必须持久化 (对齐 MCA PRegBuf → /MODSPRegSaveBuf.bin)
            regs::HOLD_HW_VER => {
                self.hw_version = value;
                return WriteResult::Persist;
            }
            regs::CFG_FW_VER => {
                self.fw_version = value;
                return WriteResult::Ok;
            }
            regs::CFG_CFG_VER => {
                self.cfg_version = value;
                return WriteResult::Ok;
            }
            regs::CFG_APPLY => {
                if value == 0xB5B5 {
                    return WriteResult::Apply;
                }
                return WriteResult::Ok;
            }
            regs::CFG_RESET_DEFAULT => {
                if value == 0xD5D5 {
                    return WriteResult::Reset;
                }
                return WriteResult::Ok;
            }
            _ => {}
        }
        // 网络 — 写后需 Apply 重启生效 (参考固件: SET_NET_INFO → restart)
        if addr == regs::CFG_DHCP {
            self.dhcp = value != 0;
            return WriteResult::Apply;
        }
        if let Some(()) = write_ipv4(&mut self.ip, regs::HOLD_IP_BASE, addr, value) {
            return WriteResult::Apply;
        }
        if let Some(()) = write_ipv4(&mut self.mask, regs::HOLD_MASK_BASE, addr, value) {
            return WriteResult::Apply;
        }
        if let Some(()) = write_ipv4(&mut self.gateway, regs::HOLD_GW_BASE, addr, value) {
            return WriteResult::Apply;
        }
        if let Some(()) = write_ipv4(&mut self.dns, regs::HOLD_DNS_BASE, addr, value) {
            return WriteResult::Apply;
        }
        if let Some(()) = write_mac(&mut self.eth_mac, regs::HOLD_MAC_BASE, addr, value) {
            return WriteResult::Apply;
        }
        // BLE 节点名称兼容窗口。D98..D101 写入必须更新唯一权威 ble_name，
        // 与 FC=04 0x08E2..0x08E5、BLE 自定义 0xCB 写入保持一致。
        if (regs::HOLD_BLE_ADDR_BASE..regs::HOLD_BLE_ADDR_BASE + regs::HOLD_BLE_ADDR_COUNT)
            .contains(&addr)
        {
            let idx = (addr - regs::HOLD_BLE_ADDR_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.ble_name[idx] = hi;
            self.ble_name[idx + 1] = lo;
            return WriteResult::Persist;
        }
        // 前 3 个物理 RS485；后两个 BT/NET 兼容块走 PRegBuf。
        for i in 0..self.rs485.len() {
            let base = regs::HOLD_RS485_BASE + (i as u16) * regs::HOLD_RS485_STRIDE;
            if (base..base + 5).contains(&addr) {
                let off = (addr - base) as usize;
                let r = &mut self.rs485[i];
                match off {
                    0 => {
                        let baud_idx = ((value >> 12) & 0xF) as u32;
                        let baud_table: [u32; 16] = [
                            9600, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200,
                            128000, 153600, 230400, 256000, 460800, 921600,
                        ];
                        r.baudrate = *baud_table.get(baud_idx as usize).unwrap_or(&9600);
                        r.parity = ((value >> 10) & 0x3) as u8;
                        r.stop_bits = (((value >> 9) & 0x1) + 1) as u8;
                        r.data_bits = if (value >> 8) & 0x1 != 0 { 7 } else { 8 };
                        r.mode = if value & 0xFF == 1 { 0 } else { 1 };
                    }
                    1 => r.slave_addr = value as u8,
                    2 => r.retry_count = value, // LOOP5 修复: 之前 2..=4 => {} 漏存 retry/timeout/interval
                    3 => r.timeout_ms = value,
                    4 => r.interval_ms = value,
                    _ => {}
                }
                // RS485 配置 — 用户可编辑, Persist (运行时切换由 rs485::port 监听).
                return WriteResult::Persist;
            }
        }
        // 2239-2242 由调用方落入 PRegBuf，不能假成功后丢弃 PC 工具写入。
        // TCP COM 端口 (2243-2246) — 运行时可写, Persist 落盘 (对齐参考固件)
        if (regs::HOLD_TCP_COM_BASE..regs::HOLD_TCP_COM_BASE + regs::HOLD_TCP_COM_COUNT)
            .contains(&addr)
        {
            if value == 0 {
                return WriteResult::NotFound;
            }
            let idx = (addr - regs::HOLD_TCP_COM_BASE) as usize;
            self.tcp_ports[idx] = value;
            return WriteResult::Persist;
        }
        WriteResult::NotFound
    }

    // --------------------------------------------------------------------
    // 辅助展示
    // --------------------------------------------------------------------

    pub fn sn_str(&self) -> String {
        // LOOP9: 改回 ASCII 字节解码 (LOOP7 错误引入 UTF-16 BE)
        let end = self
            .sn
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.sn.len());
        String::from_utf8_lossy(&self.sn[..end]).into_owned()
    }

    pub fn name_str(&self) -> String {
        // LOOP9: 改回 ASCII 字节解码
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len());
        String::from_utf8_lossy(&self.name[..end]).into_owned()
    }

    pub fn ble_name_str(&self) -> String {
        // LOOP9: 改回 ASCII 字节解码, 与 metuory 1.0.78 parseBluetoothIDItem 一致
        // (BLE 名字最多 8 字符, ASCII 编码)
        let end = self
            .ble_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.ble_name.len());
        String::from_utf8_lossy(&self.ble_name[..end]).into_owned()
    }

    pub fn ip_str(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.ip[0], self.ip[1], self.ip[2], self.ip[3]
        )
    }

    pub fn mask_str(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.mask[0], self.mask[1], self.mask[2], self.mask[3]
        )
    }

    pub fn gw_str(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.gateway[0], self.gateway[1], self.gateway[2], self.gateway[3]
        )
    }

    pub fn dns_str(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.dns[0], self.dns[1], self.dns[2], self.dns[3]
        )
    }

    pub fn mac_str(&self) -> String {
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.eth_mac[0],
            self.eth_mac[1],
            self.eth_mac[2],
            self.eth_mac[3],
            self.eth_mac[4],
            self.eth_mac[5]
        )
    }

    pub fn ble_mac_str(&self) -> String {
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.ble_mac[0],
            self.ble_mac[1],
            self.ble_mac[2],
            self.ble_mac[3],
            self.ble_mac[4],
            self.ble_mac[5]
        )
    }
}

// ----------------------------------------------------------------------------
// 字段编解码辅助
// ----------------------------------------------------------------------------

/// IPv4: 4 字节 → 4 个 U16, 每字 1 字节 (参考固件: SLAVE_REG_PIP1..PIP4)
fn read_ipv4(bytes: &[u8; 4], base: u16, addr: u16) -> Option<u16> {
    let off = addr.checked_sub(base)?;
    if off >= 4 {
        return None;
    }
    Some(bytes[off as usize] as u16)
}

fn write_ipv4(bytes: &mut [u8; 4], base: u16, addr: u16, value: u16) -> Option<()> {
    let off = addr.checked_sub(base)?;
    if off >= 4 {
        return None;
    }
    bytes[off as usize] = (value & 0xFF) as u8;
    Some(())
}

/// MAC: 6 字节 → 6 个 U16, 每字 1 字节 (参考固件: SLAVE_REG_MAC1..MAC6)
fn read_mac(bytes: &[u8; 6], base: u16, addr: u16) -> Option<u16> {
    let off = addr.checked_sub(base)?;
    if off >= 6 {
        return None;
    }
    Some(bytes[off as usize] as u16)
}

fn write_mac(bytes: &mut [u8; 6], base: u16, addr: u16, value: u16) -> Option<()> {
    let off = addr.checked_sub(base)?;
    if off >= 6 {
        return None;
    }
    bytes[off as usize] = (value & 0xFF) as u8;
    Some(())
}

/// 解析 "192.168.51.221" → [192,168,51,221]
pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p.parse().ok()?;
    }
    Some(out)
}

/// 解析 "AA:BB:CC:DD:EE:FF" → [0xAA,0xBB,0xCC,0xDD,0xEE,0xFF]
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

// ============================================================================
// 单元测试 — SystemConfig 字段读写
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = SystemConfig::defaults();
        assert_eq!(cfg.dhcp, false);
        assert_eq!(cfg.ip, [192, 168, 51, 221]);
        assert_eq!(cfg.eth_mac, [0; 6]);
        assert_eq!(cfg.rs485.len(), 3);
    }

    #[test]
    fn test_parse_ipv4() {
        assert_eq!(parse_ipv4("192.168.1.1"), Some([192, 168, 1, 1]));
        assert_eq!(parse_ipv4("0.0.0.0"), Some([0, 0, 0, 0]));
        assert_eq!(parse_ipv4("255.255.255.255"), Some([255, 255, 255, 255]));
        assert_eq!(parse_ipv4(""), None);
        assert_eq!(parse_ipv4("256.0.0.0"), None);
        assert_eq!(parse_ipv4("192.168.1"), None);
    }

    #[test]
    fn test_parse_mac() {
        assert_eq!(
            parse_mac("AA:BB:CC:DD:EE:FF"),
            Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
        assert_eq!(
            parse_mac("00:08:DC:11:22:33"),
            Some([0x00, 0x08, 0xDC, 0x11, 0x22, 0x33])
        );
        assert_eq!(
            parse_mac("aa:bb:cc:dd:ee:ff"),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
        );
        assert_eq!(parse_mac("invalid"), None);
        assert_eq!(parse_mac("AA:BB:CC"), None);
    }

    #[test]
    fn test_ip_str_format() {
        let cfg = SystemConfig::defaults();
        let s = cfg.ip_str();
        assert!(s.contains("192.168.51.221"));
    }

    #[test]
    fn test_read_reg_unmapped_returns_none() {
        let cfg = SystemConfig::defaults();
        // 不在已知映射中的地址返回 None
        assert_eq!(cfg.read_reg(0xFFFF), None);
    }

    #[test]
    fn test_read_reg_fw_ver() {
        let cfg = SystemConfig::defaults();
        // FW_VER 在输入寄存器区
        assert!(cfg.read_reg(0x087E).is_some());
    }

    #[test]
    fn test_write_reg_apply_request() {
        let mut cfg = SystemConfig::defaults();
        // CFG_APPLY = 0xFF01, 写任意值触发 Apply
        let result = cfg.write_reg(0xFF01, 0x0000);
        assert_eq!(result, WriteResult::Apply);
    }

    #[test]
    fn test_write_reg_reset_request() {
        let mut cfg = SystemConfig::defaults();
        // CFG_RESET_DEFAULT = 0xFF02, 写 0xD5D5 触发 Reset
        let result = cfg.write_reg(0xFF02, 0xD5D5);
        assert_eq!(result, WriteResult::Reset);
    }

    #[test]
    fn test_write_reg_dhcp() {
        let mut cfg = SystemConfig::defaults();
        let result = cfg.write_reg(0xFF03, 1);
        assert_eq!(result, WriteResult::Apply);
        // cfg.dhcp is private; assert via behavior
        let result2 = cfg.write_reg(0xFF03, 0);
        assert_eq!(result2, WriteResult::Apply);
    }

    #[test]
    fn test_str_format_helpers() {
        let cfg = SystemConfig::defaults();
        // 默认 SN 应该是 32 字节零填充
        let sn = cfg.sn_str();
        assert!(sn.len() <= 32);
        // 默认 BLE 名称
        let name = cfg.ble_name_str();
        assert!(name.len() <= 16);
    }

    /// LOOP9 回归: 默认值改回 ASCII 编码 (与 MCA + metuory 1.0.78 一致).
    /// 手持机 1.0.78 要求 BLE 名字以 "m" 开头 (忽略大小写) 才在扫描列表显示.
    #[test]
    fn test_defaults_ascii_decoded() {
        let cfg = SystemConfig::defaults();
        // 默认 BLE 名字 = "Mesh", 必须以 'M'/'m' 开头 (手持机过滤要求)
        let ble = cfg.ble_name_str();
        assert_eq!(
            ble, "Mesh",
            "default ble_name must decode to 'Mesh' (got '{ble}')"
        );
        let first = ble.chars().next().unwrap();
        assert!(
            first == 'M' || first == 'm',
            "BLE name must start with m/M for handheld visibility"
        );
        // 默认 SN = "ESP32-001"
        assert_eq!(cfg.sn_str(), "ESP32-001");
        // 默认设备名 = "GW-ESP32"
        assert_eq!(cfg.name_str(), "GW-ESP32");
    }

    // ========================================================================
    // 写入语义测试 (阶段 1: P0-B NVS 持久化基线)
    // ========================================================================
    //
    // 关键约定:
    // - 用户可编辑配置 (SN/PLACE/BLE_NAME/RS485/BLE_MESH_EN) → Persist
    // - 诊断/只读字段 (FW_VER/UNKNOW) → Ok (不持久化)
    // - 网络/BLE MAC → Apply (需重启或重新初始化)
    // - CFG_APPLY (0xB5B5) → Apply
    // - CFG_RESET_DEFAULT (0xD5D5) → Reset

    /// 写入 SN (0x0894) 必须返回 Persist (Android WRITE_SN 不调 APPLY).
    #[test]
    fn test_write_reg_sn_persist() {
        let mut cfg = SystemConfig::defaults();
        let result = cfg.write_reg(regs::HOLD_SN_BASE, 0x4142); // "AB"
        assert_eq!(result, WriteResult::Persist);
    }

    /// 写入 PLACE (0x089D) 必须返回 Persist (Android WRITE_LOCATION).
    #[test]
    fn test_write_reg_place_persist() {
        let mut cfg = SystemConfig::defaults();
        let result = cfg.write_reg(regs::HOLD_PLACE_BASE, 0x2021); // " !"
        assert_eq!(result, WriteResult::Persist);
    }

    /// D98..D101 是旧 MCA 的 BLE 节点名称兼容窗口，必须别名到 ble_name。
    #[test]
    fn test_ble_name_hold_alias_roundtrip() {
        let mut cfg = SystemConfig::defaults();
        let original_mac = cfg.ble_mac;
        for (offset, word) in [0x4142, 0x4344, 0x4546, 0x4748].into_iter().enumerate() {
            let result = cfg.write_reg(regs::HOLD_BLE_ADDR_BASE + offset as u16, word);
            assert_eq!(result, WriteResult::Persist);
            assert_eq!(
                cfg.read_reg(regs::HOLD_BLE_ADDR_BASE + offset as u16),
                Some(word)
            );
        }
        assert_eq!(&cfg.ble_name, b"ABCDEFGH");
        assert_eq!(
            cfg.ble_mac, original_mac,
            "BLE hardware MAC must stay independent"
        );
    }

    #[test]
    fn test_ble_name_input_and_hold_windows_are_same_value() {
        let mut cfg = SystemConfig::defaults();
        cfg.ble_name = *b"Mesh001\0";
        for offset in 0..regs::HOLD_BLE_ADDR_COUNT {
            let hold = cfg
                .read_reg(regs::HOLD_BLE_ADDR_BASE + offset)
                .expect("D98..D101 must be readable");
            let input = cfg
                .read_reg(regs::INREG_BLE_ID_BASE + offset)
                .expect("FC=04 BLE ID window must be readable");
            assert_eq!(hold, input, "BLE ID windows differ at word {offset}");
        }
    }

    /// 写入 RS485 配置 (HOLD_RS485_BASE) 必须返回 Persist.
    #[test]
    fn test_write_reg_rs485_persist() {
        let mut cfg = SystemConfig::defaults();
        // PC/C++ Word1 = 0x4001 (9600/N/8/1/Master)
        let result = cfg.write_reg(regs::HOLD_RS485_BASE, 0x4001);
        assert_eq!(result, WriteResult::Persist);
    }

    /// LOOP10: HW_VER 写入必须返回 Persist (对齐 MCA PRegBuf 持久化)
    #[test]
    fn test_write_reg_hw_ver_persist() {
        let mut cfg = SystemConfig::defaults();
        let result = cfg.write_reg(regs::HOLD_HW_VER, 0x00F3);
        assert_eq!(result, WriteResult::Persist);
        assert_eq!(cfg.hw_version, 0x00F3);
    }

    /// PC local_port1..4 必须交给通用 PRegBuf，不能在 SystemConfig 中假成功丢弃。
    #[test]
    fn test_write_reg_pc_client_port_falls_back_to_preg() {
        let mut cfg = SystemConfig::defaults();
        let result = cfg.write_reg(regs::HOLD_UNKNOWN_BASE, 0x1234);
        assert_eq!(result, WriteResult::NotFound);
        assert_eq!(cfg.read_reg(regs::HOLD_UNKNOWN_BASE), None);
    }

    /// TCP_COM 端口 (2243-2246) 写入必须持久化并可读回。
    #[test]
    fn test_write_reg_tcp_com_ok() {
        let mut cfg = SystemConfig::defaults();
        let result = cfg.write_reg(regs::HOLD_TCP_COM_BASE, 502);
        assert_eq!(result, WriteResult::Persist);
        assert_eq!(cfg.read_reg(regs::HOLD_TCP_COM_BASE), Some(502));
    }

    /// IP 写入必须返回 Apply (网络需重新初始化).
    #[test]
    fn test_write_reg_ip_apply() {
        let mut cfg = SystemConfig::defaults();
        let result = cfg.write_reg(regs::HOLD_IP_BASE, 0xC0A8); // 192.168
        assert_eq!(result, WriteResult::Apply);
    }

    /// 4000+ 是连续逻辑区，SystemConfig 必须让出给 PRegBuf。
    #[test]
    fn test_user_logic_area_falls_back_to_preg() {
        let mut cfg = SystemConfig::defaults();
        let addr = regs::HOLD_USER_BASE + 4;
        let result = cfg.write_reg(addr, 0xAABB);
        assert_eq!(result, WriteResult::NotFound);
        assert_eq!(cfg.read_reg(addr), None);
    }

    // ========================================================================
    // 回归测试: BLE NAME (0x08E2) / RS485 (0x08A6) 持久化路径
    // 背景: metuory 1.0.78 通过 BLE 写 0x08E2 期望更新蓝牙 ID (ble_name), 不是 BLE MAC.
    //       旧代码误路由到 ble_mac (Persist/Apply 但写错字段), 重启后显示丢失.
    //       同样 RS485 (0x08A6) 也必须正确路由并 Persist, 否则重启后回归默认 9600/8/N/1.
    // ========================================================================

    /// metuory 写入 0x08E2 (4 regs) → 必须 Persist + 写入 ble_name, 不污染 ble_mac
    #[test]
    fn test_ble_name_at_metuory_addr_is_persist() {
        let mut cfg = SystemConfig::defaults();
        let original_mac = cfg.ble_mac; // 不应被改
        // 模拟 metuory WRITE_BLUETOOTH_ID: 写 4 个寄存器到 0x08E2
        // "AB" = 0x4142 (大端 BE), "CD" = 0x4344
        let result0 = cfg.write_reg(0x08E2, 0x4142);
        let result1 = cfg.write_reg(0x08E3, 0x4344);
        let result2 = cfg.write_reg(0x08E4, 0x4500); // 'E' + padding
        let result3 = cfg.write_reg(0x08E5, 0x0000);
        assert_eq!(
            result0,
            WriteResult::Persist,
            "metuory BLE ID write must trigger NVS persist"
        );
        assert_eq!(result1, WriteResult::Persist);
        assert_eq!(result2, WriteResult::Persist);
        assert_eq!(result3, WriteResult::Persist);
        // 验证 ble_name 已被更新 (BE 编码)
        assert_eq!(&cfg.ble_name[0..2], b"AB", "ble_name[0..2] should be AB");
        assert_eq!(&cfg.ble_name[2..4], b"CD", "ble_name[2..4] should be CD");
        // 关键: ble_mac 必须保持不变
        assert_eq!(
            cfg.ble_mac, original_mac,
            "ble_mac must NOT be touched by metuory BLE ID write"
        );
    }

    /// 0x0FA4 不得再截断 PC 场景逻辑数据。
    #[test]
    fn test_legacy_ble_mac_alias_no_longer_shadows_logic_data() {
        let mut cfg = SystemConfig::defaults();
        let original = cfg.ble_mac;
        let result = cfg.write_reg(0x0FA4, 0xAABB);
        assert_eq!(result, WriteResult::NotFound);
        assert_eq!(cfg.ble_mac, original);
    }

    /// metuory 读 0x08E2 (4 regs) → 必须返回 ble_name (不是 ble_mac)
    #[test]
    fn test_ble_name_read_at_metuory_addr() {
        let mut cfg = SystemConfig::defaults();
        cfg.ble_name = *b"Mesh\0\0\0\0";
        cfg.ble_mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let v0 = cfg.read_reg(0x08E2).expect("must be Some");
        let v1 = cfg.read_reg(0x08E3).expect("must be Some");
        // ble_name[0..2] = "Me" = 0x4D 0x65 → BE u16 = 0x4D65
        assert_eq!(
            v0, 0x4D65,
            "0x08E2 read should return ble_name[0..2] BE = Me"
        );
        assert_eq!(
            v1, 0x7368,
            "0x08E3 read should return ble_name[2..4] BE = sh"
        );
    }

    /// 物理 RS485 (0x08A6..0x08B4, 3 端口 × 5 字) Persist 路径完整覆盖。
    /// 后两个 BT/NET 块由 PRegBuf 持久化，禁止覆盖 rs485[0]。
    #[test]
    fn test_rs485_persist_three_physical_ports() {
        let mut cfg = SystemConfig::defaults();
        // PC/C++: 115200 / Even / 1 stop / 8 data / Master = 0x9801
        let word1 = 0x9801u16;
        // idx=0, baud=115200, parity=Even, stop=1, data=8, mode=0
        let r0 = cfg.write_reg(0x08A6, word1);
        let r1 = cfg.write_reg(0x08A7, 5); // slave=5
        let r2 = cfg.write_reg(0x08A8, 3); // retry=3
        let r3 = cfg.write_reg(0x08A9, 500); // timeout=500ms
        let r4 = cfg.write_reg(0x08AA, 50); // interval=50ms
        for r in [r0, r1, r2, r3, r4] {
            assert_eq!(
                r,
                WriteResult::Persist,
                "RS485 reg write must Persist (not Ok/Apply)"
            );
        }
        assert_eq!(cfg.rs485[0].baudrate, 115200);
        assert_eq!(cfg.rs485[0].parity, 2);
        assert_eq!(cfg.rs485[0].stop_bits, 1);
        assert_eq!(cfg.rs485[0].data_bits, 8);
        assert_eq!(cfg.rs485[0].slave_addr, 5);
        // idx=1 (0x08AB..0x08AF)
        let r5 = cfg.write_reg(0x08AB, 0xF000); // idx=1 baud=921600, slave
        assert_eq!(r5, WriteResult::Persist);
        assert_eq!(cfg.rs485[1].baudrate, 921600);
        let before = cfg.rs485[0].clone();
        assert_eq!(cfg.write_reg(0x08B5, 0x9000), WriteResult::NotFound);
        assert_eq!(cfg.rs485[0].baudrate, before.baudrate);
        assert_eq!(cfg.read_reg(0x08B5), None);
    }

    /// RS485 写入后, 立即读取必须返回相同值 (RCU RMW 闭环)
    #[test]
    fn test_rs485_write_then_read_consistency() {
        let mut cfg = SystemConfig::defaults();
        // PC/C++: baud_idx=9 (115200), Odd, 2 stop, 7 data, Master(1)
        cfg.write_reg(0x08A6, 0x9701);
        cfg.write_reg(0x08A7, 42); // slave=42

        // 读回
        let w1 = cfg.read_reg(0x08A6).expect("Word1 must be Some");
        let w2 = cfg.read_reg(0x08A7).expect("Word2 must be Some");
        assert_eq!(w1, 0x9701, "PC holding encoding must round-trip exactly");
        assert_eq!(w2, 42, "round-trip word2: write 42 → read 42");
    }

    /// NVS encode → decode 闭环: 修改后保存, 加载必须等于修改后
    #[test]
    fn test_nvs_roundtrip_rs485_and_ble_name() {
        let mut cfg = SystemConfig::defaults();
        // 修改 RS485 idx=0
        cfg.rs485[0] = Rs485Config {
            baudrate: 19200,
            data_bits: 7,
            stop_bits: 2,
            parity: 1, // Odd
            slave_addr: 12,
            mode: 2, // Gateway
            retry_count: 3,
            timeout_ms: 500,
            interval_ms: 50,
        };
        // 修改 BLE NAME
        cfg.ble_name = *b"Test1234";
        // 修改 ETH MAC
        cfg.eth_mac = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE];
        // 修改 IP
        cfg.ip = [10, 20, 30, 40];
        cfg.tcp_ports = [1502, 1503, 1504, 1505];

        // encode → decode
        let buf = cfg.encode();
        assert_eq!(buf.len(), CFG_BLOB_SIZE);
        let decoded = SystemConfig::decode(&buf);

        assert_eq!(decoded.rs485[0].baudrate, 19200);
        assert_eq!(decoded.rs485[0].data_bits, 7);
        assert_eq!(decoded.rs485[0].stop_bits, 2);
        assert_eq!(decoded.rs485[0].parity, 1);
        assert_eq!(decoded.rs485[0].slave_addr, 12);
        assert_eq!(decoded.rs485[0].mode, 2);
        assert_eq!(&decoded.ble_name[..8], b"Test1234");
        assert_eq!(decoded.eth_mac, [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE]);
        assert_eq!(decoded.ip, [10, 20, 30, 40]);
        assert_eq!(decoded.tcp_ports, [1502, 1503, 1504, 1505]);
        // BLE MAC 默认 (HW) 在 encode/decode 后应保持不变
        assert_eq!(decoded.ble_mac, cfg.ble_mac);
    }

    // ========================================================================
    // 寄存器布局对照 MCA (目标 #3: 与 MCA 每一个地址都不能偏差)
    // ========================================================================

    /// MCA SLAVE_REG_SN1 = 2196 = 0x894
    #[test]
    fn test_layout_sn_matches_mca() {
        assert_eq!(
            regs::HOLD_SN_BASE,
            0x0894,
            "SN base must be 0x0894 (MCA 2196)"
        );
        assert_eq!(
            regs::HOLD_SN_COUNT,
            9,
            "SN count must be 9 (MCA 18 chars UTF-16)"
        );
    }

    /// MCA SLAVE_REG_PLACE1 = 2205 = 0x89D
    #[test]
    fn test_layout_place_matches_mca() {
        assert_eq!(
            regs::HOLD_PLACE_BASE,
            0x089D,
            "PLACE base must be 0x089D (MCA 2205)"
        );
        assert_eq!(
            regs::HOLD_PLACE_COUNT,
            8,
            "PLACE count must be 8 (MCA 16 chars UTF-16)"
        );
    }

    /// MCA SLAVE_REG_HW_VER = 2213 = 0x8A5
    #[test]
    fn test_layout_hw_ver_matches_mca() {
        assert_eq!(
            regs::HOLD_HW_VER,
            0x08A5,
            "HW_VER must be 0x08A5 (MCA 2213)"
        );
    }

    /// MCA SLAVE_REG_485_1_1 = 2214 = 0x8A6, stride=5
    #[test]
    fn test_layout_rs485_matches_mca() {
        assert_eq!(
            regs::HOLD_RS485_BASE,
            0x08A6,
            "RS485 base must be 0x08A6 (MCA 2214)"
        );
        assert_eq!(regs::HOLD_RS485_STRIDE, 5, "RS485 stride must be 5");
    }

    /// MCA SLAVE_REG_PIP1 = 2247 = 0x8C7
    #[test]
    fn test_layout_ip_matches_mca() {
        assert_eq!(
            regs::HOLD_IP_BASE,
            0x08C7,
            "IP base must be 0x08C7 (MCA 2247)"
        );
    }

    /// MCA SLAVE_REG_PNTEMASK1 = 2251 = 0x8CB
    #[test]
    fn test_layout_mask_matches_mca() {
        assert_eq!(
            regs::HOLD_MASK_BASE,
            0x08CB,
            "MASK base must be 0x08CB (MCA 2251)"
        );
    }

    /// MCA SLAVE_REG_PGW1 = 2255 = 0x8CF
    #[test]
    fn test_layout_gw_matches_mca() {
        assert_eq!(
            regs::HOLD_GW_BASE,
            0x08CF,
            "GW base must be 0x08CF (MCA 2255)"
        );
    }

    /// MCA SLAVE_REG_MAC1 = 2263 = 0x8D7
    #[test]
    fn test_layout_mac_matches_mca() {
        assert_eq!(
            regs::HOLD_MAC_BASE,
            0x08D7,
            "MAC base must be 0x08D7 (MCA 2263)"
        );
    }

    /// 旧 MCA 以 2274..2277 保存 BLE 地址；0x0FA4 保留给逻辑配置。
    #[test]
    fn test_layout_bt_addr_matches_design() {
        assert_eq!(
            regs::HOLD_BLE_ADDR_BASE,
            2274,
            "BLE address must match MCA register 2274"
        );
        assert!((regs::HOLD_USER_BASE..=regs::HOLD_CFG_END).contains(&0x0FA4));
    }

    /// HOLD_485_ERR RO 必须返回 0 (cold boot 初值, 与 MCA 一致)
    #[test]
    fn test_read_reg_485_err_ro_zero() {
        let cfg = SystemConfig::defaults();
        assert_eq!(cfg.read_reg(regs::HOLD_485_1_COMERR), Some(0));
        assert_eq!(cfg.read_reg(regs::HOLD_485_1_APPERR), Some(0));
        assert_eq!(cfg.read_reg(regs::HOLD_485_2_COMERR), Some(0));
        assert_eq!(cfg.read_reg(regs::HOLD_485_2_APPERR), Some(0));
    }

    /// 设备文本区对齐 Android 0x1388 (5000) 起始
    #[test]
    fn test_layout_device_text_matches_android() {
        assert_eq!(
            regs::DEVICE_TEXT_BASE,
            0x1388,
            "DEVICE_TEXT_BASE must be 0x1388 (Android 5000)"
        );
        assert_eq!(
            regs::DEVICE_TEXT_COUNT,
            2000,
            "DEVICE_TEXT_COUNT must be 2000"
        );
        assert_eq!(regs::DEVICE_TEXT_END, 6999);
    }

    /// INREG_AI_COUNT 必须跟随 F-version (F16=4 / F48=8, 与 MCA REG_AMAX 对齐)
    #[test]
    fn test_inreg_ai_count_matches_hw_version() {
        // 物理采 6 路 (ADC1_CH0..5), 但 Modbus 报告值跟随 hw_version
        let expected = crate::config::hw_version::AI_COUNT;
        assert_eq!(
            regs::INREG_AI_COUNT,
            crate::config::hw_version::AI_COUNT,
            "INREG_AI_COUNT must follow hw_version (F16=4 / F48=8)"
        );
        // 至少 4, 最多 8 (硬编码范围)
        assert!(
            expected >= 4 && expected <= 8,
            "AI_COUNT must be 4..=8 (MCA F16/F48)"
        );
    }
}
