//! 大容量存储状态 - RCU (Read-Copy-Update) 无锁
//!
//! ## 之前 (parking_lot::Mutex<StorageState>)
//! - 11KB 大锁, Modbus 多寄存器写持锁 N 次
//! - 高并发时锁竞争明显
//!
//! ## 现在 (Rcu<StorageState>)
//! - 读: lock-free 原子加载
//! - 写: 构造新值, 原子替换并按 epoch 回收旧值
//! - 写极少 (commit/reload), 读极多 (Modbus 请求)
//!
//! ## 关键优化
//! - 写时构造完整新值, 一次原子 swap
//! - 写期间读者看到的是旧值, 不会看到部分更新
//! - 完全没有锁竞争

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::LazyLock;

use super::rcu::Rcu;

/// 协议存储区
///
/// 大数组使用 `Arc<[u16]>`。克隆快照只增加引用计数；写者再通过
/// `Arc::make_mut` 仅复制实际修改的数据区，避免每次 PRegBuf 写都深拷贝约 11KB。
#[derive(Clone)]
pub struct ProtoStore {
    pub data: Arc<[u16]>,
    pub version: u16,
    pub length: u16,
    pub dirty: bool,
    pub status: u8,
}

impl Default for ProtoStore {
    fn default() -> Self {
        Self {
            data: Arc::from(vec![0u16; 1500].into_boxed_slice()),
            version: 0,
            length: 0,
            dirty: false,
            status: 0,
        }
    }
}

/// 存储快照 (不可变)
///
/// 三个大数组字段均为 `Arc<[u16]>`；快照 clone 是固定开销，写时 COW。
#[derive(Clone)]
pub struct StorageSnapshot {
    pub proto: ProtoStore,
    /// 设备文本区 (5000-6999 = 2000 字)
    pub device_text: Arc<[u16]>,
    /// 通用 P区保持寄存器缓冲 (0x0880..0x107F = 2048 字)
    pub holding_buf: Arc<[u16]>,
}

impl StorageSnapshot {
    pub fn new() -> Self {
        Self {
            proto: ProtoStore::default(),
            device_text: Arc::from(vec![0u16; 2000].into_boxed_slice()),
            holding_buf: Arc::from(vec![0u16; 2048].into_boxed_slice()),
        }
    }
}

impl Default for StorageSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局存储 (RCU, lock-free 读)
pub static STORAGE: LazyLock<Rcu<StorageSnapshot>> =
    LazyLock::new(|| Rcu::new(StorageSnapshot::new()));

/// 读 (lock-free, 永远不阻塞)
pub fn storage_read() -> Option<Arc<StorageSnapshot>> {
    STORAGE.read().map(|s| Arc::new(s.clone()))
}

/// 写 (原子替换)
pub fn storage_write(snapshot: StorageSnapshot) {
    STORAGE.write(snapshot);
}

/// 读并执行 (无 Arc 分配)
pub fn storage_read_with<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&StorageSnapshot) -> R,
{
    STORAGE.read_with(f)
}

/// holding_buf NVS 持久化 dirty 标志 (LOOP13).
///
/// Modbus/AT/NFC 写 holding_buf 后置 true, DeviceActor 异步写入 NVS 后清 false.
/// 使用独立 AtomicBool 而非在 StorageSnapshot 里加 bool, 原因:
/// - holding_buf 写入通过 COW 发布，不能只为翻一个 bool 再发布一代快照
/// - NFC 仅读不写此标志, RCU 读者 (Modbus FC=03/04) 不需要它, 仅 DeviceActor 使用
pub static HOLDING_DIRTY: AtomicBool = AtomicBool::new(false);

/// holding_buf 相对 NFC 备份是否有本地新改动。不得与 `HOLDING_DIRTY` 共用：
/// NVS 成功和 NFC 成功是两个独立提交点，任一方清除另一方的 dirty 都会造成数据回滚。
pub static HOLDING_NFC_DIRTY: AtomicBool = AtomicBool::new(false);

/// 启动时是否从至少一个通过 magic/version/CRC 校验的 NVS holding blob 恢复成功。
/// NFC 自动冲突决策只在此值为 false 时允许用标签覆盖本机；有效 NVS 永远优先。
pub static HOLDING_NVS_VALID: AtomicBool = AtomicBool::new(false);

/// DO NVS 持久化 dirty 标志 (LOOP13).
///
/// `tick_do_output` 写硬件后同步置 true; main_loop 每 1s 节流检查并写 NVS.
/// 用于避免 DO 写频次 (~Hz 量级) 过高导致 NVS flash 过写.
pub static DO_NVS_DIRTY: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_basic() {
        let mut snap = StorageSnapshot::new();
        snap.proto.length = 42;
        storage_write(snap);
        let read = storage_read().unwrap();
        assert_eq!(read.proto.length, 42);
    }

    #[test]
    fn test_snapshot_clone_shares_large_regions_until_write() {
        let original = StorageSnapshot::new();
        let mut cloned = original.clone();
        assert!(Arc::ptr_eq(&original.proto.data, &cloned.proto.data));
        assert!(Arc::ptr_eq(&original.device_text, &cloned.device_text));
        assert!(Arc::ptr_eq(&original.holding_buf, &cloned.holding_buf));

        Arc::make_mut(&mut cloned.holding_buf)[0] = 0x55AA;
        assert!(!Arc::ptr_eq(&original.holding_buf, &cloned.holding_buf));
        assert!(Arc::ptr_eq(&original.proto.data, &cloned.proto.data));
        assert!(Arc::ptr_eq(&original.device_text, &cloned.device_text));
        assert_eq!(original.holding_buf[0], 0);
    }

    #[test]
    fn test_storage_concurrent() {
        use std::thread;
        storage_write(StorageSnapshot::new());
        let mut handles = vec![];
        for _ in 0..4 {
            let h = thread::spawn(|| {
                for _ in 0..100 {
                    let _ = storage_read();
                }
            });
            handles.push(h);
        }
        for i in 1..=5 {
            let mut snap = StorageSnapshot::new();
            snap.proto.length = i;
            storage_write(snap);
        }
        for h in handles {
            h.join().unwrap();
        }
    }
}

// ----------------------------------------------------------------------------
// 阶段 B: 把 `proto.status` (commit=1 / reload=2 / failed=3) 抽到独立原子
// ----------------------------------------------------------------------------
//
// 每次 Modbus 写一个 ProtoCommit/Reload 拍子就改 1 帧的 status；不应为此发布一代
// RCU 快照。用专属 `AtomicU8` 让 status 成为常量开销路径。
//
// 快照内的 `proto.status` 字段保留 (snapshot.version 兼容), 但其读端改由 `proto_status()`
// 取最新 atomic 值; 写端通过 `proto_status_set` 落 atomic, 写 snapshot 时同步镜像.

use std::sync::atomic::AtomicU8;

/// 全局 ProtoStore.status (权威): 0=idle, 1=committing, 2=loading, 3=failed.
pub static PROTO_STATUS_ATOMIC: AtomicU8 = AtomicU8::new(0);

/// 读当前 proto.status (原子).
pub fn proto_status() -> u8 {
    PROTO_STATUS_ATOMIC.load(Ordering::Acquire)
}

/// 设置当前 proto.status (原子). 写者调用.
pub fn proto_status_set(status: u8) {
    PROTO_STATUS_ATOMIC.store(status, Ordering::Release);
}
