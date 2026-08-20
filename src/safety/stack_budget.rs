//! 生产任务栈容量预算（单位：字节）。
//!
//! ESP-IDF pthread 默认从 `MALLOC_CAP_INTERNAL` 分配栈；即使启用了 PSRAM，
//! 这些栈仍会占用稀缺的内部 SRAM。所有用户 pthread 必须引用这里的常量，
//! 禁止在业务模块中继续散落魔法数字或按连接动态创建线程。

pub const KIB: usize = 1024;

pub const MAIN: usize = 24 * KIB;
pub const DEVICE_ACTOR: usize = 12 * KIB;
pub const MODBUS_RTU_PORT0: usize = 8 * KIB;
pub const MODBUS_RTU_PORT1: usize = 8 * KIB;
pub const MODBUS_RTU_PORT2: usize = 8 * KIB;
pub const MODBUS_RTU_MASTER: usize = MODBUS_RTU_PORT0;
pub const MODBUS_RTU_SLAVE: usize = MODBUS_RTU_PORT1;
pub const UDP_MULTICAST: usize = 4 * KIB;
pub const NFC: usize = 6 * KIB;
pub const HTTP: usize = 10 * KIB;
pub const WIFI_HEARTBEAT: usize = 6 * KIB;

pub const BLUEDROID_BTC: usize = 8 * KIB;
pub const BLUEDROID_BTU: usize = 8 * KIB;
pub const LWIP_TCPIP: usize = 8 * KIB;
pub const SYSTEM_EVENT: usize = 4 * KIB;
pub const FREERTOS_TIMER: usize = 4 * KIB;
pub const W5500_RX: usize = 4 * KIB;

/// 默认功能集常驻用户任务栈。
pub const DEFAULT_USER_STACK_TOTAL: usize =
    MAIN + DEVICE_ACTOR + MODBUS_RTU_PORT0 + MODBUS_RTU_PORT1 + UDP_MULTICAST + NFC + HTTP;

/// 可选任务全部启用时的用户任务栈上限（不含 ESP-IDF 系统任务）。
pub const ALL_USER_STACK_TOTAL: usize =
    DEFAULT_USER_STACK_TOTAL + MODBUS_RTU_PORT2 + WIFI_HEARTBEAT;

pub const KNOWN_SYSTEM_STACK_TOTAL: usize =
    BLUEDROID_BTC + BLUEDROID_BTU + LWIP_TCPIP + SYSTEM_EVENT + FREERTOS_TIMER + W5500_RX;

/// 用户任务栈预算硬上限。为 BLE/LwIP/事件/W5500、DMA 和内部堆保留空间。
pub const USER_STACK_BUDGET_LIMIT: usize = 128 * KIB;

const _: () = assert!(ALL_USER_STACK_TOTAL <= USER_STACK_BUDGET_LIMIT);
const _: () = assert!(DEVICE_ACTOR <= 12 * KIB);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_user_stacks_stay_within_internal_sram_budget() {
        assert_eq!(DEFAULT_USER_STACK_TOTAL, 72 * KIB);
        assert_eq!(ALL_USER_STACK_TOTAL, 86 * KIB);
        assert_eq!(KNOWN_SYSTEM_STACK_TOTAL, 36 * KIB);
        assert!(ALL_USER_STACK_TOTAL <= USER_STACK_BUDGET_LIMIT);
    }
}
