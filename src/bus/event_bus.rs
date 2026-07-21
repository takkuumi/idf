//! 事件总线 (MpscRing, 无 parking_lot / 无 std::sync::Mutex)
//!
//! ## 目的
//! 任务间事件通知 (DI 变化, AI 采样完成等)
//!
//! ## 实现说明
//! 之前用 `std::sync::Mutex<heapless::spsc::Queue>` 包装 SPSC Queue (因为 Queue 不能直接 Sync).
//! 现在改用 [`crate::sync::MpscRing`]: 内部 Spin 串行化 enqueue, 不阻塞 OS 调度器, 满即覆盖最旧.
//!
//! ## 性能
//! - push: O(1) 加一短自旋 enqueue (~纳秒级, 不 park)
//! - pop: O(1) 单消费者出队
//! - 队列满自动覆盖最旧 (不会阻塞, 不丢最新事件的整体上下文)

use std::sync::LazyLock;

use crate::sync::MpscRing;

/// IO 事件类型
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoEvent {
    /// DI 状态变化
    DiChanged,
    /// DO 状态变化
    DoChanged,
    /// AI 采样完成
    AiSampled,
    /// AO 输出更新
    AoUpdated,
    /// 设备复位请求
    ResetRequested,
    /// IP 已分配 (DHCP 完成), 携带 (ip, mask, gw)
    IpAssigned([u8; 4], [u8; 4], [u8; 4]),
}

/// 全局事件队列 (容量 32, 满了覆盖最旧)
static IO_EVENTS: LazyLock<MpscRing<IoEvent, 32>> = LazyLock::new(MpscRing::new);

/// 发送事件 (non-blocking, 满了覆盖最旧)
pub fn send_event(event: IoEvent) {
    IO_EVENTS.enqueue_drop_oldest(event);
}

/// 接收事件 (non-blocking, 空则返回 None)
pub fn recv_event() -> Option<IoEvent> {
    IO_EVENTS.dequeue()
}

/// 检查是否有事件
pub fn has_event() -> bool {
    !IO_EVENTS.is_empty()
}

/// 当前事件队列长度
pub fn event_count() -> usize {
    IO_EVENTS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_basic() {
        send_event(IoEvent::DiChanged);
        let _ = recv_event();
    }

    #[test]
    fn test_event_overflow() {
        // 填满队列 (32 个) + 再发送 5 个
        for _ in 0..40 {
            send_event(IoEvent::AiSampled);
        }
        let count = event_count();
        assert!(count <= 32);
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    #[test]
    fn test_recv_returns_some() {
        // 测试 recv 能返回事件
        send_event(IoEvent::ResetRequested);
        assert!(recv_event().is_some());
    }

    #[test]
    fn test_event_count() {
        let initial = event_count();
        send_event(IoEvent::DiChanged);
        send_event(IoEvent::DoChanged);
        let new_count = event_count();
        assert!(new_count >= initial);
    }
}
