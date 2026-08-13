//! HTTP Web 配置服务器 (现代化 JSON API)
//!
//! 移植自参考固件 MCA_F16V2_1_F48_BLE.ino 的 WebServer (端口 80),
//! 现代化升级为纯 JSON API (application/json), 去除 HTML 模板依赖.
//!
//! ## API 路由 (对齐参考固件, 现代化 JSON 格式)
//!
//! | 方法 | 路径 | 功能 |
//! |------|------|------|
//! | GET | `/` | 重定向到 `/login` |
//! | POST | `/login` | 登录 `{username, pwd}` → JSON `{type, code, data}` |
//! | GET | `/getsysteminfo` | 设备信息 (name/manufacturer/model/version) |
//! | POST | `/updatesysteminfoconfig` | 更新设备信息 |
//! | GET | `/getnfcstatus` | 查询 NFC 维护状态 |
//! | POST | `/nfcbackup` | 触发配置备份到 NFC |
//! | POST | `/nfcrestore` | 触发从 NFC 恢复配置 |
//! | GET | `/getportconfig` | RS485 端口配置 (3 路) |
//! | POST | `/updateportconfig` | 更新 RS485 端口配置 |
//! | GET | `/getiodata` | DI/DO/AI 实时数据 |
//! | POST | `/iocontrol` | DO 输出控制 |
//! | GET | `/getnetworkconfig` | 网络配置 (IP/Mask/GW/DNS/MAC/SN) |
//! | POST | `/updatenetworkconfig` | 更新网络配置 |
//! | POST | `/updatepwd` | 修改密码 |
//! | POST | `/updateota` | OTA 固件上传 |
//!
//! ## 认证
//!
//! Cookie-based 会话认证 (LOOP11 升级, 替代旧的硬编码 `ESPSESSIONID=1`):
//! - 未认证请求 → 301 重定向到 `/login`
//! - POST `/login` 验证成功 → 200 JSON + `Set-Cookie: ESPSESSIONID=<32hex>`
//!   (16 字节 TRNG 随机 token 的 hex 编码, HttpOnly; Path=/; Max-Age=86400)
//! - 服务端 `SESSION` 全局保存活跃 token, `is_authenticated()` 常量时间比较
//! - `/logout` 销毁服务端会话 + 清除浏览器 Cookie
//! - 默认密码: admin/admin123 (可修改, 明文存储到 NVS, 对齐 MCA `/WebPwd.txt`)
//!
//! ## 性能
//!
//! - 单线程 accept + 短连接 (连接处理 ~10ms, 无需线程池)
//! - 响应体手写 JSON (零分配, 不依赖 serde/serde_json)
//! - 收发缓冲区 4KB (足够单条 HTTP 请求/响应)
//! - 非阻塞 accept, 每 10ms 轮询一次 (独立 HTTP 任务)

mod pages;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::bus::storage_state::storage_read_with;
use crate::bus::{IO, config_state::config_read_with};
use crate::config::regs;
use crate::error::AppResult;
use crate::health::{self, TaskHb};

/// HTTP 端口 (对齐参考固件: server(80))
const HTTP_PORT: u16 = 80;
/// 无连接时的 accept 轮询周期，兼顾新连接延迟与 CPU 让出。
const ACCEPT_POLL_MS: u64 = 10;
/// 收发缓冲区大小 (流式 OTA/header 读取的单块大小)
const BUF_SIZE: usize = 4096;
/// 单条 HTTP header 行最大字节数 (防止恶意超长 header → OOM)
const MAX_HEADER_LINE: usize = 1024;
/// 配置类 POST body 上限。现有表单均小于 4KB，保留 16KB 兼容余量；OTA 走独立流式路径。
const MAX_BODY_SIZE: usize = 16 * 1024;
/// 流式 OTA body 总量上限 (与 flash 分区大小一致: ota 分区 2.25MB)
const MAX_OTA_SIZE: usize = 0x24_0000;
/// HTTP 头总量和字段数量上限，避免大量短 header 消耗 PSRAM。
const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_HEADER_COUNT: usize = 32;
const MAX_HTTP_METHOD: usize = 8;
const MAX_HTTP_PATH: usize = 192;
const MAX_COOKIE_VALUE: usize = 512;
const MAX_FORM_VALUE: usize = 256;
const JSON_RESPONSE_MAX: usize = 128;
/// 默认用户名
const DEFAULT_USERNAME: &str = "admin";
/// 默认密码 (NVS 未保存时使用)
const DEFAULT_PASSWORD: &str = "admin123";

/// NVS 密码 key
const NVS_KEY_WEB_PWD: &str = "web_pwd";
const NVS_KEY_WEB_PWD_FLAG: &str = "web_pwd_f";
/// 原 MCA `/SystemInfo.txt` 的三个字段改为单个定长 NVS blob，避免掉电时只更新部分字段。
const NVS_KEY_WEB_SYSTEM_INFO: &str = "web_sysinfo";
const WEB_SYSTEM_INFO_FIELD_LEN: usize = 63;
const WEB_SYSTEM_INFO_BLOB_LEN: usize = (WEB_SYSTEM_INFO_FIELD_LEN + 1) * 3;
#[cfg(feature = "f4")]
const MCA_WEB_VERSION: &str = "F4";
#[cfg(not(feature = "f4"))]
const MCA_WEB_VERSION: &str = "F3";

/// 会话有效期 (秒): 24 小时 (与 Cookie Max-Age 一致)
const SESSION_TTL_SECS: u64 = 86400;

#[derive(Clone, Debug, PartialEq, Eq)]
struct WebSystemInfo {
    device_name: String,
    manufacturer: String,
    model: String,
}

fn valid_web_system_text(value: &str) -> bool {
    value.len() <= WEB_SYSTEM_INFO_FIELD_LEN
        && !value.as_bytes().contains(&0)
        && !value.chars().any(char::is_control)
}

fn encode_web_system_info(info: &WebSystemInfo) -> Option<[u8; WEB_SYSTEM_INFO_BLOB_LEN]> {
    if !valid_web_system_text(&info.device_name)
        || !valid_web_system_text(&info.manufacturer)
        || !valid_web_system_text(&info.model)
    {
        return None;
    }

    let mut blob = [0u8; WEB_SYSTEM_INFO_BLOB_LEN];
    for (index, value) in [&info.device_name, &info.manufacturer, &info.model]
        .iter()
        .enumerate()
    {
        let offset = index * (WEB_SYSTEM_INFO_FIELD_LEN + 1);
        let bytes = value.as_bytes();
        blob[offset] = bytes.len() as u8;
        blob[offset + 1..offset + 1 + bytes.len()].copy_from_slice(bytes);
    }
    Some(blob)
}

fn decode_web_system_info(blob: &[u8]) -> Option<WebSystemInfo> {
    if blob.len() != WEB_SYSTEM_INFO_BLOB_LEN {
        return None;
    }
    let mut fields: [String; 3] = core::array::from_fn(|_| String::new());
    for (index, field) in fields.iter_mut().enumerate() {
        let offset = index * (WEB_SYSTEM_INFO_FIELD_LEN + 1);
        let length = blob[offset] as usize;
        if length > WEB_SYSTEM_INFO_FIELD_LEN {
            return None;
        }
        *field = core::str::from_utf8(&blob[offset + 1..offset + 1 + length])
            .ok()?
            .to_string();
    }
    Some(WebSystemInfo {
        device_name: core::mem::take(&mut fields[0]),
        manufacturer: core::mem::take(&mut fields[1]),
        model: core::mem::take(&mut fields[2]),
    })
}

fn load_web_system_info() -> WebSystemInfo {
    let fallback = WebSystemInfo {
        device_name: "工业测控执行器".to_string(),
        manufacturer: String::new(),
        model: "MR-MCA-200".to_string(),
    };
    crate::device::try_with_nvs(|nvs| {
        let mut blob = [0u8; WEB_SYSTEM_INFO_BLOB_LEN];
        nvs.get_blob(NVS_KEY_WEB_SYSTEM_INFO, &mut blob)
            .ok()
            .flatten()
            .and_then(decode_web_system_info)
    })
    .flatten()
    .unwrap_or(fallback)
}

fn save_web_system_info(info: &WebSystemInfo) -> bool {
    let Some(blob) = encode_web_system_info(info) else {
        return false;
    };
    matches!(
        crate::device::try_with_nvs_mut(|nvs| nvs
            .set_blob(NVS_KEY_WEB_SYSTEM_INFO, &blob)
            .map_err(|e| format!("{e:?}"))),
        Some(Ok(()))
    )
}

// ============================================================================
// LOOP11: 会话管理 (替代硬编码 ESPSESSIONID=1)
// ============================================================================

/// 会话状态 (单会话设备: 同一时间仅允许一个管理员登录)
/// - `token`: 16 字节硬件随机数, hex 编码后作为 Cookie 值
/// - `active`: 当前会话是否有效 (logout 时清零)
struct Session {
    token: [u8; 16],
    active: bool,
    created_at: Option<std::time::Instant>,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    token: [0u8; 16],
    active: false,
    created_at: None,
});

/// 生成 16 字节硬件随机 token (生产路径: ESP32 TRNG; 测试路径: 固定序列).
/// 设备上调用 `esp_random()` 4 次填充 16 字节; 主机测试无法链接 esp-idf-sys,
/// 使用固定可预测值 (仅用于验证 hex 编码逻辑, 不用于真实鉴权).
fn generate_session_token() -> [u8; 16] {
    #[cfg(not(test))]
    {
        let mut token = [0u8; 16];
        for chunk in token.chunks_mut(4) {
            let r = unsafe { esp_idf_sys::esp_random() };
            let bytes = r.to_le_bytes();
            let n = chunk.len().min(4);
            chunk[..n].copy_from_slice(&bytes[..n]);
        }
        token
    }
    #[cfg(test)]
    {
        // 测试用固定序列: 0x01..0x10, 验证编码/解码对称性
        let mut token = [0u8; 16];
        for (i, b) in token.iter_mut().enumerate() {
            *b = (i + 1) as u8;
        }
        token
    }
}

/// 创建新会话并返回 Set-Cookie 字符串.
fn create_session_cookie() -> String {
    let token = generate_session_token();
    let hex = hex_encode_16(&token);
    if let Ok(mut s) = SESSION.lock() {
        s.token = token;
        s.active = true;
        s.created_at = Some(std::time::Instant::now());
    }
    // Set-Cookie: ESPSESSIONID=<32hex>; HttpOnly; Path=/; Max-Age=86400
    format!(
        "ESPSESSIONID={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        std::str::from_utf8(&hex).unwrap_or(""),
        SESSION_TTL_SECS
    )
}

/// 销毁服务端会话 (logout 调用)
fn destroy_session() {
    if let Ok(mut s) = SESSION.lock() {
        s.token = [0u8; 16];
        s.active = false;
        s.created_at = None;
    }
}

/// 验证 Cookie 中的 token 是否与会话匹配 (常量时间比较, 防时序攻击)
fn validate_session_cookie(token_hex: &[u8]) -> bool {
    if token_hex.len() != 32 {
        return false;
    }
    if let Ok(mut s) = SESSION.lock() {
        if !s.active {
            return false;
        }
        let expired = s
            .created_at
            .map(|created| created.elapsed().as_secs() >= SESSION_TTL_SECS)
            .unwrap_or(true);
        if expired {
            s.token = [0u8; 16];
            s.active = false;
            s.created_at = None;
            return false;
        }
        // 对比 hex(token) == cookie_hex (常量时间)
        let expected = hex_encode_16(&s.token);
        let mut diff = 0u8;
        for i in 0..32 {
            diff |= expected[i] ^ token_hex[i];
        }
        diff == 0
    } else {
        false
    }
}

/// 16 字节 → 32 字符小写 hex (无分配, 写入固定 32 字节缓冲)
fn hex_encode_16(src: &[u8; 16]) -> [u8; 32] {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = [0u8; 32];
    for i in 0..16 {
        out[i * 2] = HEX[(src[i] >> 4) as usize];
        out[i * 2 + 1] = HEX[(src[i] & 0x0f) as usize];
    }
    out
}

/// HTTP 服务器任务心跳 (阈值 30s, 5s accept 循环 + 余量)
static TASK_HB: TaskHb = TaskHb::new_with_stall("http-srv", 30);
static STARTED: AtomicBool = AtomicBool::new(false);

/// 启动 HTTP Web 服务器 (后台线程)
pub fn start() -> AppResult<()> {
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    let spawn_result = std::thread::Builder::new()
        .name("http-srv".into())
        // LOOP18: http-srv 栈从 8KB 提到 12KB.
        //  - OTA 上传期间单帧 buf 已缩到 2048B (见 handle_ota_upload_stream)
        //  - 但 handle_get_io_data + handle_get_system_status 等长路径
        //    单次请求会构造 ~30+ format! 临时字符串, 加上 BufReader 内部状态
        //    + LwIP socket 状态, 实测峰值接近 7KB. 8KB 边界易触发 Stack canary.
        //  - 与 CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=12288 对齐, 移除
        //    BLE/ETH/Modbus 同时启动时的栈压力来源.
        .stack_size(crate::safety::stack_budget::HTTP)
        .spawn(server_loop);
    if let Err(e) = spawn_result {
        return Err(crate::error::AppError::Sys(format!("spawn http: {e}")));
    }
    STARTED.store(true, Ordering::Release);
    crate::health::register_with_stack(&TASK_HB, crate::safety::stack_budget::HTTP);
    log::info!("[http] web server started on port {}", HTTP_PORT);
    Ok(())
}

pub fn is_started() -> bool {
    STARTED.load(Ordering::Acquire)
}

/// 服务器主循环
fn server_loop() {
    // LOOP9: 订阅硬件 WDT, 否则 feed_wdt() 高频报 "task not found" 刷屏
    health::subscribe_wdt();
    loop {
        TASK_HB.tick();
        health::feed_wdt();

        let listener = match TcpListener::bind(("0.0.0.0", HTTP_PORT)) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[http] bind failed: {}, retry in 5s", e);
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        if let Err(e) = listener.set_nonblocking(true) {
            log::warn!("[http] set_nonblocking failed: {}, retry in 5s", e);
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        log::info!("[http] listening on 0.0.0.0:{}", HTTP_PORT);

        loop {
            TASK_HB.tick();
            health::feed_wdt();

            match listener.accept() {
                Ok((stream, _addr)) => {
                    if let Err(e) = stream.set_nodelay(true) {
                        log::warn!("[http] set TCP_NODELAY failed: {}", e);
                        continue;
                    }
                    if let Err(e) = stream.set_read_timeout(Some(Duration::from_secs(3))) {
                        log::warn!("[http] set read timeout failed: {}", e);
                        continue;
                    }
                    if let Err(e) = stream.set_write_timeout(Some(Duration::from_secs(3))) {
                        log::warn!("[http] set write timeout failed: {}", e);
                        continue;
                    }
                    if let Err(e) = handle_connection(stream) {
                        log::debug!("[http] connection error: {}", e);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // 10ms 可将 Web 新连接的额外延迟控制在人机界面无感范围，
                    // 同时保留 sleep 让出 CPU，避免无连接时忙轮询。
                    std::thread::sleep(Duration::from_millis(ACCEPT_POLL_MS));
                }
                Err(e) => {
                    log::warn!("[http] accept error: {}", e);
                    break; // 重新 bind
                }
            }
        }
    }
}

// ============================================================================
// 请求解析
// ============================================================================

/// 解析后的 HTTP 请求
struct HttpRequest {
    method: heapless::String<MAX_HTTP_METHOD>,
    path: heapless::String<MAX_HTTP_PATH>,
    cookie: heapless::String<MAX_COOKIE_VALUE>,
    body: Vec<u8>,
}

impl HttpRequest {
    /// 获取指定 header 值
    fn header(&self, name: &str) -> Option<&str> {
        name.eq_ignore_ascii_case("Cookie")
            .then_some(self.cookie.as_str())
            .filter(|value| !value.is_empty())
    }

    /// 检查 Cookie 中是否有有效的会话 token.
    ///
    /// LOOP11: 替代旧 `ESPSESSIONID=1` 字面量, 改为随机 token 验证.
    /// 从 Cookie header 提取 `ESPSESSIONID` 值, 与全局 SESSION 中的
    /// hex(token) 做常量时间比较.
    fn is_authenticated(&self) -> bool {
        let Some(cookie) = self.header("Cookie") else {
            return false;
        };
        for pair in cookie.split(';') {
            let kv = pair.trim();
            if let Some((k, v)) = kv.split_once('=') {
                if k.trim() == "ESPSESSIONID" {
                    return validate_session_cookie(v.trim().as_bytes());
                }
            }
        }
        false
    }

    /// 从 body 中解析 URL 编码的表单字段
    fn form_field(&self, name: &str) -> Option<heapless::String<MAX_FORM_VALUE>> {
        let body = std::str::from_utf8(&self.body).ok()?;
        for pair in body.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let val = parts.next().unwrap_or("");
            if key == name {
                return url_decode_into(val);
            }
        }
        None
    }
}

fn url_decode_bounded(s: &str) -> Option<heapless::String<MAX_HTTP_PATH>> {
    url_decode_into(s)
}

fn url_decode_into<const N: usize>(s: &str) -> Option<heapless::String<N>> {
    let bytes = s.as_bytes();
    let mut decoded = heapless::Vec::<u8, N>::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = core::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                index += 3;
                decoded.push(u8::from_str_radix(hex, 16).ok()?).ok()?;
            }
            b'+' => {
                index += 1;
                decoded.push(b' ').ok()?;
            }
            byte => {
                index += 1;
                decoded.push(byte).ok()?;
            }
        }
    }
    let text = core::str::from_utf8(&decoded).ok()?;
    let mut out = heapless::String::new();
    out.push_str(text).ok()?;
    Some(out)
}

// ============================================================================
// 响应构建 (手写 JSON, 零分配)
// ============================================================================

/// 发送 HTTP 响应
fn send_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        400 => "Bad Request",
        401 => "Unauthorized",
        413 => "Payload Too Large",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        status_text,
        content_type,
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}

/// 发送 JSON 响应 (application/json)
fn send_json(stream: &mut TcpStream, status: u16, json: &str) -> std::io::Result<()> {
    send_response(stream, status, "application/json", json.as_bytes())
}

/// 发送带 Set-Cookie 的 JSON 响应 (LOOP11: 登录成功专用).
///
/// 旧实现 `send_redirect_with_cookie` 返回 301 + 空 body, 但 login.html 用
/// `fetch().then(r => r.json())` 期望 JSON body — fetch 自动跟随 301 拿到 INDEX_HTML
/// 后 `r.json()` 抛 SyntaxError, 导致登录看似无反应. 改为 200 + JSON + Set-Cookie,
/// login.html 收到 `code:"0"` 后自行 `location.href='/'`.
fn send_json_with_cookie(
    stream: &mut TcpStream,
    status: u16,
    json: &str,
    cookie: &str,
) -> std::io::Result<()> {
    let status_text = match status {
        200 => "OK",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nSet-Cookie: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        status_text,
        cookie,
        json.len()
    )?;
    stream.write_all(json.as_bytes())?;
    Ok(())
}

/// 发送 301 重定向
fn send_redirect(stream: &mut TcpStream, location: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 301 Moved Permanently\r\nLocation: {}\r\nCache-Control: no-cache\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        location
    )?;
    Ok(())
}

/// 构建标准 JSON 响应 (对齐参考固件 returnResponseJson,
/// 包含 `msg` 字段以兼容旧前端 WebIndex.cpp error 提示)
fn json_response(response_type: &str, code: &str) -> heapless::String<JSON_RESPONSE_MAX> {
    use core::fmt::Write as _;
    let mut out = heapless::String::new();
    let result = write!(
        out,
        r#"{{"type":"{}","code":"{}","msg":"","data":{{}}}}"#,
        response_type, code
    );
    debug_assert!(result.is_ok());
    out
}

/// 有界读取一行。`BufRead::read_line` 只有在返回后才能检查长度，攻击者若一直不发
/// 换行会让 String 无界增长；Take 将单行实际读取量限制为 MAX_HEADER_LINE + 1。
fn read_bounded_header_line(
    reader: &mut BufReader<TcpStream>,
    line: &mut heapless::String<MAX_HEADER_LINE>,
) -> std::io::Result<usize> {
    line.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(line.len());
        }
        let take = available
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len() + take > line.capacity() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http header line too long",
            ));
        }
        let text = core::str::from_utf8(&available[..take]).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "http header is not utf-8")
        })?;
        let complete = text.ends_with('\n');
        line.push_str(text).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "http header line too long")
        })?;
        reader.consume(take);
        if complete {
            return Ok(line.len());
        }
    }
}

/// 解析请求行 + headers (不含 body)
///
/// LOOP9 安全加固:
/// - header 行长度上限 MAX_HEADER_LINE (防恶意超长 header → OOM)
/// 返回 (HttpRequest 框架, content_length, reader) — body 由调用方按需读取,
/// OTA 走流式 (handle_ota_upload_stream), 其余路由走 read_exact 上限 MAX_BODY_SIZE.
fn parse_request_headers(
    stream: TcpStream,
) -> std::io::Result<(HttpRequest, usize, BufReader<TcpStream>)> {
    let mut reader = BufReader::new(stream);
    // 读请求行 (限制长度)
    let mut request_line = heapless::String::<MAX_HEADER_LINE>::new();
    if read_bounded_header_line(&mut reader, &mut request_line)? == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "empty http request",
        ));
    }
    let mut parts = request_line.trim().splitn(3, ' ');
    let method_raw = parts.next().unwrap_or("");
    let path_raw = parts.next().unwrap_or("/");
    let method = heapless::String::try_from(method_raw).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "http method too long")
    })?;
    let path = url_decode_bounded(path_raw).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "http path too long or invalid",
        )
    })?;

    // 读 Headers (每行限制长度)
    let mut cookie = heapless::String::<MAX_COOKIE_VALUE>::new();
    let mut content_length = 0usize;
    let mut header_count = 0usize;
    let mut header_bytes = request_line.len();
    loop {
        let mut line = heapless::String::<MAX_HEADER_LINE>::new();
        if read_bounded_header_line(&mut reader, &mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "http headers not terminated",
            ));
        }
        header_bytes = header_bytes.saturating_add(line.len());
        if header_bytes > MAX_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http headers too large",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if header_count >= MAX_HEADER_COUNT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "too many http headers",
            ));
        }
        header_count += 1;
        if let Some((k, v)) = trimmed.split_once(':') {
            let name = k.trim();
            let value = v.trim();
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = value.parse().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid Content-Length")
                })?;
            } else if name.eq_ignore_ascii_case("Cookie") {
                cookie.push_str(value).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Cookie too long")
                })?;
            }
        }
    }

    Ok((
        HttpRequest {
            method,
            path,
            cookie,
            body: Vec::new(),
        },
        content_length,
        reader,
    ))
}

/// 解析请求行 + headers + body (body 走 read_exact, 上限 MAX_BODY_SIZE).
/// OTA 路由不调用此函数 (走流式).
fn parse_request(stream: TcpStream) -> std::io::Result<HttpRequest> {
    let (mut req, content_length, mut reader) = parse_request_headers(stream)?;
    if content_length > MAX_BODY_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("body too large: {} > {}", content_length, MAX_BODY_SIZE),
        ));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    req.body = body;
    Ok(req)
}

// ============================================================================
// 连接处理
// ============================================================================

fn handle_connection(stream: TcpStream) -> std::io::Result<()> {
    // LOOP9: OTA 走流式路径, 不缓存整 body (防 OOM); 其余路由缓存 body (上限 MAX_BODY_SIZE)
    let path_peek = parse_request_headers(stream)?;
    let req = path_peek.0;
    let content_length = path_peek.1;
    let mut reader = path_peek.2;

    log::debug!(
        "[http] {} {} (auth={})",
        req.method,
        req.path,
        req.is_authenticated()
    );

    // OTA 流式: 直接消费 reader, 不构造完整 body
    if req.path == "/updateota" {
        if req.method != "POST" {
            let mut s = reader.into_inner();
            send_json(&mut s, 405, &json_response("updateota", "405"))?;
            return Ok(());
        }
        if !req.is_authenticated() {
            let mut s = reader.into_inner();
            send_redirect(&mut s, "/login")?;
            return Ok(());
        }
        // 必须把现有 BufReader 原样传入。into_inner() 会丢弃解析 header 时已经
        // 预读到 BufReader 内部的固件字节，导致 OTA 固件头缺失或上传永久超时。
        return handle_ota_upload_stream(reader, content_length);
    }

    // 非 OTA: 缓存 body (上限校验)
    let mut req = req;
    if content_length > MAX_BODY_SIZE {
        let mut s = reader.into_inner();
        send_json(&mut s, 413, &json_response("error", "413"))?;
        return Ok(());
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
        req.body = body;
        let stream = reader.into_inner();
        return dispatch_request(stream, req);
    }
    // content_length == 0: 无 body, 直接 dispatch
    let stream = reader.into_inner();
    dispatch_request(stream, req)
}

/// 分发已解析的请求到各路由处理器
fn dispatch_request(mut stream: TcpStream, req: HttpRequest) -> std::io::Result<()> {
    match req.path.as_str() {
        // ---- 公开路由 (无需认证) ----
        "/login" => handle_login(&mut stream, &req),
        "/logout" => {
            // LOOP11: 销毁服务端会话 + 清除浏览器 Cookie
            destroy_session();
            write!(
                stream,
                "HTTP/1.1 301 Moved Permanently\r\nLocation: /login\r\nCache-Control: no-cache\r\nSet-Cookie: ESPSESSIONID=0; Path=/; Max-Age=0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
            Ok(())
        }
        "/favicon.ico" => send_response(&mut stream, 204, "text/plain", b""),
        "/" => {
            if req.is_authenticated() {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    pages::INDEX_HTML.as_bytes(),
                )
            } else {
                send_redirect(&mut stream, "/login")
            }
        }

        // ---- 其余路由均需认证 ----
        _ => {
            if !req.is_authenticated() {
                return send_redirect(&mut stream, "/login");
            }
            match req.path.as_str() {
                "/getsysteminfo" => handle_get_system_info(&mut stream, &req),
                "/updatesysteminfoconfig" => handle_update_system_info(&mut stream, &req),
                "/getportconfig" => handle_get_port_config(&mut stream, &req),
                "/updateportconfig" => handle_update_port_config(&mut stream, &req),
                "/getiodata" => handle_get_io_data(&mut stream, &req),
                "/iocontrol" => handle_io_control(&mut stream, &req),
                "/getnetworkconfig" => handle_get_network_config(&mut stream, &req),
                "/updatenetworkconfig" => handle_update_network_config(&mut stream, &req),
                "/getbleconfig" => handle_get_ble_config(&mut stream, &req),
                "/updatebleconfig" => handle_update_ble_config(&mut stream, &req),
                "/getsensorconfig" => handle_get_sensor_config(&mut stream, &req),
                "/updatesensorconfig" => handle_update_sensor_config(&mut stream, &req),
                "/getsystemstatus" => handle_get_system_status(&mut stream, &req),
                "/getnfcstatus" => handle_get_nfc_status(&mut stream, &req),
                "/nfcbackup" => handle_nfc_backup(&mut stream, &req),
                "/nfcrestore" => handle_nfc_restore(&mut stream, &req),
                "/reboot" => handle_reboot(&mut stream, &req),
                "/updatepwd" => handle_update_password(&mut stream, &req),
                _ => send_json(&mut stream, 404, &json_response("error", "404")),
            }
        }
    }
}

// ============================================================================
// 路由处理器
// ============================================================================

/// POST /login
fn handle_login(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        // GET: 返回登录页面
        return send_response(
            stream,
            200,
            "text/html; charset=utf-8",
            pages::LOGIN_HTML.as_bytes(),
        );
    }
    let username = req.form_field("username").unwrap_or_default();
    let pwd = req.form_field("pwd").unwrap_or_default();

    let actual_pwd = load_web_password();

    if username != DEFAULT_USERNAME {
        return send_json(stream, 200, &json_response("login", "0x01000001"));
    }
    if pwd.as_str() != actual_pwd {
        return send_json(stream, 200, &json_response("login", "0x01000002"));
    }
    // LOOP11: 返回 200 JSON + 随机 token Cookie (不再 301 空 body, 否则
    // login.html 的 fetch().then(r=>r.json()) 解析失败导致登录看似无反应)
    let cookie = create_session_cookie();
    send_json_with_cookie(stream, 200, &json_response("login", "0"), &cookie)
}

/// GET /getsysteminfo — 对齐 C++ handleGetSystemInfo: devicename/manufacturer/model/version
fn handle_get_system_info(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let Some((address_info, fw_ver, hw_ver, fw_date)) = config_read_with(|cs| {
        let cfg = &cs.cfg;
        (cfg.name_str(), cfg.fw_version, cfg.hw_version, cfg.fw_date)
    }) else {
        return send_json(stream, 500, &json_response("getsysteminfo", "500"));
    };
    let info = load_web_system_info();
    let json = format!(
        r#"{{"type":"getsysteminfo","code":"0","msg":"","data":{{"devicename":"{}","manufacturer":"{}","model":"{}","version":"V{}.{}.{}.{}","hw_version":"0x{:04X}","addressinfo":"{}"}}}}"#,
        escape_json(&info.device_name),
        escape_json(&info.manufacturer),
        escape_json(&info.model),
        fw_ver / 100,
        (fw_ver % 100) / 10,
        fw_ver % 10,
        fw_date,
        hw_ver,
        escape_json(&address_info),
    );
    send_json(stream, 200, &json)
}

/// POST /updatesysteminfoconfig
fn handle_update_system_info(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updatesysteminfoconfig", "405"));
    }
    let devicename = req.form_field("devicename");
    let manufacturer = req.form_field("manufacturer");
    let model = req.form_field("model");
    let addressinfo = req.form_field("addressinfo");

    if devicename.is_none() && manufacturer.is_none() && model.is_none() && addressinfo.is_none() {
        return send_json(
            stream,
            200,
            &json_response("updatesysteminfoconfig", "0x01000001"),
        );
    }
    if addressinfo
        .as_ref()
        .is_some_and(|value| value.as_bytes().len() > 16)
    {
        return send_json(
            stream,
            200,
            &json_response("updatesysteminfoconfig", "0x01000003"),
        );
    }
    log::info!(
        "[http] update system info: devicename={:?}, manufacturer={:?}, model={:?}",
        devicename,
        manufacturer,
        model
    );
    let mut info = load_web_system_info();
    if let Some(value) = devicename {
        info.device_name = value.as_str().to_owned();
    }
    if let Some(value) = manufacturer {
        info.manufacturer = value.as_str().to_owned();
    }
    if let Some(value) = model {
        info.model = value.as_str().to_owned();
    }
    if !save_web_system_info(&info) {
        return send_json(
            stream,
            200,
            &json_response("updatesysteminfoconfig", "0x01000003"),
        );
    }
    if let Some(name) = addressinfo {
        let name_bytes = name.as_bytes();
        crate::bus::backends::config_modify(|cfg| {
            cfg.name.fill(0);
            cfg.name[..name_bytes.len()].copy_from_slice(name_bytes);
        });
        if let Err(error) = crate::device::apply_config_sync() {
            log::error!("[http] system info persist failed: {error}");
            return send_json(
                stream,
                500,
                &json_response("updatesysteminfoconfig", "0x01000003"),
            );
        }
        let _ = crate::nfc::backup_now();
    }
    send_json(stream, 200, &json_response("updatesysteminfoconfig", "0"))
}

/// GET /getnetworkconfig
fn handle_get_network_config(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let json = match config_read_with(|cs| {
        let cfg = &cs.cfg;
        let ip = format!("{}.{}.{}.{}", cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3]);
        let mask = format!(
            "{}.{}.{}.{}",
            cfg.mask[0], cfg.mask[1], cfg.mask[2], cfg.mask[3]
        );
        let gw = format!(
            "{}.{}.{}.{}",
            cfg.gateway[0], cfg.gateway[1], cfg.gateway[2], cfg.gateway[3]
        );
        let dns = format!(
            "{}.{}.{}.{}",
            cfg.dns[0], cfg.dns[1], cfg.dns[2], cfg.dns[3]
        );
        let mac = format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            cfg.eth_mac[0],
            cfg.eth_mac[1],
            cfg.eth_mac[2],
            cfg.eth_mac[3],
            cfg.eth_mac[4],
            cfg.eth_mac[5],
        );
        let sn = cfg.sn_str();
        let name = cfg.name_str();
        let ble_name = cfg.ble_name_str();
        format!(
            r#"{{"type":"getnetworkconfig","code":"0","msg":"","data":{{"ip":"{}","mask":"{}","gateway":"{}","dns":"{}","mac":"{}","sn":"{}","addressinfo":"{}","dhcp":{},"version":"{}","bloothaddress":"{}"}}}}"#,
            ip,
            mask,
            gw,
            dns,
            mac,
            escape_json(&sn),
            escape_json(&name),
            if cfg.dhcp { 1 } else { 0 },
            MCA_WEB_VERSION,
            escape_json(&ble_name),
        )
    }) {
        Some(j) => j,
        None => return send_json(stream, 500, &json_response("getnetworkconfig", "500")),
    };
    send_json(stream, 200, &json)
}

/// POST /updatenetworkconfig
fn handle_update_network_config(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updatenetworkconfig", "405"));
    }

    // 解析 IP/mask/gw/dns (每字段 "." 分隔 4 个数)
    let parse_ip = |s: &str| -> Option<[u8; 4]> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 4 {
            return None;
        }
        Some([
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
            parts[3].parse().ok()?,
        ])
    };

    let parsed = || -> Option<([u8; 4], [u8; 4], [u8; 4], Option<[u8; 4]>)> {
        let ip = parse_ip(&req.form_field("ip")?)?;
        let mask = parse_ip(&req.form_field("mask")?)?;
        let gw = parse_ip(&req.form_field("gateway")?)?;
        if ip == [0; 4] || ip[0] == 0 || ip[0] >= 224 || mask == [0; 4] {
            return None;
        }
        let mask_u32 = u32::from_be_bytes(mask);
        let inverted = !mask_u32;
        if inverted & inverted.wrapping_add(1) != 0 {
            return None; // netmask 必须是连续的 1 后接连续的 0
        }
        let dns = match req.form_field("dns") {
            Some(value) => Some(parse_ip(&value)?),
            None => None,
        };
        Some((ip, mask, gw, dns))
    };
    let Some((ip, mask, gw, dns)) = parsed() else {
        return send_json(
            stream,
            200,
            &json_response("updatenetworkconfig", "0x01000003"),
        );
    };

    let addressinfo = req.form_field("addressinfo");
    let sn = req.form_field("sn");
    let ble_name = req.form_field("bloothaddress");
    if addressinfo
        .as_ref()
        .is_some_and(|value| value.as_bytes().len() > 16)
        || sn.as_ref().is_some_and(|value| value.as_bytes().len() > 32)
        || ble_name
            .as_ref()
            .is_some_and(|value| value.as_bytes().len() > 8)
    {
        return send_json(
            stream,
            200,
            &json_response("updatenetworkconfig", "0x01000003"),
        );
    }

    let network_changed = config_read_with(|cs| {
        let cfg = &cs.cfg;
        cfg.ip != ip
            || cfg.mask != mask
            || cfg.gateway != gw
            || dns.is_some_and(|value| cfg.dns != value)
            || cfg.dhcp
    })
    .unwrap_or(true);

    crate::bus::backends::config_modify(|cfg| {
        cfg.ip = ip;
        cfg.mask = mask;
        cfg.gateway = gw;
        if let Some(dns) = dns {
            cfg.dns = dns;
        }
        cfg.dhcp = false; // 手动设置后 DHCP 关闭
        if let Some(addressinfo) = addressinfo.as_ref() {
            let bytes = addressinfo.as_bytes();
            cfg.name.fill(0);
            cfg.name[..bytes.len()].copy_from_slice(bytes);
        }
        if let Some(sn) = sn.as_ref() {
            let bytes = sn.as_bytes();
            cfg.sn.fill(0);
            cfg.sn[..bytes.len()].copy_from_slice(bytes);
        }
        if let Some(ble_name) = ble_name.as_ref() {
            let bytes = ble_name.as_bytes();
            cfg.ble_name.fill(0);
            cfg.ble_name[..bytes.len()].copy_from_slice(bytes);
        }
    });

    log::info!(
        "[http] update network: {}.{}.{}.{} / {}.{}.{}.{} gw {}.{}.{}.{}",
        ip[0],
        ip[1],
        ip[2],
        ip[3],
        mask[0],
        mask[1],
        mask[2],
        mask[3],
        gw[0],
        gw[1],
        gw[2],
        gw[3],
    );

    // Web 保存必须在确认 NVS 成功后再返回；网络重配延后到响应发出之后，
    // 避免修改本机 IP 时当前 HTTP 连接先被拆除而让页面误报保存失败。
    if let Err(error) = crate::device::apply_config_sync() {
        log::error!("[http] network config persist failed: {error}");
        return send_json(
            stream,
            500,
            &json_response("updatenetworkconfig", "0x01000003"),
        );
    }
    if ble_name.is_some() {
        crate::ble_at::notify_ble_name_changed();
    }
    let _ = crate::nfc::backup_now();
    let response = send_json(stream, 200, &json_response("updatenetworkconfig", "0"));
    if response.is_ok() && network_changed {
        #[cfg(feature = "ethernet-w5500")]
        crate::ethernet::w5500::request_reconfigure();
    }
    response
}

/// C++ 固件波特率索引表（与 handleGetPortConfig 中 (PRegBuf[reg] >> 12) & 0x0F 完全一致，
/// 0/4=9600, 1=1200, 2=2400, 3=4800, 5=14400, 6=19200, 7=38400, 8=57600, 9=115200,
/// 10=128000, 11=153600, 12=230400, 13=256000, 14=460800, 15=921600)
const C_BAUD_TABLE: [u32; 16] = [
    9600, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200, 128000, 153600, 230400,
    256000, 460800, 921600,
];

/// GET /getportconfig — 对齐 C++ handleGetPortConfig 字段格式
/// C++ 字段: datalen/checkmode/stopbit/baud/masterslaveport/slaveaddress/retrycount/responeinteval/tti
/// C++ baud 是 4 位索引 (0..15)，datalen 是 0=8bit/1=7bit, stopbit 是 0=1bit/1=2bit
fn handle_get_port_config(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let json = match config_read_with(|cs| {
        let cfg = &cs.cfg;
        let port_fields = |i: usize| -> String {
            if i < cfg.rs485.len() {
                let r = &cfg.rs485[i];
                // 把实际波特率反查成 C++ 索引（不匹配时落到 0=9600）
                let baud_idx = C_BAUD_TABLE
                    .iter()
                    .position(|&b| b == r.baudrate)
                    .unwrap_or(0) as u16;
                let datalen = if r.data_bits == 7 { 1 } else { 0 };
                let checkmode = r.parity;
                let stopbit = r.stop_bits.saturating_sub(1);
                format!(
                    r#""datalen_{}":{},"checkmode_{}":{},"stopbit_{}":{},"baud_{}":{},"masterslaveport_{}":{},"slaveaddress_{}":{},"retrycount_{}":{},"responeinteval_{}":{},"tti_{}":{}"#,
                    i,
                    datalen,
                    i,
                    checkmode,
                    i,
                    stopbit,
                    i,
                    baud_idx,
                    i,
                    r.mode,
                    i,
                    r.slave_addr,
                    i,
                    r.retry_count,
                    i,
                    r.timeout_ms,
                    i,
                    r.interval_ms,
                )
            } else {
                format!(
                    r#""datalen_{}":0,"checkmode_{}":0,"stopbit_{}":0,"baud_{}":0,"masterslaveport_{}":0,"slaveaddress_{}":1,"retrycount_{}":3,"responeinteval_{}":1000,"tti_{}":20"#,
                    i, i, i, i, i, i, i, i, i
                )
            }
        };
        format!(
            r#"{{"type":"getportconfig","code":"0","data":{{{},{},{}}}}}"#,
            port_fields(0),
            port_fields(1),
            port_fields(2),
        )
    }) {
        Some(j) => j,
        None => return send_json(stream, 500, &json_response("getportconfig", "500")),
    };
    send_json(stream, 200, &json)
}

/// POST /updateportconfig
fn handle_update_port_config(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updateportconfig", "405"));
    }

    let index: usize = req
        .form_field("index")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // C++ 字段名为 retrycount/responeinteval/tti，新前端 index.html 用 retry/timeout/interval；
    // 同时兼容两套命名 (旧前端优先，C++ 兼容)
    let baud_idx: u32 = req
        .form_field("baud")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let baud: u32 = if (baud_idx as usize) < C_BAUD_TABLE.len() {
        C_BAUD_TABLE[baud_idx as usize]
    } else {
        9600
    };
    let mode: u8 = req
        .form_field("masterslaveport")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let slave_addr: u16 = req
        .form_field("slaveaddress")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let databits: u8 = req
        .form_field("datalen")
        .and_then(|s| s.parse().ok())
        .map(|v: u16| if v == 1 { 7 } else { 8 })
        .unwrap_or(8);
    let stopbits: u8 = req
        .form_field("stopbit")
        .and_then(|s| s.parse().ok())
        .map(|v: u16| if v == 1 { 2 } else { 1 })
        .unwrap_or(1);
    let parity: u8 = req
        .form_field("checkmode")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let retry: u16 = req
        .form_field("retrycount")
        .or_else(|| req.form_field("retry"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let timeout: u16 = req
        .form_field("responeinteval")
        .or_else(|| req.form_field("timeout"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let interval: u16 = req
        .form_field("tti")
        .or_else(|| req.form_field("interval"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    if index >= 3
        || baud_idx as usize >= C_BAUD_TABLE.len()
        || mode > 2
        || !(1..=247).contains(&slave_addr)
        || parity > 2
    {
        return send_json(
            stream,
            200,
            &json_response("updateportconfig", "0x01000003"),
        );
    }
    crate::bus::backends::config_modify(|cfg| {
        cfg.rs485[index].baudrate = baud;
        cfg.rs485[index].mode = mode;
        cfg.rs485[index].slave_addr = slave_addr as u8;
        cfg.rs485[index].data_bits = databits;
        cfg.rs485[index].stop_bits = stopbits;
        cfg.rs485[index].parity = parity;
        cfg.rs485[index].retry_count = retry;
        cfg.rs485[index].timeout_ms = timeout;
        cfg.rs485[index].interval_ms = interval;
    });
    log::info!(
        "[http] update RS485 port {}: baud={} mode={} slave={} retry={} timeout={} interval={}",
        index,
        baud,
        mode,
        slave_addr,
        retry,
        timeout,
        interval
    );

    // 三个端口由 Web 连续提交。端口保存只落盘，不得重配 W5500，否则第一个
    // 请求会中断后两个请求，造成页面提示成功但仅端口 1 实际保存。
    if let Err(error) = crate::device::apply_config_sync() {
        log::error!("[http] RS485 port config persist failed: {error}");
        return send_json(
            stream,
            500,
            &json_response("updateportconfig", "0x01000003"),
        );
    }
    let _ = crate::nfc::backup_now();
    send_json(stream, 200, &json_response("updateportconfig", "0"))
}

fn handle_get_system_status(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let stats = crate::error::recovery::stats();
    let mode = match stats.mode {
        crate::error::recovery::DegradedMode::Normal => "Normal",
        crate::error::recovery::DegradedMode::BleOnly => "BleOnly",
        crate::error::recovery::DegradedMode::LocalOnly => "LocalOnly",
        crate::error::recovery::DegradedMode::Minimal => "Minimal",
    };
    let eth = {
        #[cfg(feature = "ethernet-w5500")]
        {
            crate::ethernet::w5500::link_up()
        }
        #[cfg(not(feature = "ethernet-w5500"))]
        {
            false
        }
    };
    let ble = {
        #[cfg(feature = "ble-at")]
        {
            crate::ble_at::has_client()
        }
        #[cfg(not(feature = "ble-at"))]
        {
            false
        }
    };
    let udp = crate::udp_multicast::is_receiving();
    let json = format!(
        r#"{{"type":"getsystemstatus","code":"0","msg":"","data":{{"uptime":{},"ethernet_link":{},"ble_connected":{},"ble_notify":{},"udp_multicast":{},"udp_received_bytes":{},"rs485_1_comerr":{},"rs485_1_apperr":{},"rs485_2_comerr":{},"rs485_2_apperr":{},"recovery_mode":"{}","recoverable":{},"degradable":{},"severe":{},"free_heap":{},"reset_count":{},"reset_reason":{}}}}}"#,
        IO.sys.get_uptime(),
        eth,
        ble,
        {
            #[cfg(feature = "ble-at")]
            {
                crate::ble_at::notify_enabled()
            }
            #[cfg(not(feature = "ble-at"))]
            {
                false
            }
        },
        udp,
        crate::udp_multicast::received_len(),
        crate::modbus::shared::RS485_STATS.master_comerr(),
        crate::modbus::shared::RS485_STATS.master_apperr(),
        crate::modbus::shared::RS485_STATS.slave_comerr(),
        crate::modbus::shared::RS485_STATS.slave_apperr(),
        mode,
        stats.recoverable,
        stats.degradable,
        stats.severe,
        unsafe { esp_idf_sys::esp_get_free_heap_size() },
        IO.sys.get_reset_count(),
        IO.sys.get_reset_reason(),
    );
    send_json(stream, 200, &json)
}

fn handle_get_nfc_status(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "GET" {
        return send_json(stream, 405, &json_response("getnfcstatus", "405"));
    }
    let json = format!(
        r#"{{"type":"getnfcstatus","code":"0","msg":"","data":{{"started":{},"state":"{}"}}}}"#,
        crate::nfc::is_started(),
        crate::nfc::state().as_str(),
    );
    send_json(stream, 200, &json)
}

fn handle_nfc_backup(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    handle_nfc_command(stream, req, "nfcbackup", crate::nfc::backup_now)
}

fn handle_nfc_restore(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    handle_nfc_command(stream, req, "nfcrestore", crate::nfc::restore_now)
}

fn handle_nfc_command(
    stream: &mut TcpStream,
    req: &HttpRequest,
    response_type: &str,
    command: fn() -> crate::error::AppResult<()>,
) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response(response_type, "405"));
    }
    if !crate::nfc::is_started() {
        return send_json(stream, 503, &json_response(response_type, "0x01000004"));
    }
    match command() {
        Ok(()) => send_json(stream, 200, &json_response(response_type, "0")),
        Err(error) => {
            log::error!("[http] {} request failed: {}", response_type, error);
            send_json(stream, 500, &json_response(response_type, "0x01000003"))
        }
    }
}

/// GET /getiodata
fn handle_get_io_data(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let di_bits = IO.di.load_bits();
    let do_bits = IO.do_.load_bits();
    let ble_connected = {
        #[cfg(feature = "ble-at")]
        {
            crate::ble_at::has_client()
        }
        #[cfg(not(feature = "ble-at"))]
        {
            false
        }
    };
    let ethernet_link = {
        #[cfg(feature = "ethernet-w5500")]
        {
            crate::ethernet::w5500::link_up()
        }
        #[cfg(not(feature = "ethernet-w5500"))]
        {
            false
        }
    };
    // 与原 MCA `states_led[0..7]` 字段保持相同的顺序: PWR/RUN/BT/LAN/485_1/485_2/FIB1/FIB2.
    // 485 状态与原 MCA 一样表示最近一次有效通信脉冲；累计错误只在诊断页显示。
    let state_data = format!(
        r#""state_addr_0":1,"state_addr_1":{},"state_addr_2":{},"state_addr_3":{},"state_addr_4":{},"state_addr_5":{},"state_addr_6":{},"state_addr_7":{}"#,
        (IO.sys.get_uptime() & 1) as u8,
        ble_connected as u8,
        ethernet_link as u8,
        crate::modbus::shared::RS485_STATS.master_active() as u8,
        crate::modbus::shared::RS485_STATS.slave_active() as u8,
        (unsafe {
            esp_idf_sys::gpio_get_level(crate::config::pins::FIB1_PIN as esp_idf_sys::gpio_num_t)
        } == 0) as u8,
        (unsafe {
            esp_idf_sys::gpio_get_level(crate::config::pins::FIB2_PIN as esp_idf_sys::gpio_num_t)
        } == 0) as u8,
    );

    // DI/DO 状态数组 (对齐参考固件: di_addr_0..15, do_addr_0..15)
    let mut di_data = String::new();
    let mut do_data = String::new();
    let di_count = crate::config::hw_version::DI_COUNT as u32;
    let do_count = crate::config::hw_version::DO_COUNT as u32;

    for i in 0..di_count {
        let bit = (di_bits >> i) & 1 != 0;
        if i > 0 {
            di_data.push(',');
        }
        di_data.push_str(&format!("\"di_addr_{}\":{}", i, if bit { 1 } else { 0 }));
    }
    for i in 0..do_count {
        let bit = (do_bits >> i) & 1 != 0;
        if i > 0 {
            do_data.push(',');
        }
        do_data.push_str(&format!("\"do_addr_{}\":{}", i, if bit { 1 } else { 0 }));
    }

    // AI 值
    let ai_count = crate::config::hw_version::AI_COUNT as u16;
    let mut ai_data = String::new();
    let mut ai_max = String::new();
    let mut ai_min = String::new();
    for i in 0..ai_count {
        if i > 0 {
            ai_data.push(',');
            ai_max.push(',');
            ai_min.push(',');
        }
        let val = crate::bus::backends::read_input_reg(regs::INREG_AI_BASE + i).unwrap_or(0);
        ai_data.push_str(&format!("\"ai_addr_{}\":{}", i, val));
        // C++ 在 getiodata 同时返回 ai_data_max / ai_data_min (校准值), 来自 holding_buf 2288+/2280+
        let max_v =
            crate::bus::backends::read_hold_reg(regs::HOLD_SENSOR_MAX_BASE + i).unwrap_or(0);
        let min_v =
            crate::bus::backends::read_hold_reg(regs::HOLD_SENSOR_MIN_BASE + i).unwrap_or(0);
        ai_max.push_str(&format!("\"ai_addr_max_{}\":{}", i, max_v));
        ai_min.push_str(&format!("\"ai_addr_min_{}\":{}", i, min_v));
    }

    let json = format!(
        r#"{{"type":"getiodata","code":"0","msg":"","data":{{"state_data":{{{}}},"di_data":{{{}}},"do_data":{{{}}},"ai_data":{{{}}},"ai_data_max":{{{}}},"ai_data_min":{{{}}}}}}}"#,
        state_data, di_data, do_data, ai_data, ai_max, ai_min,
    );
    send_json(stream, 200, &json)
}

/// POST /iocontrol
fn handle_io_control(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("iocontrol", "405"));
    }

    let addr: u16 = req
        .form_field("addr")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let value: bool = req
        .form_field("value")
        .and_then(|s| s.parse::<u8>().ok())
        .map(|v| v != 0)
        .unwrap_or(false);

    // LOOP13: 校验 addr 范围, 越界返回错误码 (0x01000003 = addr 超限)
    if addr >= crate::config::hw_version::DO_COUNT as u16 {
        return send_json(stream, 200, &json_response("iocontrol", "0x01000003"));
    }

    // 通过 Modbus coil 写入接口控制 DO (addr 为 DO 编号 0..N)
    // LOOP13: write_coil 内部已内化 notify(), 不需要在此显式调用
    let coil_addr = regs::COIL_DO_BASE + addr;
    let _ = crate::bus::backends::write_coil(coil_addr, value);

    log::info!("[http] IO control: addr={} value={}", addr, value);
    send_json(stream, 200, &json_response("iocontrol", "0"))
}

/// GET /getbleconfig — BLE 名称 + MAC (现场识别用)
fn handle_get_ble_config(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let json = match config_read_with(|cs| {
        let cfg = &cs.cfg;
        let name_bytes: Vec<u8> = cfg
            .ble_name
            .iter()
            .take_while(|&&b| b != 0)
            .copied()
            .collect();
        let name = String::from_utf8_lossy(&name_bytes);
        format!(
            r#"{{"type":"getbleconfig","code":"0","data":{{"blename":"{}","mac":"{}"}}}}"#,
            escape_json(&name),
            escape_json(&format!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                cfg.ble_mac[0],
                cfg.ble_mac[1],
                cfg.ble_mac[2],
                cfg.ble_mac[3],
                cfg.ble_mac[4],
                cfg.ble_mac[5]
            )),
        )
    }) {
        Some(j) => j,
        None => return send_json(stream, 500, &json_response("getbleconfig", "500")),
    };
    send_json(stream, 200, &json)
}

/// POST /updatebleconfig — 更新 BLE 名称 (≤8 字节 ASCII)
fn handle_update_ble_config(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updatebleconfig", "405"));
    }
    let name = req.form_field("blename").unwrap_or_default();
    let name_bytes = name.as_bytes();
    if name_bytes.len() > 8 {
        return send_json(stream, 200, &json_response("updatebleconfig", "0x01000004"));
    }
    crate::bus::backends::config_modify(|cfg| {
        for b in &mut cfg.ble_name {
            *b = 0;
        }
        let take = name_bytes.len().min(8);
        cfg.ble_name[..take].copy_from_slice(&name_bytes[..take]);
    });
    log::info!("[http] update BLE name: {:?}", name);
    if let Err(error) = crate::device::apply_config_sync() {
        log::error!("[http] BLE config persist failed: {error}");
        return send_json(
            stream,
            500,
            &json_response("updatebleconfig", "0x01000003"),
        );
    }
    // 只有 NVS 已确认保存后才更新 GAP 名，避免持久化失败时运行值与重启值不一致。
    crate::ble_at::notify_ble_name_changed();
    send_json(stream, 200, &json_response("updatebleconfig", "0"))
}

/// GET /getsensorconfig — AI 传感器零点/满度 (2280-2295, 8 通道×2)
fn handle_get_sensor_config(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let ai_count = crate::config::hw_version::AI_COUNT as usize;
    let json = match storage_read_with(|s| {
        let base_min = regs::HOLD_SENSOR_MIN_BASE as usize;
        let base_max = regs::HOLD_SENSOR_MAX_BASE as usize;
        let cfg_base = regs::HOLD_CFG_BASE as usize;
        let mut sensor_data = String::new();
        for i in 0..ai_count {
            if i > 0 {
                sensor_data.push(',');
            }
            let min_v = s
                .holding_buf
                .get(base_min - cfg_base + i)
                .copied()
                .unwrap_or(605);
            let max_v = s
                .holding_buf
                .get(base_max - cfg_base + i)
                .copied()
                .unwrap_or(3016);
            sensor_data.push_str(&format!(
                r#""ai_{}":{{"min":{},"max":{}}}"#,
                i, min_v, max_v
            ));
        }
        format!(
            r#"{{"type":"getsensorconfig","code":"0","data":{{{}}}}}"#,
            sensor_data
        )
    }) {
        Some(j) => j,
        None => return send_json(stream, 500, &json_response("getsensorconfig", "500")),
    };
    send_json(stream, 200, &json)
}

/// POST /updatesensorconfig — 写入 AI 零点/满度 (单字 FC=06 风格)
fn handle_update_sensor_config(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updatesensorconfig", "405"));
    }
    let ch: usize = req
        .form_field("ch")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let min_v: u16 = req
        .form_field("min")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let max_v: u16 = req
        .form_field("max")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let ai_count = crate::config::hw_version::AI_COUNT as usize;
    if ch >= ai_count {
        return send_json(
            stream,
            200,
            &json_response("updatesensorconfig", "0x01000003"),
        );
    }
    let base_min = regs::HOLD_SENSOR_MIN_BASE as usize;
    let base_max = regs::HOLD_SENSOR_MAX_BASE as usize;
    let cfg_base = regs::HOLD_CFG_BASE as usize;
    let base_min_idx = base_min - cfg_base + ch;
    let base_max_idx = base_max - cfg_base + ch;
    crate::bus::backends::storage_modify_holding(|holding| {
        if base_min_idx < holding.len() {
            holding[base_min_idx] = min_v;
        }
        if base_max_idx < holding.len() {
            holding[base_max_idx] = max_v;
        }
    });
    crate::device::request_persist_holding();
    log::info!("[http] update sensor AI{} min={} max={}", ch, min_v, max_v);
    send_json(stream, 200, &json_response("updatesensorconfig", "0"))
}

/// POST /reboot — Web UI 触发设备重启 (Apply 配置后需要)
fn handle_reboot(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("reboot", "405"));
    }
    send_json(stream, 200, &json_response("reboot", "0"))?;
    crate::bus::IO.sys.request_reset();
    Ok(())
}

/// POST /updatepwd
fn handle_update_password(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updatepwd", "405"));
    }

    let old_pwd = req.form_field("opwd").unwrap_or_default();
    let new_pwd = req.form_field("npwd").unwrap_or_default();

    let actual_pwd = load_web_password();

    if old_pwd.as_str() != actual_pwd {
        return send_json(stream, 200, &json_response("updatepwd", "0x00010001"));
    }

    // 保存新密码到 NVS
    if let Some(Ok(())) = crate::device::try_with_nvs_mut(|nvs| -> Result<(), String> {
        nvs.set_str(NVS_KEY_WEB_PWD, &new_pwd)
            .map_err(|e| format!("{:?}", e))?;
        nvs.set_u8(NVS_KEY_WEB_PWD_FLAG, 0x66)
            .map_err(|e| format!("{:?}", e))?;
        Ok(())
    }) {
        log::info!("[http] web password updated");
    } else {
        log::warn!("[http] password save failed (NVS unavailable)");
        return send_json(stream, 500, &json_response("updatepwd", "0x01000003"));
    }

    send_json(stream, 200, &json_response("updatepwd", "0"))
}

/// POST /updateota (OTA 固件上传 — 流式, 不缓存整 body)
///
/// LOOP9 重构: 旧实现 `vec![0u8; content_length]` 会立即 OOM (ESP32-S3 仅 ~200KB
/// 可用 internal heap, 固件可达 2MB)。新实现按 BUF_SIZE(4KB) chunk 流式读取,
/// 边读边写入 OTA 分区, 内存占用恒定 ~4KB。
///
/// 同时放宽 TCP read/write 超时 (固件上传耗时较长, 3s 太短)。
fn handle_ota_upload_stream(
    mut reader: BufReader<TcpStream>,
    content_length: usize,
) -> std::io::Result<()> {
    if content_length == 0 {
        return send_json(reader.get_mut(), 400, &json_response("updateota", "400"));
    }
    if content_length > MAX_OTA_SIZE {
        log::warn!(
            "[http] OTA rejected: content_length={} > MAX_OTA_SIZE={}",
            content_length,
            MAX_OTA_SIZE
        );
        return send_json(reader.get_mut(), 413, &json_response("updateota", "413"));
    }

    // 放宽超时: 固件上传是慢操作, 3s 全局超时易误触发
    // socket 单次阻塞必须短于 10s Task WDT；总的“无进度”窗口仍允许 60s。
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .ok();
    reader
        .get_ref()
        .set_write_timeout(Some(Duration::from_secs(60)))
        .ok();

    let total = content_length as u32;
    log::info!("[http] OTA upload (streaming): {} bytes", total);

    // 分块写入 OTA 分区
    match crate::ota::begin(total) {
        Ok(()) => {}
        Err(e) => {
            log::error!("[http] OTA begin failed: {}", e);
            // LOOP9: begin 失败时也尝试 abort (abort 内部对空会话是幂等 no-op), 防状态泄漏
            let _ = crate::ota::abort();
            return send_json(reader.get_mut(), 500, &json_response("updateota", "500"));
        }
    }

    // LOOP18: OTA 上传单帧最大 4096B 过大, 在 8KB http-srv 栈上占用 50% 预算.
    // 缩小到 2048B (仍是合理块大小, OTA::write_chunk 处理任意长度), 给 stack
    // 溢出风险预留余量. BufReader 内部仍有 8KB heap 缓冲, 不影响吞吐.
    let mut buf = vec![0u8; 2048].into_boxed_slice();
    let mut received = 0usize;
    let mut last_progress = std::time::Instant::now();
    while received < content_length {
        let want = (content_length - received).min(buf.len());
        let count = match reader.read(&mut buf[..want]) {
            Ok(0) => {
                log::error!("[http] OTA peer closed at {}/{}", received, content_length);
                let _ = crate::ota::abort();
                let mut s = reader.into_inner();
                return send_json(&mut s, 400, &json_response("updateota", "400"));
            }
            Ok(n) => {
                last_progress = std::time::Instant::now();
                n
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                TASK_HB.tick();
                health::feed_wdt();
                if last_progress.elapsed() < Duration::from_secs(60) {
                    continue;
                }
                log::error!("[http] OTA idle timeout at {}/{}", received, content_length);
                let _ = crate::ota::abort();
                let mut s = reader.into_inner();
                return send_json(&mut s, 500, &json_response("updateota", "500"));
            }
            Err(e) => {
                log::error!("[http] OTA read failed at {}: {}", received, e);
                let _ = crate::ota::abort();
                let mut s = reader.into_inner();
                return send_json(&mut s, 500, &json_response("updateota", "500"));
            }
        };
        match crate::ota::write_chunk(&buf[..count]) {
            Ok(n) => received += n,
            Err(e) => {
                log::error!("[http] OTA write failed at {}: {}", received, e);
                let _ = crate::ota::abort();
                let mut s = reader.into_inner();
                return send_json(&mut s, 500, &json_response("updateota", "500"));
            }
        }
        TASK_HB.tick();
        health::feed_wdt();
    }

    match crate::ota::end() {
        Ok(()) => {
            log::info!("[http] OTA complete, rebooting in 1s");
            let mut s = reader.into_inner();
            send_json(&mut s, 200, &json_response("updateota", "0"))?;
            std::thread::sleep(Duration::from_secs(1));
            crate::ota::reboot_to_new_firmware();
        }
        Err(e) => {
            log::error!("[http] OTA end failed: {}", e);
            let _ = crate::ota::abort();
            let mut s = reader.into_inner();
            send_json(&mut s, 500, &json_response("updateota", "500"))
        }
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 从 NVS 加载 Web 密码
fn load_web_password() -> String {
    match crate::device::try_with_nvs(|nvs| -> Option<String> {
        let flag = nvs.get_u8(NVS_KEY_WEB_PWD_FLAG).ok().flatten()?;
        if flag == 0x66 {
            let mut buf = [0u8; 64];
            nvs.get_str(NVS_KEY_WEB_PWD, &mut buf)
                .ok()
                .flatten()
                .map(|s| s.to_string())
        } else {
            None
        }
    }) {
        Some(Some(pwd)) => pwd,
        _ => DEFAULT_PASSWORD.to_string(),
    }
}

/// JSON 字符串转义 (处理 " 和 \ 和控制字符)
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 10);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

// ============================================================================
// 测试
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_response_format() {
        let r = json_response("login", "0");
        assert_eq!(r, r#"{"type":"login","code":"0","msg":"","data":{}}"#);
    }

    #[test]
    fn test_escape_json() {
        assert_eq!(escape_json("hello"), "hello");
        assert_eq!(escape_json("a\"b"), "a\\\"b");
        assert_eq!(escape_json("a\nb"), "a\\nb");
        assert_eq!(escape_json("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(
            url_decode_into::<32>("hello%20world").unwrap(),
            "hello world"
        );
        assert_eq!(url_decode_into::<32>("a+b").unwrap(), "a b");
        assert_eq!(url_decode_into::<32>("abc").unwrap(), "abc");
        assert_eq!(url_decode_into::<32>("%41%42%43").unwrap(), "ABC");
        assert_eq!(url_decode_into::<32>("%E8%BF%88%E9%94%90").unwrap(), "迈锐");
        assert!(url_decode_into::<3>("abcd").is_none());
        assert!(url_decode_into::<32>("%FF").is_none());
    }

    #[test]
    fn test_web_system_info_round_trip() {
        let info = WebSystemInfo {
            device_name: "工业测控执行器".to_string(),
            manufacturer: "迈锐".to_string(),
            model: "MR-MCA-200".to_string(),
        };
        let encoded = encode_web_system_info(&info).expect("valid system info must encode");
        assert_eq!(encoded.len(), WEB_SYSTEM_INFO_BLOB_LEN);
        assert_eq!(decode_web_system_info(&encoded), Some(info));
    }

    #[test]
    fn test_web_system_info_rejects_oversized_or_corrupt_data() {
        let oversized = WebSystemInfo {
            device_name: "x".repeat(WEB_SYSTEM_INFO_FIELD_LEN + 1),
            manufacturer: String::new(),
            model: String::new(),
        };
        assert!(encode_web_system_info(&oversized).is_none());

        let mut corrupt = [0u8; WEB_SYSTEM_INFO_BLOB_LEN];
        corrupt[0] = (WEB_SYSTEM_INFO_FIELD_LEN + 1) as u8;
        assert!(decode_web_system_info(&corrupt).is_none());
    }

    #[test]
    fn test_mac_web_navigation_names_are_present() {
        for label in [
            "系统信息",
            "设备基本信息",
            "端口设置",
            "I/O口信息",
            "用户设置",
            "系统维护",
        ] {
            assert!(
                pages::INDEX_HTML.contains(label),
                "missing navigation: {label}"
            );
        }
        assert!(pages::INDEX_HTML.contains("工业测控执行器管理系统"));
        assert!(pages::LOGIN_HTML.contains("工业控制器登录界面"));
    }

    #[test]
    fn test_nfc_maintenance_controls_are_present() {
        for endpoint in ["/getnfcstatus", "/nfcbackup", "/nfcrestore"] {
            assert!(
                pages::INDEX_HTML.contains(endpoint),
                "missing endpoint: {endpoint}"
            );
        }
        assert!(pages::INDEX_HTML.contains("confirm("));
    }

    #[test]
    fn test_io_page_uses_numeric_order_and_one_based_labels() {
        assert!(pages::INDEX_HTML.contains("Number(a.slice('di_addr_'.length))"));
        assert!(pages::INDEX_HTML.contains("Number(a.slice('do_addr_'.length))"));
        assert!(pages::INDEX_HTML.contains("<span>DI${n + 1}</span>"));
        assert!(pages::INDEX_HTML.contains("<span>DO${n + 1}</span>"));
        assert!(
            pages::INDEX_HTML.contains("onchange=\"toggleDO(${n},this.checked)\""),
            "display must be one-based without changing the zero-based control address"
        );
    }

    // ---- LOOP11: 会话管理回归测试 ----

    /// hex 编码 16 字节 → 32 字符, 与已知向量对照
    #[test]
    fn test_hex_encode_16() {
        let token = [
            0x01, 0x02, 0x0a, 0x0f, 0x10, 0xff, 0xab, 0xcd, 0x00, 0x99, 0x88, 0x77, 0x66, 0x55,
            0x44, 0x33,
        ];
        let hex = hex_encode_16(&token);
        let s = std::str::from_utf8(&hex).unwrap();
        assert_eq!(s, "01020a0f10ffabcd0099887766554433");
    }

    /// 登录 → 已认证 → logout → 未认证 完整流程
    #[test]
    fn test_session_auth_flow() {
        // 初始: 无活跃会话
        destroy_session();
        assert!(!validate_session_cookie(
            b"00000000000000000000000000000000"
        ));

        // 登录: 创建会话, Cookie 含正确 token hex
        let cookie = create_session_cookie();
        assert!(cookie.starts_with("ESPSESSIONID="));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Max-Age=86400"));
        // 提取 token hex (ESPSESSIONID= 后到第一个 ; 之前)
        let token_hex = cookie
            .strip_prefix("ESPSESSIONID=")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .as_bytes();
        assert_eq!(token_hex.len(), 32);
        // 用该 token 验证 → 应通过
        assert!(validate_session_cookie(token_hex));

        // logout 后 → 同一 token 应失效
        destroy_session();
        assert!(!validate_session_cookie(token_hex));
    }

    /// 错误 token / 错误长度 → 拒绝 (防伪造)
    #[test]
    fn test_session_reject_invalid_token() {
        destroy_session();
        let cookie = create_session_cookie();
        let valid = cookie
            .strip_prefix("ESPSESSIONID=")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .as_bytes();
        // 旧硬编码值 "1" 应被拒绝
        assert!(!validate_session_cookie(b"1"));
        // 长度不足应被拒绝
        assert!(!validate_session_cookie(b"abc"));
        // 翻转一个字节的 token 应被拒绝
        let mut tampered = valid.to_vec();
        tampered[0] ^= 0x01;
        assert!(!validate_session_cookie(&tampered));
        // 正确 token 应通过
        assert!(validate_session_cookie(valid));
    }

    /// 常量时间比较: 长度相同但内容不同 → false, 不 panic
    #[test]
    fn test_session_constant_time_compare() {
        destroy_session();
        let _ = create_session_cookie();
        let wrong = b"deadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(wrong.len(), 32);
        assert!(!validate_session_cookie(wrong));
    }

    #[test]
    fn test_session_server_side_expiry() {
        destroy_session();
        let cookie = create_session_cookie();
        let token = cookie
            .strip_prefix("ESPSESSIONID=")
            .and_then(|value| value.split(';').next())
            .expect("session token");
        if let Ok(mut session) = SESSION.lock() {
            session.created_at = Some(
                std::time::Instant::now() - std::time::Duration::from_secs(SESSION_TTL_SECS + 1),
            );
        }
        assert!(!validate_session_cookie(token.as_bytes()));
    }
}
