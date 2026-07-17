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
    pub parity: u8,     // 0=None 1=Odd 2=Even
    pub slave_addr: u8, // 0=主站
    pub mode: u8,       // 0=Master 1=Slave 2=Gateway
}

impl Default for Rs485Config {
    /// 匹配参考固件 MODS_Init 中的 PRegBuf 默认值:
    /// Word1=0x0000 (1200bps/N/8/1/Master), Word2=1, Word3=0, Word4=1000, Word5=20
    fn default() -> Self {
        Self {
            baudrate: 1200, // BAUD_RATE[0] = 1200
            data_bits: 8,
            stop_bits: 1,
            parity: 0,
            slave_addr: 1,
            mode: 0, // Master (匹配参考固件 0x0000 低字节=0)
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
        // BLE 广播名称 (≤ 8 字节, 含 0 结尾)
        // 默认 "Mesh", 与原 C++ 固件 spp_adv_data 一致, 兼容手持机/手机扫描
        // 用户可通过 AT+CFGBTNAME=<name> 修改
        let ble_str_static = "Mesh".to_owned();
        let ble_bytes = ble_str_static.as_bytes();
        let copy_len = ble_bytes.len().min(ble_name.len());
        ble_name[..copy_len].copy_from_slice(&ble_bytes[..copy_len]);

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
        let major: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
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
        s.ble_name
            .copy_from_slice(&b[OFF_BLE_NAME..OFF_BLE_NAME + 8]);
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
        // BLE 名称 — 2字节/字 (参考固件: BT_ARRD1..BT_ARRD4)
        if (regs::HOLD_BT_ADDR_BASE..regs::HOLD_BT_ADDR_BASE + 4).contains(&addr) {
            let idx = (addr - regs::HOLD_BT_ADDR_BASE) as usize * 2;
            return Some(u16::from_be_bytes([
                self.ble_name[idx],
                self.ble_name[idx + 1],
            ]));
        }
        if addr == regs::CFG_BLE_MESH_EN {
            return Some(self.ble_mesh_enable as u16);
        }
        // RS485 — 5端口×5字, Word1=组合格式匹配参考固件
        for i in 0..5usize {
            let base = regs::HOLD_RS485_BASE + (i as u16) * regs::HOLD_RS485_STRIDE;
            if (base..base + 5).contains(&addr) {
                let off = (addr - base) as usize;
                let r = if i < self.rs485.len() {
                    &self.rs485[i]
                } else {
                    &self.rs485[0]
                };
                return Some(match off {
                    // Word 1: (baud_idx<<12)|(parity<<10)|(stop<<9)|(data<<8)|mode
                    0 => {
                        let baud_idx = match r.baudrate {
                            1200 => 0,
                            2400 => 1,
                            4800 => 2,
                            9600 => 3,
                            19200 => 4,
                            38400 => 5,
                            57600 => 6,
                            115200 => 7,
                            230400 => 8,
                            460800 => 9,
                            921600 => 10,
                            _ => 3,
                        };
                        (baud_idx << 12)
                            | ((r.parity as u16 & 0x3) << 10)
                            | ((r.stop_bits.saturating_sub(1) as u16 & 0x1) << 9)
                            | ((if r.data_bits == 7 { 1u16 } else { 0u16 }) << 8)
                            | (r.mode as u16 & 0xFF)
                    }
                    1 => r.slave_addr as u16, // Word 2: Slave ID
                    2 => 0u16,                // Word 3: Retry count (default 0)
                    3 => 1000u16,             // Word 4: Response timeout (default 1000ms)
                    4 => 20u16,               // Word 5: Delay between polls (default 20ms)
                    _ => 0,
                });
            }
        }
        // 未知保留区 (2239-2242, 参考固件默认 5500-5503)
        if (regs::HOLD_UNKNOWN_BASE..regs::HOLD_UNKNOWN_BASE + regs::HOLD_UNKNOWN_COUNT)
            .contains(&addr)
        {
            return Some(5500 + (addr - regs::HOLD_UNKNOWN_BASE) as u16);
        }
        // TCP COM 端口 (2243-2246)
        if (regs::HOLD_TCP_COM_BASE..regs::HOLD_TCP_COM_BASE + regs::HOLD_TCP_COM_COUNT)
            .contains(&addr)
        {
            let idx = (addr - regs::HOLD_TCP_COM_BASE) as usize;
            return Some(regs::TCP_PORTS_DEFAULT.get(idx).copied().unwrap_or(0));
        }
        None
    }

    pub fn write_reg(&mut self, addr: u16, value: u16) -> WriteResult {
        // SN
        if (regs::HOLD_SN_BASE..regs::HOLD_SN_BASE + regs::HOLD_SN_COUNT).contains(&addr) {
            let idx = (addr - regs::HOLD_SN_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.sn[idx] = hi;
            self.sn[idx + 1] = lo;
            return WriteResult::Ok;
        }
        // name
        if (regs::HOLD_PLACE_BASE..regs::HOLD_PLACE_BASE + regs::HOLD_PLACE_COUNT).contains(&addr) {
            let idx = (addr - regs::HOLD_PLACE_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.name[idx] = hi;
            self.name[idx + 1] = lo;
            return WriteResult::Ok;
        }
        match addr {
            regs::HOLD_HW_VER => {
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
        // BLE 名称 — 2字节/字
        if (regs::HOLD_BT_ADDR_BASE..regs::HOLD_BT_ADDR_BASE + 4).contains(&addr) {
            let idx = (addr - regs::HOLD_BT_ADDR_BASE) as usize * 2;
            let [hi, lo] = value.to_be_bytes();
            self.ble_name[idx] = hi;
            self.ble_name[idx + 1] = lo;
            return WriteResult::Ok;
        }
        if addr == regs::CFG_BLE_MESH_EN {
            self.ble_mesh_enable = value != 0;
            return WriteResult::Ok;
        }
        // RS485 — Word1=组合格式匹配参考固件
        for i in 0..5usize {
            let base = regs::HOLD_RS485_BASE + (i as u16) * regs::HOLD_RS485_STRIDE;
            if (base..base + 5).contains(&addr) {
                let off = (addr - base) as usize;
                let r = if i < self.rs485.len() {
                    &mut self.rs485[i]
                } else {
                    &mut self.rs485[0]
                };
                match off {
                    0 => {
                        let baud_idx = ((value >> 12) & 0xF) as u32;
                        let baud_table: [u32; 11] = [
                            1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800,
                            921600,
                        ];
                        r.baudrate = *baud_table.get(baud_idx as usize).unwrap_or(&9600);
                        r.parity = ((value >> 10) & 0x3) as u8;
                        r.stop_bits = (((value >> 9) & 0x1) + 1) as u8;
                        r.data_bits = if (value >> 8) & 0x1 != 0 { 7 } else { 8 };
                        r.mode = (value & 0xFF) as u8;
                    }
                    1 => r.slave_addr = value as u8,
                    2..=4 => {} // Retry/Timeout/Delay
                    _ => {}
                }
                return WriteResult::Ok;
            }
        }
        // 未知保留区 (2239-2242, 可写)
        if (regs::HOLD_UNKNOWN_BASE..regs::HOLD_UNKNOWN_BASE + regs::HOLD_UNKNOWN_COUNT)
            .contains(&addr)
        {
            return WriteResult::Ok;
        }
        // TCP COM 端口 (2243-2246, 可写)
        if (regs::HOLD_TCP_COM_BASE..regs::HOLD_TCP_COM_BASE + regs::HOLD_TCP_COM_COUNT)
            .contains(&addr)
        {
            return WriteResult::Ok;
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
        assert_eq!(cfg.dhcp, true);
        assert_eq!(cfg.ip, [192, 168, 1, 200]);
        assert_eq!(cfg.mac, [0x00, 0x08, 0xDC, 0x11, 0x22, 0x33]);
        assert_eq!(cfg.rs485.len(), 2);
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
        assert_eq!(parse_mac("AA:BB:CC:DD:EE:FF"), Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]));
        assert_eq!(parse_mac("00:08:DC:11:22:33"), Some([0x00, 0x08, 0xDC, 0x11, 0x22, 0x33]));
        assert_eq!(parse_mac("aa:bb:cc:dd:ee:ff"), Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
        assert_eq!(parse_mac("invalid"), None);
        assert_eq!(parse_mac("AA:BB:CC"), None);
    }

    #[test]
    fn test_ip_str_format() {
        let cfg = SystemConfig::defaults();
        let s = cfg.ip_str();
        assert!(s.contains("192.168.1.200"));
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
        assert_eq!(result, WriteResult::Ok);
        // cfg.dhcp is private; assert via behavior
        let result2 = cfg.write_reg(0xFF03, 0);
        assert_eq!(result2, WriteResult::Ok);
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
}
