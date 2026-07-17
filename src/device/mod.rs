//! 设备协议存储 + 系统配置
//!
//! 本模块管理两类 NVS 持久化数据:
//!
//! 1. **协议存储区** (`ProtoStore`)
//!    - 单段连续 1500 个 U16 (3000 字节)
//!    - 用户自定义协议, 外部解析器消费, 本系统不解析语义
//!    - 寄存器: 0x4000-0x45E1
//!
//! 2. **系统配置** (`SystemConfig`)
//!    - SN/MAC/IP/网关/RS485/BLE 等结构化字段
//!    - 寄存器: 0x0200-0x025F
//!    - 修改后写 CFG_APPLY=0xB5B5 触发持久化 + 应用
//!    - 写 CFG_RESET=0xD5D5 恢复默认
//!
//! 持久化策略:
//! - RAM 镜像常驻 SRAM
//! - NVS blob 持久化 (各自 namespace/key)
//! - 异步监听线程消费 commit/reload/apply 请求
//!
//! # 原子性保证 (双 blob A/B 轮换)
//!
//! 协议数据采用 A/B 双 blob 策略, 避免写入中途掉电导致数据损坏:
//! - 两个 blob key: `proto_a` / `proto_b`, 各含完整头部 (magic/version/length/crc) + 数据
//! - `proto_act` key 标记当前有效 blob (0=A, 1=B)
//! - 写入流程: 先写 inactive blob → 校验 CRC → 切换 active 标志
//! - 读取流程: 读 active blob → CRC 校验失败则回退到 inactive → 都失败用默认值
//! - 兼容旧格式: 若 `proto_act` 不存在但 `proto_data` 存在, 迁移到 A

pub mod system_config;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvsPartition, NvsDefault};

use crate::bus;
use crate::error::{AppError, AppResult};
use crate::health::{self, TaskHb};

pub use system_config::{SystemConfig, parse_ipv4, parse_mac};

/// device-store 监听任务心跳 (静态分配)
static WATCH_HB: TaskHb = TaskHb::new("device-store");

// ----------------------------------------------------------------------------
// 常量
// ----------------------------------------------------------------------------

/// NVS 命名空间 (≤ 15 字符)
const NVS_NAMESPACE: &str = "gateway";

// ---- 协议数据双 blob (A/B 轮换, 原子写入) ----
/// blob A key (含 magic+ver+len+crc+data, 共 3010 字节)
const NVS_KEY_DATA_A: &str = "proto_a";
/// blob B key
const NVS_KEY_DATA_B: &str = "proto_b";
/// 当前 active blob 标志 (0=A, 1=B)
const NVS_KEY_ACTIVE: &str = "proto_act";

// ---- 兼容旧格式 (单 blob, 仅用于迁移) ----
const NVS_KEY_DATA_LEGACY: &str = "proto_data";
const NVS_KEY_MAGIC_LEGACY: &str = "proto_magic";
const NVS_KEY_VERSION_LEGACY: &str = "proto_ver";
const NVS_KEY_LENGTH_LEGACY: &str = "proto_len";

/// 魔数标识, 用于校验 NVS 是否已初始化
pub const PROTO_MAGIC: u16 = 0x4757; // 'G'<<8 | 'W'

/// 复位计数 NVS key
const NVS_KEY_RESET_CNT: &str = "rst_cnt";
/// BLE Mesh net_idx NVS key (配网后由 BLE Mesh 回调写入)
const NVS_KEY_MESH_NET_IDX: &str = "mesh_nidx";
/// BLE Mesh app_idx NVS key (配网后由 BLE Mesh 回调写入)
const NVS_KEY_MESH_APP_IDX: &str = "mesh_aidx";

/// 协议数据字数 (U16)
const PROTO_WORDS: usize = 1500;
/// 协议数据字节数
const PROTO_DATA_BYTES: usize = PROTO_WORDS * 2; // 3000
/// blob 头部大小: magic(2) + version(2) + length(2) + crc(4) = 10 字节
const BLOB_HEADER_BYTES: usize = 10;
/// 完整 blob 大小: 头部 + 数据
const BLOB_TOTAL_BYTES: usize = BLOB_HEADER_BYTES + PROTO_DATA_BYTES; // 3010

// ----------------------------------------------------------------------------
// 全局状态
// ----------------------------------------------------------------------------

/// NVS 分区句柄 (单例, take 一次)
pub static NVS_PARTITION: Lazy<EspDefaultNvsPartition> = Lazy::new(|| {
    EspNvsPartition::<NvsDefault>::take().expect("nvs partition take failed")
});

/// NVS 句柄 (gateway namespace)
/// 注意 esp_idf_svc 0.50: EspDefaultNvs::new 第三参数 read_write: bool
pub static NVS: Lazy<Mutex<EspDefaultNvs>> = Lazy::new(|| {
    let nvs = EspDefaultNvs::new(NVS_PARTITION.clone(), NVS_NAMESPACE, true)
        .expect("nvs open failed");
    Mutex::new(nvs)
});

/// COMMIT 请求标志 (Modbus / AT 命令设置, 监听线程消费)
static COMMIT_REQUEST: AtomicBool = AtomicBool::new(false);
/// RELOAD 请求标志
static RELOAD_REQUEST: AtomicBool = AtomicBool::new(false);
/// APPLY_CONFIG 请求标志 (写 CFG_APPLY=0xB5B5 或 AT+CFGAPPLY 触发)
static APPLY_CONFIG_REQUEST: AtomicBool = AtomicBool::new(false);

// ----------------------------------------------------------------------------
// 初始化
// ----------------------------------------------------------------------------

/// 初始化设备模块:
/// 1. 强制初始化 NVS 全局
/// 2. 从 NVS 加载 ProtoStore + SystemConfig 到 bus
/// 3. 启动 commit/reload/apply 监听线程
pub fn init() -> AppResult<()> {
    // 1. 强制初始化 NVS
    Lazy::force(&NVS_PARTITION);
    Lazy::force(&NVS);

    // 2. 加载协议数据
    let nvs = NVS.lock();
    let (data, version, length) = load_proto_from_nvs(&nvs)?;

    // 3. 加载系统配置 + 从硬件填充 MAC + 同步 fw_version
    let mut cfg = SystemConfig::load_from_nvs(&nvs)?;
    cfg.fill_hw_macs();
    // fw_version 始终从 Cargo.toml 同步, 防止固件升级后版本号不更新
    cfg.fw_version = SystemConfig::fw_version_from_cargo();
    drop(nvs);

    {
        let mut bus = bus::lock_timeout()
            .ok_or_else(|| AppError::Config("bus lock timeout".into()))?;
        bus.proto.data.copy_from_slice(&data[..PROTO_WORDS]);
        bus.proto.version = version;
        bus.proto.length = length;
        bus.proto.dirty = false;
        bus.proto.status = 0;

        bus.cfg = cfg.clone();
    }

    log::info!(
        "[device] proto loaded: {} words, version={:#06x}",
        length, version
    );
    log::info!(
        "[device] cfg loaded: sn='{}' ip={} mac={} fw=0x{:04X} ble_mesh={}",
        cfg.sn_str(),
        cfg.ip_str(),
        cfg.mac_str(),
        cfg.fw_version,
        cfg.ble_mesh_enable
    );

    // 4. 启动监听线程
    health::register(&WATCH_HB);
    health::set_next_thread_core(health::CORE_NET);
    let result = std::thread::Builder::new()
        .name("device-store".into())
        .stack_size(16384)
        .spawn(watch_loop);
    health::reset_thread_core();
    result.map_err(|e| AppError::Config(format!("spawn device-store: {e}")))?;

    Ok(())
}

/// 借用 NVS 分区句柄 (供其它模块如 blemesh 复用, 避免重复 take)
pub fn nvs_partition() -> EspDefaultNvsPartition {
    NVS_PARTITION.clone()
}

/// 从 NVS 读取 blob 到 buffer, 返回实际读取的字节数
pub fn nvs_read_to_buf(key: &str, buf: &mut [u8]) -> Option<usize> {
    let nvs = NVS.lock();
    match nvs.get_blob(key, buf) {
        Ok(Some(data)) => Some(data.len()),
        _ => None,
    }
}

/// 写入 blob 到 NVS
pub fn nvs_write(key: &str, data: &[u8]) -> AppResult<()> {
    let nvs = NVS.lock();
    nvs.set_blob(key, data)
        .map_err(|e| AppError::Config(format!("nvs set_blob {key}: {e:?}")))
}

// ----------------------------------------------------------------------------
// 复位计数持久化 (工业可靠性: 记录复位历史)
// ----------------------------------------------------------------------------

/// 从 NVS 读取复位计数 (首次启动返回 0)
pub fn load_reset_count() -> u16 {
    // NVS 可能未初始化 (init 失败时), 此时返回 0
    if let Some(nvs) = NVS.try_lock() {
        nvs.get_u16(NVS_KEY_RESET_CNT)
            .ok()
            .flatten()
            .unwrap_or(0)
    } else {
        0
    }
}

/// 保存复位计数到 NVS
pub fn save_reset_count(cnt: u16) -> AppResult<()> {
    let mut nvs = NVS.lock();
    nvs.set_u16(NVS_KEY_RESET_CNT, cnt)
        .map_err(|e| AppError::Config(format!("nvs set rst_cnt: {e:?}")))?;
    Ok(())
}

// ----------------------------------------------------------------------------
// BLE Mesh 密钥索引持久化 (配网完成后由 mesh 回调写入, 启动时读取)
// ----------------------------------------------------------------------------

/// 从 NVS 读取 BLE Mesh net_idx / app_idx (未配网时返回 (0, 0))
pub fn load_mesh_keys() -> (u16, u16) {
    if let Some(nvs) = NVS.try_lock() {
        let net_idx = nvs.get_u16(NVS_KEY_MESH_NET_IDX).ok().flatten().unwrap_or(0);
        let app_idx = nvs.get_u16(NVS_KEY_MESH_APP_IDX).ok().flatten().unwrap_or(0);
        (net_idx, app_idx)
    } else {
        (0, 0)
    }
}

/// 保存 BLE Mesh net_idx / app_idx 到 NVS (配网完成时调用)
pub fn save_mesh_keys(net_idx: u16, app_idx: u16) -> AppResult<()> {
    let mut nvs = NVS.lock();
    nvs.set_u16(NVS_KEY_MESH_NET_IDX, net_idx)
        .map_err(|e| AppError::Config(format!("nvs set mesh_nidx: {e:?}")))?;
    nvs.set_u16(NVS_KEY_MESH_APP_IDX, app_idx)
        .map_err(|e| AppError::Config(format!("nvs set mesh_aidx: {e:?}")))?;
    Ok(())
}

// ----------------------------------------------------------------------------
// 监听线程
// ----------------------------------------------------------------------------

fn watch_loop() {
    loop {
        // 心跳: 每次 50ms 循环
        WATCH_HB.tick();
        if COMMIT_REQUEST.swap(false, Ordering::SeqCst) {
            // 去抖: 若已在写入中 (status=1), 跳过本次请求, 避免重复排队
            let already_committing = bus::lock_timeout()
                .map(|b| b.proto.status == 1)
                .unwrap_or(false);
            if already_committing {
                log::debug!("[device] commit skipped (status=1, already committing)");
            } else if let Err(e) = commit() {
                log::error!("[device] commit failed: {}", e);
                if let Some(mut bus) = bus::lock_timeout() {
                    bus.proto.status = 3; // 校验失败
                }
            }
        }
        if RELOAD_REQUEST.swap(false, Ordering::SeqCst) {
            // 去抖: 若已在加载中 (status=2), 跳过
            let already_loading = bus::lock_timeout()
                .map(|b| b.proto.status == 2)
                .unwrap_or(false);
            if already_loading {
                log::debug!("[device] reload skipped (status=2, already loading)");
            } else if let Err(e) = reload() {
                log::error!("[device] reload failed: {}", e);
                if let Some(mut bus) = bus::lock_timeout() {
                    bus.proto.status = 3;
                }
            }
        }
        if APPLY_CONFIG_REQUEST.swap(false, Ordering::SeqCst) {
            if let Err(e) = apply_config() {
                log::error!("[device] apply_config failed: {}", e);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ----------------------------------------------------------------------------
// 公开 API
// ----------------------------------------------------------------------------

/// 请求持久化 (由 Modbus 写 COMMIT 寄存器或 AT+COMMIT 调用)
/// 实际写入由监听线程异步执行, 避免阻塞 Modbus/AT 主线程
pub fn request_commit() {
    COMMIT_REQUEST.store(true, Ordering::SeqCst);
}

/// 请求重载 (由 Modbus 写 RELOAD 寄存器或 AT+RELOAD 调用)
pub fn request_reload() {
    RELOAD_REQUEST.store(true, Ordering::SeqCst);
}

/// 请求应用配置 (由 Modbus 写 CFG_APPLY=0xB5B5 或 AT+CFGAPPLY 调用)
pub fn request_apply_config() {
    APPLY_CONFIG_REQUEST.store(true, Ordering::SeqCst);
}

/// 同步提交 (供 AT 命令直接调用, 阻塞至完成)
pub fn commit_sync() -> AppResult<()> {
    commit()
}

/// 同步重载 (供 AT 命令直接调用)
pub fn reload_sync() -> AppResult<()> {
    reload()
}

/// 同步应用配置 (供 AT 命令直接调用)
pub fn apply_config_sync() -> AppResult<()> {
    apply_config()
}

// ----------------------------------------------------------------------------
// 内部实现
// ----------------------------------------------------------------------------

fn commit() -> AppResult<()> {
    // 1. 从 bus 拷贝数据 (持锁时间短)
    let (data, version, length) = {
        let mut bus = bus::lock_timeout()
            .ok_or_else(|| AppError::Config("bus lock timeout".into()))?;
        bus.proto.status = 1; // 写入中
        (bus.proto.data, bus.proto.version, bus.proto.length)
    };

    // 2. 序列化为 blob: [magic:2][version:2][length:2][crc:4][data:3000] = 3010 字节
    let mut blob = [0u8; BLOB_TOTAL_BYTES];
    blob[0..2].copy_from_slice(&PROTO_MAGIC.to_le_bytes());
    blob[2..4].copy_from_slice(&version.to_le_bytes());
    blob[4..6].copy_from_slice(&length.to_le_bytes());
    // blob[6..10] = crc, 稍后填入
    for (i, &v) in data.iter().enumerate() {
        blob[BLOB_HEADER_BYTES + 2 * i..BLOB_HEADER_BYTES + 2 * i + 2]
            .copy_from_slice(&v.to_le_bytes());
    }
    // 计算 CRC32 (覆盖 header[0..6] + data, 不含 crc 字段本身)
    let crc = crc32(&blob[0..6]);
    let crc_data = crc32(&blob[BLOB_HEADER_BYTES..]);
    let combined_crc = crc.wrapping_add(crc_data);
    blob[6..10].copy_from_slice(&combined_crc.to_le_bytes());

    // 3. 读当前 active 标志, 决定写入哪个 blob
    let mut nvs = NVS.lock();
    let active = nvs
        .get_u8(NVS_KEY_ACTIVE)
        .ok()
        .flatten()
        .unwrap_or(0); // 默认 A (首次启动)
    let write_key = if active == 0 {
        NVS_KEY_DATA_B
    } else {
        NVS_KEY_DATA_A
    };
    let new_active = if active == 0 { 1u8 } else { 0u8 };

    // 4. 写入 inactive blob (若此时掉电, active 仍指向旧 blob, 数据不丢)
    nvs.set_blob(write_key, &blob)
        .map_err(|e| AppError::Config(format!("nvs set_blob {write_key}: {e:?}")))?;

    // 5. 切换 active 标志 (原子操作: set_u8 是单 page 写入, ESP-IDF NVS 保证页级原子性)
    nvs.set_u8(NVS_KEY_ACTIVE, new_active)
        .map_err(|e| AppError::Config(format!("nvs set active: {e:?}")))?;

    // 6. 清理 legacy key (可选, 首次迁移后删除以释放空间)
    // 注: 删除失败不影响功能, 仅浪费少量 NVS 空间
    let _ = nvs.remove(NVS_KEY_DATA_LEGACY);
    let _ = nvs.remove(NVS_KEY_MAGIC_LEGACY);
    let _ = nvs.remove(NVS_KEY_VERSION_LEGACY);
    let _ = nvs.remove(NVS_KEY_LENGTH_LEGACY);

    // 7. 更新 bus 状态
    {
        let mut bus = bus::lock_timeout()
            .ok_or_else(|| AppError::Config("bus lock timeout".into()))?;
        bus.proto.dirty = false;
        bus.proto.status = 0;
    }

    log::info!(
        "[device] proto committed: {} words, ver={:#06x}, blob={}",
        length, version, if new_active == 0 { 'A' } else { 'B' }
    );
    Ok(())
}

fn reload() -> AppResult<()> {
    // 1. 标记状态
    {
        let mut bus = bus::lock_timeout()
            .ok_or_else(|| AppError::Config("bus lock timeout".into()))?;
        bus.proto.status = 2; // 加载中
    }

    // 2. 从 NVS 读取
    let nvs = NVS.lock();
    let (data, version, length) = load_proto_from_nvs(&nvs)?;
    drop(nvs);

    // 3. 写回 bus
    {
        let mut bus = bus::lock_timeout()
            .ok_or_else(|| AppError::Config("bus lock timeout".into()))?;
        bus.proto.data.copy_from_slice(&data[..PROTO_WORDS]);
        bus.proto.version = version;
        bus.proto.length = length;
        bus.proto.dirty = false;
        bus.proto.status = 0;
    }

    log::info!("[device] proto reloaded: {} words, ver={:#06x}", length, version);
    Ok(())
}

/// 应用系统配置 (不重启):
/// 1. 持久化当前 SystemConfig 到 NVS
/// 2. **不重启** - 避免每次 Modbus 写入配置都断开 BLE 连接
///
/// 设计变更: 之前的实现每次 Apply 都触发 esp_restart(), 导致 Android 端
/// "参数导入" 批量写入后立即断开 BLE, 用户体验极差。
/// 现在的策略: 仅写 NVS, 不重启, 让 BLE 和其他外设保持运行。
///
/// 注: 网络/RS485/BLE 配置的运行时切换不在此函数中处理, 由各模块自行监听 cfg 变化。
/// 如果用户希望完全重新初始化外设, 可以显式调用 esp_restart()。
fn apply_config() -> AppResult<()> {
    log::info!("[device] applying system config (no restart)...");

    // 1. 从 bus 拷贝当前 cfg (持锁时间短)
    let cfg = {
        let bus = bus::lock_timeout()
            .ok_or_else(|| AppError::Config("bus lock timeout".into()))?;
        bus.cfg.clone()
    };

    // 2. 持久化到 NVS (同步, 因为 NVS 写入很快, 通常 < 50ms)
    let mut nvs = NVS.lock();
    cfg.save_to_nvs(&mut nvs)?;
    drop(nvs);

    log::info!(
        "[device] cfg persisted: sn='{}' ip={} ver={} eth_mac={}",
        cfg.sn_str(),
        cfg.ip_str(),
        cfg.cfg_version,
        cfg.mac_str()
    );
    log::info!("[device] BLE and other peripherals continue running (no restart)");
    Ok(())
}

/// 从 NVS 加载协议数据 (双 blob A/B + legacy 兼容)
///
/// 读取顺序:
/// 1. 读 active 标志 → 读对应 blob → 校验 magic + CRC
/// 2. 失败则读另一个 blob → 校验
/// 3. 都失败则尝试 legacy 单 blob 格式 (兼容旧固件)
/// 4. 都失败则返回空默认值
fn load_proto_from_nvs(nvs: &EspDefaultNvs) -> AppResult<([u16; PROTO_WORDS], u16, u16)> {
    let mut data = [0u16; PROTO_WORDS];

    // 尝试读取双 blob
    let active = nvs
        .get_u8(NVS_KEY_ACTIVE)
        .ok()
        .flatten();

    if let Some(act) = active {
        // 双 blob 模式: 先读 active, 失败读 inactive
        let first_key = if act == 0 { NVS_KEY_DATA_A } else { NVS_KEY_DATA_B };
        let second_key = if act == 0 { NVS_KEY_DATA_B } else { NVS_KEY_DATA_A };

        if let Some((d, v, l)) = load_single_blob(nvs, first_key)? {
            log::info!("[device] loaded from active blob ({})", first_key);
            return Ok((d, v, l));
        }
        log::warn!("[device] active blob {} corrupted, trying inactive", first_key);
        if let Some((d, v, l)) = load_single_blob(nvs, second_key)? {
            log::warn!("[device] recovered from inactive blob {}", second_key);
            return Ok((d, v, l));
        }
        log::warn!("[device] both blobs corrupted, trying legacy");
    } else {
        log::info!("[device] no active flag, trying legacy single blob");
    }

    // 兼容 legacy 单 blob 格式
    if let Some((d, v, l)) = load_legacy_blob(nvs)? {
        log::info!("[device] loaded from legacy blob, will migrate to A/B on next commit");
        return Ok((d, v, l));
    }

    // 全部失败, 返回默认空数据
    log::info!("[device] nvs empty or all blobs corrupted, using defaults");
    Ok((data, 0, 0))
}

/// 读取单个 A/B blob 并校验 magic + CRC
///
/// blob 布局: [magic:2][version:2][length:2][crc:4][data:3000] = 3010 字节
/// CRC 覆盖 header[0..6] + data, 用 wrapping_add 合并两部分 CRC
fn load_single_blob(nvs: &EspDefaultNvs, key: &str) -> AppResult<Option<([u16; PROTO_WORDS], u16, u16)>> {
    let mut buf = [0u8; BLOB_TOTAL_BYTES];
    let blob = nvs
        .get_blob(key, &mut buf)
        .map_err(|e| AppError::Config(format!("nvs get_blob {key}: {e:?}")))?;

    let bytes = match blob {
        Some(b) if b.len() == BLOB_TOTAL_BYTES => b,
        _ => return Ok(None), // 不存在或长度不符
    };

    // 校验 magic
    let magic = u16::from_le_bytes([bytes[0], bytes[1]]);
    if magic != PROTO_MAGIC {
        log::warn!("[device] blob {key} magic mismatch: {magic:#06x}", );
        return Ok(None);
    }

    let version = u16::from_le_bytes([bytes[2], bytes[3]]);
    let length = u16::from_le_bytes([bytes[4], bytes[5]]);
    let stored_crc = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);

    // 校验 CRC: header[0..6] + data, wrapping_add 合并
    let crc_header = crc32(&bytes[0..6]);
    let crc_data = crc32(&bytes[BLOB_HEADER_BYTES..]);
    let calc_crc = crc_header.wrapping_add(crc_data);

    if stored_crc != calc_crc {
        log::warn!("[device] blob {key} CRC mismatch: stored={:#010x} calc={:#010x}", stored_crc, calc_crc);
        return Ok(None);
    }

    // CRC 校验通过, 解析数据
    let mut data = [0u16; PROTO_WORDS];
    let valid_words = (length as usize).min(PROTO_WORDS);
    for i in 0..valid_words {
        let off = BLOB_HEADER_BYTES + 2 * i;
        data[i] = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
    }

    Ok(Some((data, version, length)))
}

/// 读取 legacy 单 blob 格式 (兼容旧固件, 仅用于迁移)
fn load_legacy_blob(nvs: &EspDefaultNvs) -> AppResult<Option<([u16; PROTO_WORDS], u16, u16)>> {
    // 1. 检查 legacy magic
    let magic = nvs
        .get_u16(NVS_KEY_MAGIC_LEGACY)
        .map_err(|e| AppError::Config(format!("nvs get legacy magic: {e:?}")))?
        .unwrap_or(0);

    if magic != PROTO_MAGIC {
        return Ok(None);
    }

    // 2. 读 legacy blob
    let mut buf = [0u8; PROTO_DATA_BYTES];
    let blob = nvs
        .get_blob(NVS_KEY_DATA_LEGACY, &mut buf)
        .map_err(|e| AppError::Config(format!("nvs get legacy blob: {e:?}")))?;

    let bytes = match blob {
        Some(b) => b,
        None => return Ok(None),
    };

    let mut data = [0u16; PROTO_WORDS];
    let words = (bytes.len() / 2).min(PROTO_WORDS);
    for i in 0..words {
        data[i] = u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
    }

    let version = nvs
        .get_u16(NVS_KEY_VERSION_LEGACY)
        .map_err(|e| AppError::Config(format!("nvs get legacy ver: {e:?}")))?
        .unwrap_or(0);
    let length = nvs
        .get_u16(NVS_KEY_LENGTH_LEGACY)
        .map_err(|e| AppError::Config(format!("nvs get legacy len: {e:?}")))?
        .unwrap_or(0);

    log::info!("[device] legacy blob loaded: {} words, ver={:#06x}", length, version);
    Ok(Some((data, version, length)))
}

/// CRC32 (IEEE 802.3, 多项式 0xEDB88320)
///
/// 用于 blob 完整性校验。标准实现, 无外部依赖。
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

// ----------------------------------------------------------------------------
// AT 命令直接访问 API (供 ble_at 模块使用)
// ----------------------------------------------------------------------------

/// 读取协议区指定地址 (0..1500), 返回 None 表示越界
pub fn proto_read(addr: u16) -> Option<u16> {
    let bus = bus::lock_timeout()?;
    if (addr as usize) < 1500 {
        Some(bus.proto.data[addr as usize])
    } else {
        None
    }
}

/// 写入协议区指定地址 (0..1500)
pub fn proto_write(addr: u16, value: u16) -> bool {
    if let Some(mut bus) = bus::lock_timeout() {
        if (addr as usize) < 1500 {
            bus.proto.data[addr as usize] = value;
            bus.proto.dirty = true;
            return true;
        }
    }
    false
}

/// 批量读 (start..start+len, 越界自动截断)
pub fn proto_read_bulk(start: u16, len: u16) -> Vec<u16> {
    let mut out = Vec::with_capacity(len as usize);
    if let Some(bus) = bus::lock_timeout() {
        for i in 0..len {
            let a = start.saturating_add(i);
            if (a as usize) < 1500 {
                out.push(bus.proto.data[a as usize]);
            } else {
                break;
            }
        }
    }
    out
}

/// 批量写
pub fn proto_write_bulk(start: u16, values: &[u16]) -> bool {
    if let Some(mut bus) = bus::lock_timeout() {
        for (i, &v) in values.iter().enumerate() {
            let a = start.saturating_add(i as u16);
            if (a as usize) < 1500 {
                bus.proto.data[a as usize] = v;
            } else {
                break;
            }
        }
        bus.proto.dirty = true;
        return true;
    }
    false
}

/// 协议信息 (供 AT+INFO 调用)
pub struct ProtoInfo {
    pub capacity: u16,   // 1500
    pub version: u16,
    pub length: u16,
    pub dirty: bool,
    pub status: u8,
    pub magic: u16,
}

pub fn proto_info() -> ProtoInfo {
    if let Some(bus) = bus::lock_timeout() {
        ProtoInfo {
            capacity: 1500,
            version: bus.proto.version,
            length: bus.proto.length,
            dirty: bus.proto.dirty,
            status: bus.proto.status,
            magic: PROTO_MAGIC,
        }
    } else {
        ProtoInfo {
            capacity: 1500,
            version: 0,
            length: 0,
            dirty: false,
            status: 3,
            magic: PROTO_MAGIC,
        }
    }
}
