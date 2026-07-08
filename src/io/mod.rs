//! IO 点管理 (DI/DO)
//!
//! - DI：8 路光耦隔离数字输入，1ms 去抖动扫描
//! - DO：8 路开漏输出，与 Modbus 线圈直接映射
//!
//! 启动入口：[`start`]

use std::sync::Arc;

use crate::error::AppResult;
use crate::hal::Hal;

pub mod di;
pub mod do_;

/// 启动 IO 扫描任务
pub fn start(_hal: Arc<Hal>) -> AppResult<()> {
    di::start_scan_task(_hal.clone())?;
    do_::start_output_task(_hal)?;
    Ok(())
}
