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

use std::time::Duration;

use std::sync::LazyLock;
use std::sync::Arc;

use crate::actor::{spawn, Actor, ActorRef};
use crate::sync::Spin;

// ----------------------------------------------------------------------------
// DeviceActor: 异步请求队列 (取代 AtomicBool 标志位 + watch_loop 轮询)
// ----------------------------------------------------------------------------
//
// 原方案: Modbus/AT 进程通过 AtomicBool::store(true) 设置请求位, watch_loop 每 50ms
// 轮询并 swap(false). 高并发 commit 时存在去抖丢失风险且无消息类型化.
//
// 新方案: 请求封装为 `DeviceCmd` 消息, 通过 Actor mailbox 传递. Actor 线程独占
// 调用 `handle(msg)`, 内部 NVS/commit/reload 状态无需锁. `idle()` 钩子负责 50ms
// 心跳喂狗, 替代原 watch_loop 的 `WATCH_HB.tick()` + `sleep(50ms)`.

/// 设备存储命令 (Actor 消息)
#[derive(Clone, Copy, Debug)]
pub enum DeviceCmd {
    /// 异步提交协议数据到 NVS
    Commit,
    /// 异步从 NVS 重新加载协议数据
    Reload,
    /// 异步应用配置 (CFG_APPLY=0xB5B5)
    ApplyConfig,
    /// 异步持久化复位计数到 NVS
    PersistResetCount(u16),
    /// 异步持久化 BLE Mesh net_idx / app_idx 到 NVS
    PersistMeshKeys { net_idx: u16, app_idx: u16 },
}

/// DeviceActor: 单线程消费 DeviceCmd, 独占 NVS/commit/reload 状态.
pub struct DeviceActor;

impl Actor for DeviceActor {
    type Msg = DeviceCmd;

    fn handle(&mut self, msg: DeviceCmd) {
        match msg {
            DeviceCmd::Commit => {
                // 始终执行 commit. proto.status 的 "已开始" 信号由 backends::write_hold_reg
                // (PROTO_COMMIT 分支) 在排队前 `proto_status_set(1)` 提供, 这里再 set 也是
                // 幂等; commit() 内部 NVS 完成后会复位为 0 (或失败置 3).
                //
                // 旧 handle 用 "proto.status == 1 视为 in-flight 并跳过" 去抖,
                // 但 backends 已把 atomic 置 1 才入队 → 此处必然命中 → 永远 skip → 死锁.
                // Actor 单线程消费 mailbox, 真实并发不存在, 故去抖本身无意义, 直接执行.
                if let Err(e) = commit() {
                    log::error!("[device] commit failed: {}", e);
                    bus::storage_state::proto_status_set(3);
                }
            }
            DeviceCmd::Reload => {
                if let Err(e) = reload() {
                    log::error!("[device] reload failed: {}", e);
                    bus::storage_state::proto_status_set(3);
                }
            }
            DeviceCmd::ApplyConfig => {
                if let Err(e) = apply_config() {
                    log::error!("[device] apply_config failed: {}", e);
                }
            }
            DeviceCmd::PersistResetCount(cnt) => {
                match try_with_nvs_mut(|nvs| {
                    nvs.set_u16(NVS_KEY_RESET_CNT, cnt)
                        .map_err(|e| AppError::Config(format!("nvs set rst_cnt: {e:?}")))
                }) {
                    Some(Ok(())) => log::debug!("[device] reset_count={cnt} persisted"),
                    Some(Err(e)) => log::error!("[device] reset_count persist failed: {e}"),
                    None => log::warn!("[device] NVS unavailable, reset_count not persisted"),
                }
            }
            DeviceCmd::PersistMeshKeys { net_idx, app_idx } => {
                match try_with_nvs_mut(|nvs| -> AppResult<()> {
                    nvs.set_u16(NVS_KEY_MESH_NET_IDX, net_idx)
                        .map_err(|e| AppError::Config(format!("nvs set mesh_nidx: {e:?}")))?;
                    nvs.set_u16(NVS_KEY_MESH_APP_IDX, app_idx)
                        .map_err(|e| AppError::Config(format!("nvs set mesh_aidx: {e:?}")))?;
                    Ok(())
                }) {
                    Some(Ok(())) => log::debug!("[device] mesh_keys=({net_idx},{app_idx}) persisted"),
                    Some(Err(e)) => log::error!("[device] mesh_keys persist failed: {e}"),
                    None => log::warn!("[device] NVS unavailable, mesh_keys not persisted"),
                }
            }
        }
    }

    fn idle(&mut self) -> Duration {
        // 心跳喂狗 (Actor 线程独占调用, 等同原 watch_loop 每 50ms 一次)
        WATCH_HB.tick();
        Duration::from_millis(50)
    }
}

/// 全局 DeviceActor 引用 (Lazy 初始化, 由 `init()` 唤起)
static DEVICE_ACTOR: LazyLock<ActorRef<DeviceActor>> = LazyLock::new(|| spawn(DeviceActor).0);


use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvsPartition, NvsDefault};

use crate::bus;
use crate::error::{AppError, AppResult};
use crate::health::{self, TaskHb};
use crate::config::regs;

pub use system_config::{SystemConfig, parse_ipv4};

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

/// 设备文本区 NVS key (Android 1.0.78 0x1388-0x1B77, 4000 字节 UTF-16LE)
const NVS_KEY_DEV_TEXT: &str = "dev_text";
/// 设备文本区 NVS magic (用于校验 blob 完整性)
const NVS_KEY_DEV_TEXT_MAGIC: &str = "dev_text_mag";
/// 设备文本区 magic 字 (0xDEAD = "DT")
const DEV_TEXT_MAGIC: u16 = 0xDE54;

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
/// 取失败时使用 PLACEHOLDER (后续所有 NVS 操作会返回错误, 但不 panic)
pub static NVS_PARTITION: LazyLock<EspDefaultNvsPartition> = LazyLock::new(|| {
    EspNvsPartition::<NvsDefault>::take().unwrap_or_else(|e| {
        log::error!("[device] NVS partition take failed: {e:?}");
        log::error!("[device] system will run in degraded mode (no persistence)");
        // 构造一个 dummy 分区 - 实际 NVS 操作都会失败但不会 panic
        // (这里用 expect() 是因为分区是必需的, 失败后只能 reset)
        panic!("NVS partition unavailable: {e:?}");
    })
});

/// NVS 句柄 (gateway namespace)
/// 注意 esp_idf_svc 0.50: EspDefaultNvs::new 第三参数 read_write: bool
/// 使用 Option 内部存储, 初始化失败时设为 None, 后续操作 graceful fail
pub static NVS: LazyLock<Spin<Option<EspDefaultNvs>>> = LazyLock::new(|| {
    let inner = match EspDefaultNvs::new(NVS_PARTITION.clone(), NVS_NAMESPACE, true) {
        Ok(nvs) => {
            log::info!("[device] NVS namespace '{NVS_NAMESPACE}' opened");
            Some(nvs)
        }
        Err(e) => {
            log::error!("[device] NVS open failed: {e:?}");
            log::error!("[device] persistence disabled, system continues without storage");
            None
        }
    };
    Spin::new(inner)
});


// ----------------------------------------------------------------------------
// 初始化
// ----------------------------------------------------------------------------

/// 初始化设备模块:
/// 1. 强制初始化 NVS 全局
/// 2. 从 NVS 加载 ProtoStore + SystemConfig 到 bus
/// 3. 启动 commit/reload/apply 监听线程
pub fn init() -> AppResult<()> {
    // 1. 强制初始化 NVS
    LazyLock::force(&NVS_PARTITION);
    LazyLock::force(&NVS);

    // 2. 加载协议数据 (NVS 不可用时使用空数据)
    let (data, version, length) = if let Some(nvs) = LazyLock::force(&NVS).lock().as_ref() {
        load_proto_from_nvs(nvs)?
    } else {
        log::warn!("[device] NVS unavailable, using empty protocol data");
        ([0u16; PROTO_WORDS], 0, 0)
    };

    // 3. 加载设备文本区 (Android 1.0.78 0x1388-0x1B77)
    let device_text = if let Some(nvs) = LazyLock::force(&NVS).lock().as_ref() {
        load_device_text_from_nvs(nvs)?
    } else {
        log::warn!("[device] NVS unavailable, using empty device_text");
        vec![0u16; regs::DEVICE_TEXT_COUNT as usize]
    };

    // 4. 加载系统配置 (NVS 不可用时使用默认值)
    let mut cfg = if let Some(nvs) = LazyLock::force(&NVS).lock().as_ref() {
        SystemConfig::load_from_nvs(nvs)?
    } else {
        log::warn!("[device] NVS unavailable, using default SystemConfig");
        SystemConfig::defaults()
    };
    cfg.fill_hw_macs();
    // fw_version 始终从 Cargo.toml 同步, 防止固件升级后版本号不更新
    cfg.fw_version = SystemConfig::fw_version_from_cargo();

    {
        // 直接写入 RCU 快照: STORAGE (proto / device_text / holding_buf) + CONFIG (cfg / device_config).
        // proto.status 是常量开销的 atomic; 写前先 publish 0, 快照内也镜像为 0.
        use crate::bus::storage_state::{ProtoStore, StorageSnapshot, storage_write, proto_status_set};
        use crate::bus::config_state::{ConfigSnapshot, config_write};
        use crate::bus::io_global::IO;

        proto_status_set(0);
        let snap = StorageSnapshot {
            proto: ProtoStore {
                data: data.to_vec().into_boxed_slice(),
                version,
                length,
                dirty: false,
                status: 0,
            },
            device_text: device_text.into_boxed_slice(),
            holding_buf: vec![0u16; 2048].into_boxed_slice(),
        };
        storage_write(snap);

        let cs = ConfigSnapshot {
            cfg: cfg.clone(),
            device_config: Arc::new(crate::device_config::DeviceConfigTable::default()),
        };
        config_write(cs);

        // Modbus 读 INREG_FW_VER 走 IO.sys 原子; legacy Bus 既有同步点已退役,
        // 显式把 fw_version 镜像到 IO.sys 确保 RCU 读者与 Modbus 读端一致.
        IO.sys.set_fw_version(cfg.fw_version);
    }

    log::info!(
        "[device] proto loaded: {} words, version={:#06x}",
        length, version
    );
    log::info!(
        "[device] cfg loaded: sn='{}' ip={} mac={} fw=0x{:04X}",
        cfg.sn_str(),
        cfg.ip_str(),
        cfg.mac_str(),
        cfg.fw_version,
    );

    // 4. 启动 DeviceActor (替代 watch_loop)
    // 关键: 必须在此处 force(), 否则后续 DEVICE_ACTOR.send() 会触发 lazy init
    // 在其他线程/上下文访问 spinlock 时产生重入, 触发 FreeRTOS assert
    health::register(&WATCH_HB);
    health::set_next_thread_core(health::CORE_NET);
    LazyLock::force(&DEVICE_ACTOR);
    log::info!("[device] DeviceActor started (mailbox consumer thread)");
    health::reset_thread_core();

    Ok(())
}

/// 借用 NVS 分区句柄 (供其它模块如 blemesh 复用, 避免重复 take)
pub fn nvs_partition() -> EspDefaultNvsPartition {
    NVS_PARTITION.clone()
}

/// 安全获取 NVS 句柄 (NVS 可能为 None 因为初始化失败)
/// 返回的 Guard 内部是 Option<EspDefaultNvs>, 调用方需 .as_ref() 检查
pub fn nvs_lock() -> crate::sync::SpinGuard<'static, Option<EspDefaultNvs>> {
    LazyLock::force(&NVS).lock()
}

/// NVS 是否可用 (用于快速检查, 避免不必要的锁)
pub fn nvs_available() -> bool {
    LazyLock::force(&NVS).lock().is_some()
}

/// 在 NVS 可用时调用闭包 (传入 &EspDefaultNvs), 否则返回 None
pub fn try_with_nvs<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&EspDefaultNvs) -> R,
{
    let guard = nvs_lock();
    guard.as_ref().map(f)
}

/// 在 NVS 可用时调用闭包 (传入 &mut EspDefaultNvs), 否则返回 None
pub fn try_with_nvs_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut EspDefaultNvs) -> R,
{
    let mut guard = nvs_lock();
    guard.as_mut().map(f)
}

/// 从 NVS 读取 blob 到 buffer, 返回实际读取的字节数
pub fn nvs_read_to_buf(key: &str, buf: &mut [u8]) -> Option<usize> {
    let guard = nvs_lock();
    let nvs = guard.as_ref()?;
    match nvs.get_blob(key, buf) {
        Ok(Some(data)) => Some(data.len()),
        _ => None,
    }
}

/// 写入 blob 到 NVS
pub fn nvs_write(key: &str, data: &[u8]) -> AppResult<()> {
    let guard = nvs_lock();
    let nvs = guard.as_ref().ok_or_else(|| AppError::Config("NVS not available".into()))?;
    nvs.set_blob(key, data)
        .map_err(|e| AppError::Config(format!("nvs set_blob {key}: {e:?}")))
}

// ----------------------------------------------------------------------------
// 复位计数持久化 (工业可靠性: 记录复位历史)
// ----------------------------------------------------------------------------

/// 从 NVS 读取复位计数 (首次启动返回 0)
pub fn load_reset_count() -> u16 {
    // NVS 可能未初始化 (init 失败时), 此时返回 0
    try_with_nvs(|nvs| {
        nvs.get_u16(NVS_KEY_RESET_CNT)
            .ok()
            .flatten()
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// 保存复位计数到 NVS (异步, 不阻塞调用方)
///
/// 实际 NVS 写入由 device-store 监听线程在后台完成, 主线程立即返回 Ok(())。
/// 失败时通过 log 记录, 不影响业务逻辑。
pub fn save_reset_count(cnt: u16) -> AppResult<()> {
    DEVICE_ACTOR.send(DeviceCmd::PersistResetCount(cnt));
    Ok(())
}

// ----------------------------------------------------------------------------
// BLE Mesh 密钥索引持久化 (配网完成后由 mesh 回调写入, 启动时读取)
// ----------------------------------------------------------------------------

/// 从 NVS 读取 BLE Mesh net_idx / app_idx (未配网时返回 (0, 0))
pub fn load_mesh_keys() -> (u16, u16) {
    try_with_nvs(|nvs| {
        let net_idx = nvs.get_u16(NVS_KEY_MESH_NET_IDX).ok().flatten().unwrap_or(0);
        let app_idx = nvs.get_u16(NVS_KEY_MESH_APP_IDX).ok().flatten().unwrap_or(0);
        (net_idx, app_idx)
    })
    .unwrap_or((0, 0))
}

/// 保存 BLE Mesh net_idx / app_idx 到 NVS (异步: 递交 DeviceActor, 不阻塞调用方)
pub fn save_mesh_keys(net_idx: u16, app_idx: u16) -> AppResult<()> {
    DEVICE_ACTOR.send(DeviceCmd::PersistMeshKeys { net_idx, app_idx });
    Ok(())
}

// ----------------------------------------------------------------------------
// 公开 API
// ----------------------------------------------------------------------------

/// 请求持久化 (由 Modbus 写 COMMIT 寄存器或 AT+COMMIT 调用)
/// 实际写入由监听线程异步执行, 避免阻塞 Modbus/AT 主线程
pub fn request_commit() {
    DEVICE_ACTOR.send(DeviceCmd::Commit);
}

/// 请求重载 (由 Modbus 写 RELOAD 寄存器或 AT+RELOAD 调用)
pub fn request_reload() {
    DEVICE_ACTOR.send(DeviceCmd::Reload);
}

/// 请求应用配置 (由 Modbus 写 CFG_APPLY=0xB5B5 或 AT+CFGAPPLY 调用)
pub fn request_apply_config() {
    DEVICE_ACTOR.send(DeviceCmd::ApplyConfig);
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
    // 1. 标记写入中 (atomic) 并从 RCU 快照拷贝 proto (无锁, 不再抢 Spin)
    bus::storage_state::proto_status_set(1);
    let (data, version, length) = bus::storage_state::storage_read_with(|s| {
        (s.proto.data.clone(), s.proto.version, s.proto.length)
    })
    .unwrap_or((vec![0u16; PROTO_WORDS].into_boxed_slice(), 0, 0));

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

    // 3. 读当前 active 标志, 决定写入哪个 blob (若 NVS 不可用则假设首次启动)
    let active = try_with_nvs(|nvs| {
        nvs.get_u8(NVS_KEY_ACTIVE)
            .ok()
            .flatten()
            .unwrap_or(0) // 默认 A (首次启动)
    })
    .unwrap_or(0);
    let write_key = if active == 0 {
        NVS_KEY_DATA_B
    } else {
        NVS_KEY_DATA_A
    };
    let new_active = if active == 0 { 1u8 } else { 0u8 };

    // 4-6. 写入 NVS (若 NVS 不可用, 跳过持久化但不报错)
    let write_result = try_with_nvs_mut(|nvs| -> AppResult<()> {
        nvs.set_blob(write_key, &blob)
            .map_err(|e| AppError::Config(format!("nvs set_blob {write_key}: {e:?}")))?;
        nvs.set_u8(NVS_KEY_ACTIVE, new_active)
            .map_err(|e| AppError::Config(format!("nvs set active: {e:?}")))?;
        // 清理 legacy key
        let _ = nvs.remove(NVS_KEY_DATA_LEGACY);
        let _ = nvs.remove(NVS_KEY_MAGIC_LEGACY);
        let _ = nvs.remove(NVS_KEY_VERSION_LEGACY);
        let _ = nvs.remove(NVS_KEY_LENGTH_LEGACY);
        Ok(())
    });

    if let Some(result) = write_result {
        if let Err(e) = result {
            return Err(e);
        }
    } else {
        log::warn!("[device] NVS unavailable, proto commit skipped");
    }

    // 7. 更新状态: atomic 复位 + RCU RMW 把 dirty 清零 (快照镜像也会被同步)
    bus::storage_state::proto_status_set(0);
    bus::backends::storage_modify(|snap| {
        snap.proto.dirty = false;
    });

    log::info!(
        "[device] proto committed: {} words, ver={:#06x}, blob={}",
        length, version, if new_active == 0 { 'A' } else { 'B' }
    );
    Ok(())
}

fn reload() -> AppResult<()> {
    // 1. 标记加载中 (atomic)
    bus::storage_state::proto_status_set(2);

    // 2. 从 NVS 读取 (若 NVS 不可用则使用空数据)
    let (data, version, length) = try_with_nvs(|nvs| {
        load_proto_from_nvs(nvs)
    })
    .unwrap_or_else(|| {
        log::warn!("[device] NVS unavailable on reload, using empty data");
        Ok(([0u16; PROTO_WORDS], 0, 0))
    })?;

    // 3. 写回快照: RCU RMW 仅替换 proto; device_text / holding_buf 保持不动.
    bus::storage_state::proto_status_set(0);
    bus::backends::storage_modify(|snap| {
        snap.proto.data = data.to_vec().into_boxed_slice();
        snap.proto.version = version;
        snap.proto.length = length;
        snap.proto.dirty = false;
    });

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

    // 1. 从 CONFIG RCU 取当前 SystemConfig (写者已通过 config_modify 落到 RCU)
    let cfg = bus::config_state::config_read()
        .map(|cs| cs.cfg.clone())
        .unwrap_or_else(|| {
            log::warn!("[device] CONFIG RCU unavailable, using defaults");
            crate::device::system_config::SystemConfig::defaults()
        });

    // 2. 持久化到 NVS (若 NVS 不可用则跳过)
    if let Some(result) = try_with_nvs_mut(|nvs| cfg.save_to_nvs(nvs)) {
        result?;
    } else {
        log::warn!("[device] NVS unavailable, cfg persist skipped");
    }

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
/// 加载设备文本区 (2000 字 = 4000 字节, NVS blob "dev_text")
///
/// 成功加载条件:
/// 1. NVS 存在 magic == DEV_TEXT_MAGIC
/// 2. blob 长度 == 4000 字节
///
/// 失败 → 返回默认空 2000 字
fn load_device_text_from_nvs(nvs: &EspDefaultNvs) -> AppResult<Vec<u16>> {
    let expected_bytes = regs::DEVICE_TEXT_COUNT as usize * 2; // 4000
    // 1. 校验 magic
    let magic = nvs
        .get_u16(NVS_KEY_DEV_TEXT_MAGIC)
        .ok()
        .flatten()
        .unwrap_or(0);
    if magic != DEV_TEXT_MAGIC {
        log::info!("[device] dev_text magic not found (got {:#06X}), using empty", magic);
        return Ok(vec![0u16; regs::DEVICE_TEXT_COUNT as usize]);
    }
    // 2. 读 blob
    let mut buf = vec![0u8; expected_bytes];
    let blob = match nvs.get_blob(NVS_KEY_DEV_TEXT, &mut buf) {
        Ok(Some(b)) if b.len() == expected_bytes => b,
        Ok(Some(b)) => {
            log::warn!("[device] dev_text blob truncated: {}/{}", b.len(), expected_bytes);
            return Ok(vec![0u16; regs::DEVICE_TEXT_COUNT as usize]);
        }
        Ok(None) => {
            log::info!("[device] dev_text blob not present, using empty");
            return Ok(vec![0u16; regs::DEVICE_TEXT_COUNT as usize]);
        }
        Err(e) => {
            log::warn!("[device] dev_text blob read err: {e:?}, using empty");
            return Ok(vec![0u16; regs::DEVICE_TEXT_COUNT as usize]);
        }
    };
    // 3. u16 LE 解码
    let mut data = vec![0u16; regs::DEVICE_TEXT_COUNT as usize];
    for (i, chunk) in blob.chunks_exact(2).enumerate() {
        data[i] = u16::from_le_bytes([chunk[0], chunk[1]]);
    }
    log::info!("[device] dev_text loaded: {} bytes", blob.len());
    Ok(data)
}

/// 持久化设备文本区 (整个 2000 字 = 4000 字节)
pub fn save_device_text_to_nvs(text: &[u16]) -> AppResult<()> {
    let expected_bytes = regs::DEVICE_TEXT_COUNT as usize * 2;
    if text.len() != regs::DEVICE_TEXT_COUNT as usize {
        return Err(AppError::Config(format!(
            "dev_text len mismatch: {} != {}",
            text.len(),
            regs::DEVICE_TEXT_COUNT as usize
        )));
    }
    let mut blob = [0u8; 4000]; // DEVICE_TEXT_COUNT(2000) * 2
    for (i, &v) in text.iter().enumerate() {
        blob[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
    }
    if let Some(result) = try_with_nvs_mut(|nvs| -> AppResult<()> {
        nvs.set_blob(NVS_KEY_DEV_TEXT, &blob[..expected_bytes])
            .map_err(|e| AppError::Config(format!("nvs set dev_text: {e:?}")))?;
        nvs.set_u16(NVS_KEY_DEV_TEXT_MAGIC, DEV_TEXT_MAGIC)
            .map_err(|e| AppError::Config(format!("nvs set dev_text magic: {e:?}")))?;
        Ok(())
    }) {
        result?;
    } else {
        log::warn!("[device] NVS unavailable, dev_text persist skipped");
    }
    Ok(())
}

/// 请求持久化设备文本区 (走 Actor mailbox, 异步执行)
/// 由 backends::write_hold_reg 在 DEVICE_TEXT_BASE 写后调用
///
/// LOOP8: 使用 storage_read_with 避免 clone 整个 StorageSnapshot (~11KB),
/// 减少 heap 碎片. 仅提取 device_text 字段.
pub fn request_save_device_text() {
    use crate::bus::storage_state::storage_read_with;
    // 直接持 RCU reader 读 device_text, 不 clone 整个快照
    let result = storage_read_with(|snap| {
        save_device_text_to_nvs(&snap.device_text)
    });
    if let Some(Err(e)) = result {
        log::warn!("[device] dev_text persist failed: {e}");
    }
}

fn load_proto_from_nvs(nvs: &EspDefaultNvs) -> AppResult<([u16; PROTO_WORDS], u16, u16)> {
    let data = [0u16; PROTO_WORDS];

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
    let idx = addr as usize;
    if idx >= 1500 {
        return None;
    }
    bus::storage_state::storage_read_with(|s| s.proto.data[idx])
}

/// 写入协议区指定地址 (0..1500). 走 RCU RMW; 越界直接返回 false (不浪费 clone).
pub fn proto_write(addr: u16, value: u16) -> bool {
    let idx = addr as usize;
    if idx >= 1500 {
        return false;
    }
    bus::backends::storage_modify(|snap| {
        snap.proto.data[idx] = value;
        snap.proto.dirty = true;
    });
    true
}

/// 批量读 (start..start+len, 越界自动截断). 持 RCU reader 遍历整段, 单次读.
pub fn proto_read_bulk(start: u16, len: u16) -> Vec<u16> {
    let mut out = Vec::with_capacity(len as usize);
    if let Some(snap) = bus::storage_state::storage_read() {
        for i in 0..len {
            let a = start.saturating_add(i) as usize;
            if a < 1500 {
                out.push(snap.proto.data[a]);
            } else {
                break;
            }
        }
    }
    out
}

/// 批量写. 走单次 RCU RMW, 把整段值与 dirty=true 一起原子替换.
pub fn proto_write_bulk(start: u16, values: &[u16]) -> bool {
    if values.is_empty() {
        return true;
    }
    bus::backends::storage_modify(|snap| {
        for (i, &v) in values.iter().enumerate() {
            let a = start.saturating_add(i as u16) as usize;
            if a < 1500 {
                snap.proto.data[a] = v;
            } else {
                break;
            }
        }
        snap.proto.dirty = true;
    });
    true
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
    // status 走 atomic 权威值; 快照内只是镜像, 旧/未刷新时不准.
    let status = bus::storage_state::proto_status();
    if let Some(s) = bus::storage_state::storage_read() {
        ProtoInfo {
            capacity: 1500,
            version: s.proto.version,
            length: s.proto.length,
            dirty: s.proto.dirty,
            status,
            magic: PROTO_MAGIC,
        }
    } else {
        ProtoInfo {
            capacity: 1500,
            version: 0,
            length: 0,
            dirty: false,
            status,
            magic: PROTO_MAGIC,
        }
    }
}
