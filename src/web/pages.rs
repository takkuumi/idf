//! 嵌入式 HTML 页面 (登录 + 控制面板)
//!
//! HTML 源码以独立 `.html` 文件存放于本模块目录下, 编译期通过 `include_str!`
//! 宏内嵌为 `const &'static str`, 无需 SPIFFS/LittleFS 分区, 也避免在 Rust
//! 源码中维护超长 raw-string 字面量 (难以阅读 / IDE 高亮失效).
//!
//! - `login.html`  → `LOGIN_HTML`
//! - `index.html`  → `INDEX_HTML`
//!
//! 页面使用纯 vanilla JS + CSS, 零外部依赖, 通过 fetch() 调用 JSON API.

/// 登录页面
pub const LOGIN_HTML: &str = include_str!("login.html");

/// 控制面板主页
pub const INDEX_HTML: &str = include_str!("index.html");
