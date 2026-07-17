//! Modbus 协议栈测试 (CRC、帧解析、读写、异常)
//!
//! 这些测试不依赖 ESP-IDF, 可在 host 上运行 (`cargo test --tests modbus_tests`)
//! 注意: cargo test 不能直接编译 (项目是 no_std + 嵌入式), 我们使用条件编译。
//! 由于 esp-idf-sys 在 host 上无法链接, 这些测试需要在 device 上跑。

#![cfg(any())]  // 暂时禁用 (用 unit tests 替代, 见 src/ 模块内部)

// 实际的 Modbus 测试放在 src/modbus/shared.rs 末尾的 #[cfg(test)] mod tests 中。
