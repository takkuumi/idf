//! 系统配置存储
//!
//! 集中存储所有可配置的系统参数: SN/MAC/IP/网关/RS485/BLE 等。
//! Modbus 寄存器映射见 `config::regs::CFG_*` (0x0200-0x025F)。
//!
//! 持久化:
//! - 整个 SystemConfig 序列化为 ~110 字节 blob, 单 key 存 NVS
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
/// ble_mac(6) + ble_name(8) + ble_mesh(1) = 15
/// rs485[0](9) + rs485[1](9) = 18
/// 总 = 110, 取 128 留余量
const CFG_BLOB_SIZE: usize = 128;

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
const OFF_BLE_MESH: usize = 91;
const OFF_RS485_0: usize = 92;
const OFF_RS485_1: usize = 101;

// ----------------------------------------------------------------------------
// 数据结构
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Rs485Config {
    pub baudrate: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: u8,    // 0=None 1=Odd 2=Even
    pub slave_addr: u8, // 0=主站
    pub mode: u8,       // 0=Master 1=Slave 2=Gateway
}

impl Default for Rs485Config {
    fn default() -> Self {
        Self {
            baudrate: 9600,
            data_bits: 8,
            stop_bits: 1,
            parity: 0,
            slave_addr: 1,
            mode: 1, // Slave 默认
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SystemConfig {
    pub sn: [u8; 32],
    pub name: [u8; 16],
    pub hw_version: u16,
    pub fw_version: u16,
    pub cfg_version: u16,

    pub eth_mac: [u8; 6],
    pub dhcp: bool,
    pub ip: [u8; 4],
    pub mask: [u8; 4],
    pub gateway: [u8; 4],
    pub dns: [u8; 4],

    pub ble_mac: [u8; 6],
    pub ble_name: [u8; 8],
    pub ble_mesh_enable: bool,

    pub rs485: [Rs485Config; 2],
}

/// 写入结果
pub enum WriteResult {
    /// 普通字段写入成功
    Ok,
    /// APPLY 字段写入, 调用方应触发持久化+应用
    Apply,
    /// RESET 字段写入, 调用方应恢复默认并应用
    Reset,
    /// 地址不在配置区
    NotFound,
}

impl SystemConfig {
    /// 默认配置 (出厂值)
    pub fn defaults() -> Self {
        let mut sn = [0u8; 32];
        let sn_str = b"ESP32S3-UNKNOWN-0001";
        sn[..sn_str.len()].copy_from_slice(sn_str);

        let mut name = [0u8; 16];
        let name_str = b"GW-ESP32S3";
        name[..name_str.len()].copy_from_slice(name_str);

        let mut ble_name = [0u8; 8];
        let ble_str = b"GW-C5";
        ble_name[..ble_str.len()].copy_from_slice(ble_str);

        Self {
            sn,
            name,
            hw_version: 0x0100,
            fw_version: Self::fw_version_from_cargo(),
            cfg_version: 0,
            eth_mac: [0; 6],
            dhcp: false,
            ip: [192, 168, 51, 221],
            mask: [255, 255, 255, 0],
            gateway: [192, 168, 51, 1],
            dns: [192, 168, 51, 1],
            ble_mac: [0; 6],
            ble_name,
            ble_mesh_enable: true,
            rs485: [Rs485Config::default(), Rs485Config::default()],
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

    /// 从 Cargo.toml 解析固件版本 → u16 (major<<8 | minor)
    /// "0.1.0" → 0x0001
    pub fn fw_version_from_cargo() -> u16 {
        let v = env!("CARGO_PKG_VERSION");
        let mut parts = v.split('.');
        let major: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let minor: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (major << 8) | minor
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
        b[OFF_BLE_MESH] = self.ble_mesh_enable as u8;

        for i in 0..2 {
            let r = &self.rs485[i];
            let off = if i == 0 { OFF_RS485_0 } else { OFF_RS485_1 };
            b[off..off + 4].copy_from_slice(&r.baudrate.to_le_bytes());
            b[off + 4] = r.data_bits;
            b[off + 5] = r.stop_bits;
            b[off + 6] = r.parity;
            b[off + 7] = r.slave_addr;
            b[off + 8] = r.mode;
        }
        b
    }

    fn decode(b: &[u8]) -> Self {
        if b.len() < CFG_BLOB_SIZE {
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
        s.ble_name.copy_from_slice(&b[OFF_BLE_NAME..OFF_BLE_NAME + 8]);
        s.ble_mesh_enable = b[OFF_BLE_MESH] != 0;

        for i in 0..2 {
            let off = if i == 0 { OFF_RS485_0 } else { OFF_RS485_1 };
            let baud = u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]);
            s.rs485[i] = Rs485Config {
                baudrate: if baud == 0 { 9600 } else { baud },
                data_bits: if b[off + 4] == 0 { 8 } else { b[off + 4] },
                stop_bits: if b[off + 5] == 0 { 1 } else { b[off + 5] },
                parity: b[off + 6],
                slave_addr: b[off + 7],
                mode: b[off + 8],
            };
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

        if data.len() < CFG_BLOB_SIZE {
            log::warn!("[cfg] nvs blob truncated: {}/{}", data.len(), CFG_BLOB_SIZE);
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
        // SN 0x0200-0x020F
        if (regs::CFG_SN_BASE..regs::CFG_SN_BASE + regs::CFG_SN_COUNT).contains(&addr) {
            let idx = (addr - regs::CFG_SN_BASE) as usize * 2;
            return Some(u16::from_be_bytes([self.sn[idx], self.sn[idx + 1]]));
        }
        // name 0x0210-0x0217
        if (regs::CFG_NAME_BASE..regs::CFG_NAME_BASE + regs::CFG_NAME_COUNT).contains(&addr) {
            let idx = (addr - regs::CFG_NAME_BASE) as usize * 2;
            return Some(u16::from_be_bytes([self.name[idx], self.name[idx + 1]]));
        }
        // 设备信息
        match addr {
            regs::CFG_HW_VER => return Some(self.hw_version),
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
        if let Some(v) = read_ipv4(&self.ip, regs::CFG_IP_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_ipv4(&self.mask, regs::CFG_MASK_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_ipv4(&self.gateway, regs::CFG_GW_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_ipv4(&self.dns, regs::CFG_DNS_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_mac(&self.eth_mac, regs::CFG_ETH_MAC_BASE, addr) {
            return Some(v);
        }
        if let Some(v) = read_mac(&self.ble_mac, regs::CFG_BLE_MAC_BASE, addr) {
            return Some(v);
        }
        // BLE 名称
        if (regs::CFG_BLE_NAME_BASE..regs::CFG_BLE_NAME_BASE + 4).contains(&addr) {
            let idx = (addr - regs::CFG_BLE_NAME_BASE) as usize * 2;
            return Some(u16::from_be_bytes([self.ble_name[idx], self.ble_name[idx + 1]]));
        }
        if addr == regs::CFG_BLE_MESH_EN {
            return Some(self.ble_mesh_enable as u16);
        }
        // RS485
        for i in 0..regs::CFG_RS485_COUNT as usize {
            let base = regs::CFG_RS485_BASE + (i as u16) * regs::CFG_RS485_STRIDE;
            if (base..base + 6).contains(&addr) {
                let off = (addr - base) as usize;
                let r = &self.rs485[i];
                return Some(match off {
                    0 => (r.baudrate / 100) as u16,
                    1 => r.data_bits as u16,
                    2 => r.stop_bits as u16,
                    3 => r.parity as u16,
                    4 => r.slave_addr as u16,
                    5 => r.mode as u16,
                    _ => 0,
                });
            }
        }
        None
    }

    pub fn write_reg(&mut self, addr: u16, value: u16) -> WriteResult {
        // SN
        if (regs::CFG_SN_BASE..regs::CFG_SN_BASE + regs::CFG_SN_COUNT).contains(&addr) {
            let idx = (addr - regs::CFG_SN_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.sn[idx] = hi;
            self.sn[idx + 1] = lo;
            return WriteResult::Ok;
        }
        // name
        if (regs::CFG_NAME_BASE..regs::CFG_NAME_BASE + regs::CFG_NAME_COUNT).contains(&addr) {
            let idx = (addr - regs::CFG_NAME_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.name[idx] = hi;
            self.name[idx + 1] = lo;
            return WriteResult::Ok;
        }
        match addr {
            regs::CFG_HW_VER => {
                self.hw_version = value;
                return WriteResult::Ok;
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
        // 网络
        if addr == regs::CFG_DHCP {
            self.dhcp = value != 0;
            return WriteResult::Ok;
        }
        if let Some(()) = write_ipv4(&mut self.ip, regs::CFG_IP_BASE, addr, value) {
            return WriteResult::Ok;
        }
        if let Some(()) = write_ipv4(&mut self.mask, regs::CFG_MASK_BASE, addr, value) {
            return WriteResult::Ok;
        }
        if let Some(()) = write_ipv4(&mut self.gateway, regs::CFG_GW_BASE, addr, value) {
            return WriteResult::Ok;
        }
        if let Some(()) = write_ipv4(&mut self.dns, regs::CFG_DNS_BASE, addr, value) {
            return WriteResult::Ok;
        }
        if let Some(()) = write_mac(&mut self.eth_mac, regs::CFG_ETH_MAC_BASE, addr, value) {
            return WriteResult::Ok;
        }
        if let Some(()) = write_mac(&mut self.ble_mac, regs::CFG_BLE_MAC_BASE, addr, value) {
            return WriteResult::Ok;
        }
        // BLE 名称
        if (regs::CFG_BLE_NAME_BASE..regs::CFG_BLE_NAME_BASE + 4).contains(&addr) {
            let idx = (addr - regs::CFG_BLE_NAME_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.ble_name[idx] = hi;
            self.ble_name[idx + 1] = lo;
            return WriteResult::Ok;
        }
        if addr == regs::CFG_BLE_MESH_EN {
            self.ble_mesh_enable = value != 0;
            return WriteResult::Ok;
        }
        // RS485
        for i in 0..regs::CFG_RS485_COUNT as usize {
            let base = regs::CFG_RS485_BASE + (i as u16) * regs::CFG_RS485_STRIDE;
            if (base..base + 6).contains(&addr) {
                let off = (addr - base) as usize;
                let r = &mut self.rs485[i];
                match off {
                    0 => r.baudrate = (value as u32) * 100,
                    1 => r.data_bits = value as u8,
                    2 => r.stop_bits = value as u8,
                    3 => r.parity = value as u8,
                    4 => r.slave_addr = value as u8,
                    5 => r.mode = value as u8,
                    _ => {}
                }
                return WriteResult::Ok;
            }
        }
        WriteResult::NotFound
    }

    // --------------------------------------------------------------------
    // 辅助展示
    // --------------------------------------------------------------------

    pub fn sn_str(&self) -> String {
        let end = self.sn.iter().position(|&b| b == 0).unwrap_or(32);
        String::from_utf8_lossy(&self.sn[..end]).to_string()
    }

    pub fn name_str(&self) -> String {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(16);
        String::from_utf8_lossy(&self.name[..end]).to_string()
    }

    pub fn ble_name_str(&self) -> String {
        let end = self.ble_name.iter().position(|&b| b == 0).unwrap_or(8);
        String::from_utf8_lossy(&self.ble_name[..end]).to_string()
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

/// IPv4: 4 字节 → 2 个 U16 (高字节先, 192.168.51.221 → [0xC0A8, 0x33DD])
fn read_ipv4(bytes: &[u8; 4], base: u16, addr: u16) -> Option<u16> {
    let off = addr.checked_sub(base)?;
    if off >= 2 {
        return None;
    }
    let idx = off as usize * 2;
    Some(u16::from_be_bytes([bytes[idx], bytes[idx + 1]]))
}

fn write_ipv4(bytes: &mut [u8; 4], base: u16, addr: u16, value: u16) -> Option<()> {
    let off = addr.checked_sub(base)?;
    if off >= 2 {
        return None;
    }
    let idx = off as usize * 2;
    let [hi, lo] = value.to_be_bytes();
    bytes[idx] = hi;
    bytes[idx + 1] = lo;
    Some(())
}

/// MAC: 6 字节 → 3 个 U16
fn read_mac(bytes: &[u8; 6], base: u16, addr: u16) -> Option<u16> {
    let off = addr.checked_sub(base)?;
    if off >= 3 {
        return None;
    }
    let idx = off as usize * 2;
    Some(u16::from_be_bytes([bytes[idx], bytes[idx + 1]]))
}

fn write_mac(bytes: &mut [u8; 6], base: u16, addr: u16, value: u16) -> Option<()> {
    let off = addr.checked_sub(base)?;
    if off >= 3 {
        return None;
    }
    let idx = off as usize * 2;
    let [hi, lo] = value.to_be_bytes();
    bytes[idx] = hi;
    bytes[idx + 1] = lo;
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
