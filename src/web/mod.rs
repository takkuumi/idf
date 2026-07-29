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
//! - 非阻塞 accept, 每 100ms 轮询一次 (主线程 poll 模式)

mod pages;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use crate::bus::storage_state::storage_read_with;
use crate::bus::{config_state::config_read_with, IO};
use crate::config::regs;
use crate::error::AppResult;
use crate::health::{self, TaskHb};

/// HTTP 端口 (对齐参考固件: server(80))
const HTTP_PORT: u16 = 80;
/// 收发缓冲区大小 (流式 OTA/header 读取的单块大小)
const BUF_SIZE: usize = 4096;
/// 单条 HTTP header 行最大字节数 (防止恶意超长 header → OOM)
const MAX_HEADER_LINE: usize = 1024;
/// POST body 总量上限 (对齐参考固件最大包, 防 Content-Length 伪造 → OOM)
const MAX_BODY_SIZE: usize = 512 * 1024; // 512KB 足够配置/JSON; OTA 走流式 (见下方)
/// 流式 OTA body 总量上限 (与 flash 分区大小一致: ota 分区 2.25MB)
const MAX_OTA_SIZE: usize = 2 * 1024 * 1024;
/// 默认用户名
const DEFAULT_USERNAME: &str = "admin";
/// 默认密码 (NVS 未保存时使用)
const DEFAULT_PASSWORD: &str = "admin123";

/// NVS 密码 key
const NVS_KEY_WEB_PWD: &str = "web_pwd";
const NVS_KEY_WEB_PWD_FLAG: &str = "web_pwd_f";

/// 会话有效期 (秒): 24 小时 (与 Cookie Max-Age 一致)
const SESSION_TTL_SECS: u64 = 86400;

// ============================================================================
// LOOP11: 会话管理 (替代硬编码 ESPSESSIONID=1)
// ============================================================================

/// 会话状态 (单会话设备: 同一时间仅允许一个管理员登录)
/// - `token`: 16 字节硬件随机数, hex 编码后作为 Cookie 值
/// - `active`: 当前会话是否有效 (logout 时清零)
struct Session {
    token: [u8; 16],
    active: bool,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    token: [0u8; 16],
    active: false,
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
    }
    // Set-Cookie: ESPSESSIONID=<32hex>; HttpOnly; Path=/; Max-Age=86400
    format!(
        "ESPSESSIONID={}; HttpOnly; Path=/; Max-Age={}",
        std::str::from_utf8(&hex).unwrap_or(""),
        SESSION_TTL_SECS
    )
}

/// 销毁服务端会话 (logout 调用)
fn destroy_session() {
    if let Ok(mut s) = SESSION.lock() {
        s.token = [0u8; 16];
        s.active = false;
    }
}

/// 验证 Cookie 中的 token 是否与会话匹配 (常量时间比较, 防时序攻击)
fn validate_session_cookie(token_hex: &[u8]) -> bool {
    if token_hex.len() != 32 {
        return false;
    }
    if let Ok(s) = SESSION.lock() {
        if !s.active {
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

/// 启动 HTTP Web 服务器 (后台线程)
pub fn start() -> AppResult<()> {
    std::thread::Builder::new()
        .name("http-srv".into())
        // LOOP18: http-srv 栈从 8KB 提到 12KB.
        //  - OTA 上传期间单帧 buf 已缩到 2048B (见 handle_ota_upload_stream)
        //  - 但 handle_get_io_data + handle_get_system_status 等长路径
        //    单次请求会构造 ~30+ format! 临时字符串, 加上 BufReader 内部状态
        //    + LwIP socket 状态, 实测峰值接近 7KB. 8KB 边界易触发 Stack canary.
        //  - 与 CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=12288 对齐, 移除
        //    BLE/ETH/Modbus 同时启动时的栈压力来源.
        .stack_size(crate::safety::stack_budget::HTTP)
        .spawn(server_loop)
        .map_err(|e| crate::error::AppError::Sys(format!("spawn http: {e}")))?;
    crate::health::register_with_stack(&TASK_HB, crate::safety::stack_budget::HTTP);
    log::info!("[http] web server started on port {}", HTTP_PORT);
    Ok(())
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
        listener
            .set_nonblocking(true)
            .ok();
        log::info!("[http] listening on 0.0.0.0:{}", HTTP_PORT);

        loop {
            TASK_HB.tick();
            health::feed_wdt();

            match listener.accept() {
                Ok((stream, _addr)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .ok();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .ok();
                    if let Err(e) = handle_connection(stream) {
                        log::debug!("[http] connection error: {}", e);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
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
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    /// 获取指定 header 值
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
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
    fn form_field(&self, name: &str) -> Option<String> {
        let body = std::str::from_utf8(&self.body).ok()?;
        for pair in body.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let val = parts.next().unwrap_or("");
            if key == name {
                return Some(url_decode(val));
            }
        }
        None
    }
}

/// URL 解码 (处理 %XX 和 +)
fn url_decode(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(
                    core::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00"),
                    16,
                ) {
                    result.push(b);
                    i += 3;
                } else {
                    result.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                result.push(b' ');
                i += 1;
            }
            _ => {
                result.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&result).into_owned()
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
        301 => "Moved Permanently",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nSet-Cookie: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
fn json_response(response_type: &str, code: &str) -> String {
    format!(
        r#"{{"type":"{}","code":"{}","msg":"","data":{{}}}}"#,
        response_type, code
    )
}

/// 解析请求行 + headers (不含 body)
///
/// LOOP9 安全加固:
/// - header 行长度上限 MAX_HEADER_LINE (防恶意超长 header → OOM)
/// 返回 (HttpRequest 框架, content_length, reader) — body 由调用方按需读取,
/// OTA 走流式 (handle_ota_upload_stream), 其余路由走 read_exact 上限 MAX_BODY_SIZE.
fn parse_request_headers(stream: TcpStream) -> std::io::Result<(HttpRequest, usize, BufReader<TcpStream>)> {
    let mut reader = BufReader::new(stream);
    // 读请求行 (限制长度)
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if request_line.len() > MAX_HEADER_LINE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request line too long",
        ));
    }
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    let method = parts.first().unwrap_or(&"").to_string();
    let path_raw = parts.get(1).unwrap_or(&"/");
    let path = url_decode(path_raw);

    // 读 Headers (每行限制长度)
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.len() > MAX_HEADER_LINE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header line too long",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);

    Ok((
        HttpRequest {
            method,
            path,
            headers,
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
        return handle_ota_upload_stream(reader.into_inner(), content_length);
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
                send_response(&mut stream, 200, "text/html; charset=utf-8", pages::INDEX_HTML.as_bytes())
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
        return send_response(stream, 200, "text/html; charset=utf-8", pages::LOGIN_HTML.as_bytes());
    }
    let username = req.form_field("username").unwrap_or_default();
    let pwd = req.form_field("pwd").unwrap_or_default();

    let actual_pwd = load_web_password();

    if username != DEFAULT_USERNAME {
        return send_json(stream, 200, &json_response("login", "0x01000001"));
    }
    if pwd != actual_pwd {
        return send_json(stream, 200, &json_response("login", "0x01000002"));
    }
    // LOOP11: 返回 200 JSON + 随机 token Cookie (不再 301 空 body, 否则
    // login.html 的 fetch().then(r=>r.json()) 解析失败导致登录看似无反应)
    let cookie = create_session_cookie();
    send_json_with_cookie(stream, 200, &json_response("login", "0"), &cookie)
}

/// GET /getsysteminfo — 对齐 C++ handleGetSystemInfo: devicename/manufacturer/model/version
fn handle_get_system_info(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let json = match config_read_with(|cs| {
        let cfg = &cs.cfg;
        let device_name = cfg.name_str();
        let fw_ver = cfg.fw_version;
        let hw_ver = cfg.hw_version;
        let fw_date = cfg.fw_date;
        format!(
            r#"{{"type":"getsysteminfo","code":"0","msg":"","data":{{"devicename":"{}","manufacturer":"","model":"F16","version":"V{}.{}.{}.{}","hw_version":"0x{:04X}","addressinfo":"{}"}}}}"#,
            escape_json(&device_name),
            fw_ver / 100,
            (fw_ver % 100) / 10,
            fw_ver % 10,
            fw_date,
            hw_ver,
            escape_json(&device_name),
        )
    }) {
        Some(j) => j,
        None => return send_json(stream, 500, &json_response("getsysteminfo", "500")),
    };
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

    // C++ WebServer 要求 devicename/manufacturer/model 三字段；Rust 当前
    // SystemConfig 没有独立 manufacturer/model 存储槽，仍接受并保留协议兼容，
    // devicename/addressinfo 映射到原有 name 字段.
    if devicename.is_none() && addressinfo.is_none() {
        return send_json(stream, 200, &json_response("updatesysteminfoconfig", "0x01000001"));
    }
    log::info!(
        "[http] update system info: devicename={:?}, manufacturer={:?}, model={:?}",
        devicename, manufacturer, model
    );
    let name = addressinfo.or(devicename).unwrap_or_default();
    let name_bytes: Vec<u8> = name.as_bytes().to_vec();
    crate::bus::backends::config_modify(|cfg| {
        let take = name_bytes.len().min(cfg.name.len());
        cfg.name[..take].copy_from_slice(&name_bytes[..take]);
        for b in &mut cfg.name[take..] {
            *b = 0;
        }
    });
    send_json(stream, 200, &json_response("updatesysteminfoconfig", "0"))
}

/// GET /getnetworkconfig
fn handle_get_network_config(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let json = match config_read_with(|cs| {
        let cfg = &cs.cfg;
        let ip = format!("{}.{}.{}.{}", cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3]);
        let mask = format!("{}.{}.{}.{}", cfg.mask[0], cfg.mask[1], cfg.mask[2], cfg.mask[3]);
        let gw = format!("{}.{}.{}.{}", cfg.gateway[0], cfg.gateway[1], cfg.gateway[2], cfg.gateway[3]);
        let dns = format!("{}.{}.{}.{}", cfg.dns[0], cfg.dns[1], cfg.dns[2], cfg.dns[3]);
        let mac = format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            cfg.eth_mac[0], cfg.eth_mac[1], cfg.eth_mac[2],
            cfg.eth_mac[3], cfg.eth_mac[4], cfg.eth_mac[5],
        );
        let sn = cfg.sn_str();
        let name = cfg.name_str();
        format!(
            r#"{{"type":"getnetworkconfig","code":"0","msg":"","data":{{"ip":"{}","mask":"{}","gateway":"{}","dns":"{}","mac":"{}","sn":"{}","addressinfo":"{}","dhcp":{},"version":"F16","bloothaddress":""}}}}"#,
            ip, mask, gw, dns, mac, escape_json(&sn), escape_json(&name), if cfg.dhcp { 1 } else { 0 },
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

    let ip = req
        .form_field("ip")
        .and_then(|s| parse_ip(&s))
        .unwrap_or([0; 4]);
    let mask = req
        .form_field("mask")
        .and_then(|s| parse_ip(&s))
        .unwrap_or([0; 4]);
    let gw = req
        .form_field("gateway")
        .and_then(|s| parse_ip(&s))
        .unwrap_or([0; 4]);
    let dns = req
        .form_field("dns")
        .and_then(|s| parse_ip(&s))
        .unwrap_or([0; 4]);

    crate::bus::backends::config_modify(|cfg| {
        cfg.ip = ip;
        cfg.mask = mask;
        cfg.gateway = gw;
        cfg.dns = dns;
        cfg.dhcp = false; // 手动设置后 DHCP 关闭
    });

    // 更新 addressinfo (位置/桩号)
    if let Some(addr) = req.form_field("addressinfo") {
        let bytes = addr.as_bytes();
        crate::bus::backends::config_modify(|cfg| {
            let take = bytes.len().min(cfg.name.len());
            cfg.name[..take].copy_from_slice(&bytes[..take]);
            for b in &mut cfg.name[take..] {
                *b = 0;
            }
        });
    }

    log::info!(
        "[http] update network: {}.{}.{}.{} / {}.{}.{}.{} gw {}.{}.{}.{}",
        ip[0], ip[1], ip[2], ip[3],
        mask[0], mask[1], mask[2], mask[3],
        gw[0], gw[1], gw[2], gw[3],
    );

    // 持久化 + NFC 备份
    crate::device::request_save_device_text();
    let _ = crate::nfc::backup_now();

    send_json(stream, 200, &json_response("updatenetworkconfig", "0"))
}

/// C++ 固件波特率索引表（与 handleGetPortConfig 中 (PRegBuf[reg] >> 12) & 0x0F 完全一致，
/// 0/4=9600, 1=1200, 2=2400, 3=4800, 5=14400, 6=19200, 7=38400, 8=57600, 9=115200,
/// 10=128000, 11=153600, 12=230400, 13=256000, 14=460800, 15=921600)
const C_BAUD_TABLE: [u32; 16] = [
    9600, 1200, 2400, 4800, 9600, 14400, 19200, 38400,
    57600, 115200, 128000, 153600, 230400, 256000, 460800, 921600,
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
                let baud_idx = C_BAUD_TABLE.iter().position(|&b| b == r.baudrate).unwrap_or(0) as u16;
                let datalen = if r.data_bits == 7 { 1 } else { 0 };
                let checkmode = r.parity;
                let stopbit = r.stop_bits.saturating_sub(1);
                format!(
                    r#""datalen_{}":{},"checkmode_{}":{},"stopbit_{}":{},"baud_{}":{},"masterslaveport_{}":{},"slaveaddress_{}":{},"retrycount_{}":{},"responeinteval_{}":{},"tti_{}":{}"#,
                    i, datalen, i, checkmode, i, stopbit, i, baud_idx, i, r.mode, i, r.slave_addr, i, r.retry_count, i, r.timeout_ms, i, r.interval_ms,
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

    if index < 3 {
        crate::bus::backends::config_modify(|cfg| {
            if index < cfg.rs485.len() {
                cfg.rs485[index].baudrate = baud;
                cfg.rs485[index].mode = mode;
                cfg.rs485[index].slave_addr = slave_addr as u8;
                cfg.rs485[index].data_bits = databits;
                cfg.rs485[index].stop_bits = stopbits;
                cfg.rs485[index].parity = parity;
                cfg.rs485[index].retry_count = retry;
                cfg.rs485[index].timeout_ms = timeout;
                cfg.rs485[index].interval_ms = interval;
            }
        });
        log::info!(
            "[http] update RS485 port {}: baud={} mode={} slave={} retry={} timeout={} interval={}",
            index, baud, mode, slave_addr, retry, timeout, interval
        );
    }

    // 配置类写入应通过 SystemConfig 持久化 (走 ApplyConfig), 而非 device_text (那只是文本快照).
    crate::device::request_apply_config();
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
        { crate::ethernet::w5500::link_up() }
        #[cfg(not(feature = "ethernet-w5500"))]
        { false }
    };
    let ble = {
        #[cfg(feature = "ble-at")]
        { crate::ble_at::has_client() }
        #[cfg(not(feature = "ble-at"))]
        { false }
    };
    let udp = crate::udp_multicast::is_receiving();
    let json = format!(
        r#"{{"type":"getsystemstatus","code":"0","msg":"","data":{{"uptime":{},"ethernet_link":{},"ble_connected":{},"ble_notify":{},"udp_multicast":{},"udp_received_bytes":{},"rs485_1_comerr":{},"rs485_1_apperr":{},"rs485_2_comerr":{},"rs485_2_apperr":{},"recovery_mode":"{}","recoverable":{},"degradable":{},"severe":{},"free_heap":{},"reset_count":{},"reset_reason":{}}}}}"#,
        IO.sys.get_uptime(), eth, ble,
        { #[cfg(feature = "ble-at")] { crate::ble_at::notify_enabled() } #[cfg(not(feature = "ble-at"))] { false } },
        udp, crate::udp_multicast::received_len(),
        crate::modbus::shared::RS485_STATS.master_comerr(), crate::modbus::shared::RS485_STATS.master_apperr(),
        crate::modbus::shared::RS485_STATS.slave_comerr(), crate::modbus::shared::RS485_STATS.slave_apperr(),
        mode, stats.recoverable, stats.degradable, stats.severe,
        unsafe { esp_idf_sys::esp_get_free_heap_size() }, IO.sys.get_reset_count(), IO.sys.get_reset_reason(),
    );
    send_json(stream, 200, &json)
}

/// GET /getiodata
fn handle_get_io_data(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let di_bits = IO.di.load_bits();
    let do_bits = IO.do_.load_bits();

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
        let max_v = crate::bus::backends::read_hold_reg(regs::HOLD_SENSOR_MAX_BASE + i).unwrap_or(0);
        let min_v = crate::bus::backends::read_hold_reg(regs::HOLD_SENSOR_MIN_BASE + i).unwrap_or(0);
        ai_max.push_str(&format!("\"ai_addr_max_{}\":{}", i, max_v));
        ai_min.push_str(&format!("\"ai_addr_min_{}\":{}", i, min_v));
    }

    let json = format!(
        r#"{{"type":"getiodata","code":"0","msg":"","data":{{"di_data":{{{}}},"do_data":{{{}}},"ai_data":{{{}}},"ai_data_max":{{{}}},"ai_data_min":{{{}}}}}}}"#,
        di_data, do_data, ai_data, ai_max, ai_min,
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
        let name_bytes: Vec<u8> = cfg.ble_name.iter()
            .take_while(|&&b| b != 0)
            .copied().collect();
        let name = String::from_utf8_lossy(&name_bytes);
        format!(
            r#"{{"type":"getbleconfig","code":"0","data":{{"blename":"{}","mac":"{}"}}}}"#,
            escape_json(&name),
            escape_json(&format!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                cfg.ble_mac[0], cfg.ble_mac[1], cfg.ble_mac[2],
                cfg.ble_mac[3], cfg.ble_mac[4], cfg.ble_mac[5]
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
        for b in &mut cfg.ble_name { *b = 0; }
        let take = name_bytes.len().min(8);
        cfg.ble_name[..take].copy_from_slice(&name_bytes[..take]);
    });
    // 立即触发 GAP 名更新 (无需重启)
    crate::ble_at::notify_ble_name_changed();
    log::info!("[http] update BLE name: {:?}", name);
    crate::device::request_save_device_text();
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
            if i > 0 { sensor_data.push(','); }
            let min_v = s.holding_buf.get(base_min - cfg_base + i).copied().unwrap_or(605);
            let max_v = s.holding_buf.get(base_max - cfg_base + i).copied().unwrap_or(3016);
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
    let ch: usize = req.form_field("ch").and_then(|s| s.parse().ok()).unwrap_or(0);
    let min_v: u16 = req.form_field("min").and_then(|s| s.parse().ok()).unwrap_or(0);
    let max_v: u16 = req.form_field("max").and_then(|s| s.parse().ok()).unwrap_or(0);
    let ai_count = crate::config::hw_version::AI_COUNT as usize;
    if ch >= ai_count {
        return send_json(stream, 200, &json_response("updatesensorconfig", "0x01000003"));
    }
    let base_min = regs::HOLD_SENSOR_MIN_BASE as usize;
    let base_max = regs::HOLD_SENSOR_MAX_BASE as usize;
    let cfg_base = regs::HOLD_CFG_BASE as usize;
    let base_min_idx = base_min - cfg_base + ch;
    let base_max_idx = base_max - cfg_base + ch;
    crate::bus::backends::storage_modify(|snap| {
        if base_min_idx < snap.holding_buf.len() {
            snap.holding_buf[base_min_idx] = min_v;
        }
        if base_max_idx < snap.holding_buf.len() {
            snap.holding_buf[base_max_idx] = max_v;
        }
        snap.proto.dirty = true;
    });
    crate::device::request_save_device_text();
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

    if old_pwd != actual_pwd {
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
fn handle_ota_upload_stream(stream: TcpStream, content_length: usize) -> std::io::Result<()> {
    if content_length == 0 {
        return send_json(&mut { stream }, 400, &json_response("updateota", "400"));
    }
    if content_length > MAX_OTA_SIZE {
        log::warn!(
            "[http] OTA rejected: content_length={} > MAX_OTA_SIZE={}",
            content_length,
            MAX_OTA_SIZE
        );
        return send_json(&mut { stream }, 413, &json_response("updateota", "413"));
    }

    // 放宽超时: 固件上传是慢操作, 3s 全局超时易误触发
    stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(60))).ok();

    let total = content_length as u32;
    log::info!("[http] OTA upload (streaming): {} bytes", total);

    // 分块写入 OTA 分区
    match crate::ota::begin(total) {
        Ok(()) => {}
        Err(e) => {
            log::error!("[http] OTA begin failed: {}", e);
            // LOOP9: begin 失败时也尝试 abort (abort 内部对空会话是幂等 no-op), 防状态泄漏
            let _ = crate::ota::abort();
            return send_json(&mut { stream }, 500, &json_response("updateota", "500"));
        }
    }

    let mut reader = BufReader::new(stream);
    // LOOP18: OTA 上传单帧最大 4096B 过大, 在 8KB http-srv 栈上占用 50% 预算.
    // 缩小到 2048B (仍是合理块大小, OTA::write_chunk 处理任意长度), 给 stack
    // 溢出风险预留余量. BufReader 内部仍有 8KB heap 缓冲, 不影响吞吐.
    let mut buf = vec![0u8; 2048].into_boxed_slice();
    let mut received = 0usize;
    while received < content_length {
        let want = (content_length - received).min(buf.len());
        if let Err(e) = reader.read_exact(&mut buf[..want]) {
            log::error!("[http] OTA read failed at {}: {}", received, e);
            let _ = crate::ota::abort();
            // 取回 stream 发送错误响应
            let mut s = reader.into_inner();
            return send_json(&mut s, 500, &json_response("updateota", "500"));
        }
        match crate::ota::write_chunk(&buf[..want]) {
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
        assert_eq!(r, r#"{"type":"login","code":"0","data":{}}"#);
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
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("abc"), "abc");
        assert_eq!(url_decode("%41%42%43"), "ABC");
    }

    // ---- LOOP11: 会话管理回归测试 ----

    /// hex 编码 16 字节 → 32 字符, 与已知向量对照
    #[test]
    fn test_hex_encode_16() {
        let token = [0x01, 0x02, 0x0a, 0x0f, 0x10, 0xff, 0xab, 0xcd,
                     0x00, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33];
        let hex = hex_encode_16(&token);
        let s = std::str::from_utf8(&hex).unwrap();
        assert_eq!(s, "01020a0f10ffabcd0099887766554433");
    }

    /// 登录 → 已认证 → logout → 未认证 完整流程
    #[test]
    fn test_session_auth_flow() {
        // 初始: 无活跃会话
        destroy_session();
        assert!(!validate_session_cookie(b"00000000000000000000000000000000"));

        // 登录: 创建会话, Cookie 含正确 token hex
        let cookie = create_session_cookie();
        assert!(cookie.starts_with("ESPSESSIONID="));
        assert!(cookie.contains("HttpOnly"));
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
}
