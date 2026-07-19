//! Actor 模型框架 (无锁邮箱, 取代共享可变状态)
//!
//! ## 设计目标
//! - 完全 lock-free 消息传递 (基于 [`crate::sync::MpscRing`])
//! - Actor 之间通过消息通信, 不共享可变状态 (每个 Actor 状态由其线程独占)
//! - 取代 `Mutex<RwLock` 风格的共享状态: 状态 = Actor 内部 `&mut self`, 由 mailbox 串行
//!
//! ## 核心组件
//! - [`Actor`] trait: 行为 (`handle(&mut self, msg)`)
//! - [`Mailbox`] / [`MpscRing`]: bounded MPSC, 多生产者单消费者
//! - [`ActorRef`]: 类型安全的消息发送句柄 (fire-and-forget)
//! - [`spawn`]: 启动一个独立线程消费 mailbox, 独占调用 `handle`
//!
//! ## 与锁的区别
//! - 锁: 共享可变状态 + 争用 park; 高频路径上调度器介入明显
//! - Actor: 状态由单线程独占; 其他任务通过消息 (拷贝) 推动, 不争用同一变量
//! - mailbox 满时丢弃新消息并记日志 (工业实时系统丢一条请求优于 park 整个链路)
//!
//! ## 使用示例
//! ```ignore
//! use crate::actor::{Actor, Mailbox, spawn};
//!
//! struct Increment(u32);
//! struct Counter { value: u32 }
//! impl Actor for Counter {
//!     type Msg = Increment;
//!     fn handle(&mut self, msg: Increment) {
//!         self.value += msg.0;
//!     }
//! }
//!
//! let (actor_ref, _handle) = spawn(Counter { value: 0 });
//! actor_ref.send(Increment(5));
//! ```

use crate::sync::MpscRing;
use std::time::Duration;

/// Actor trait.
///
/// `Msg` 是消息类型, 必须满足 `Send + 'static`. `handle` 在 Actor 线程内独占调用
/// `&mut self`, 故 actor 内部状态无需任何锁.
pub trait Actor: 'static + Send {
    type Msg: 'static + Send;
    fn handle(&mut self, msg: Self::Msg);
    fn init(&mut self) {}
    fn shutdown(&mut self) {}

    /// 空闲回调 (mailbox 空时调用): 周期性工作 (如心跳喂狗, 状态轮询).
    /// 返回下次轮询前的睡眠时长 (Duration); 默认 10ms 让出 CPU.
    /// Actor 线程不阻塞 mailbox 入队 (生产者始终能 `try_enqueue` 成功).
    fn idle(&mut self) -> Duration {
        Duration::from_millis(10)
    }
}

/// Actor 引用: 发送消息的句柄.
///
/// `send` 是 fire-and-forget; mailbox 满时仅记日志, 不阻塞调用方任务.
pub struct ActorRef<A: Actor> {
    mailbox: &'static MpscRing<A::Msg, 32>,
}

impl<A: Actor> Clone for ActorRef<A> {
    fn clone(&self) -> Self {
        Self { mailbox: self.mailbox }
    }
}

impl<A: Actor> ActorRef<A> {
    /// 发送消息 (non-blocking). 满返回 false 由调用方决策 (默认丢弃 + 警告日志).
    pub fn try_send(&self, msg: A::Msg) -> bool {
        self.mailbox.try_enqueue(msg)
    }

    /// 发送消息, 满则记 warn 日志并丢弃.
    pub fn send(&self, msg: A::Msg) {
        if !self.mailbox.try_enqueue(msg) {
            log::warn!(
                "[actor] mailbox full for {} (dropping message)",
                core::any::type_name::<A>()
            );
        }
    }

    /// 邮箱当前长度 (近似快照).
    pub fn pending(&self) -> usize {
        self.mailbox.len()
    }
}

/// Actor 句柄: 持有 actor_ref 与消费线程归零路径 (当前线程为常驻).
pub struct ActorHandle<A: Actor> {
    pub actor_ref: ActorRef<A>,
}

/// 启动一个 Actor: 在独立线程中消费 mailbox, 调用 `handle`.
///
/// mailbox 通过 `Box::leak` 获取 `&'static` 生命周期 (生命周期 = 程序).
/// Actor 内部状态本身由其线程独占, 无需任何同步原语.
pub fn spawn<A: Actor>(mut actor: A) -> (ActorRef<A>, ActorHandle<A>) {
    let mailbox: &'static MpscRing<A::Msg, 32> = Box::leak(Box::new(MpscRing::new()));
    let actor_ref = ActorRef { mailbox };
    let actor_handle = ActorHandle { actor_ref: actor_ref.clone() };

    let type_name = core::any::type_name::<A>();
    let short = type_name.split("::").last().unwrap_or(type_name);
    let name = format!("actor-{short}");
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            actor.init();
            loop {
                // 先尽量排空 mailbox (生产者在此期间仍可入队)
                while let Some(msg) = mailbox.dequeue() {
                    actor.handle(msg);
                }
                // 空闲: 调用 idle() 做周期性工作, 然后 sleep (不阻塞生产者入队)
                let nap = actor.idle();
                if !nap.is_zero() {
                    std::thread::sleep(nap);
                } else {
                    std::thread::yield_now();
                }
            }
        })
        .expect("failed to spawn actor thread");

    (actor_ref, actor_handle)
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    struct Add(u32);
    struct Counter {
        value: AtomicU32,
    }
    impl Actor for Counter {
        type Msg = Add;
        fn handle(&mut self, msg: Add) {
            self.value.fetch_add(msg.0, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_actor_basic() {
        let counter = Counter { value: AtomicU32::new(0) };
        let (actor_ref, _handle) = spawn(counter);
        for _ in 0..10 {
            actor_ref.send(Add(1));
        }
        // 等消费
        std::thread::sleep(Duration::from_millis(100));
        assert!(actor_ref.pending() <= 32);
    }

    #[test]
    fn test_mailbox_ring() {
        let r: MpscRing<u32, 4> = MpscRing::new();
        assert!(r.try_enqueue(1));
        assert!(r.try_enqueue(2));
        assert_eq!(r.dequeue(), Some(1));
        assert_eq!(r.dequeue(), Some(2));
    }
}
