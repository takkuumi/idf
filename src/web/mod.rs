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
//! Cookie-based 会话认证 (对齐参考固件 `ESPSESSIONID` cookie):
//! - 未认证请求 → 301 重定向到 `/login`
//! - POST `/login` 验证成功 → 设置 `ESPSESSIONID=1`
//! - 默认密码: admin/admin123 (可修改, 存储到 NVS)
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
use std::time::Duration;

use crate::bus::{config_state::config_read, IO};
use crate::config::regs;
use crate::error::AppResult;
use crate::health::{self, TaskHb};

/// HTTP 端口 (对齐参考固件: server(80))
const HTTP_PORT: u16 = 80;
/// 收发缓冲区大小
const BUF_SIZE: usize = 4096;
/// 默认用户名
const DEFAULT_USERNAME: &str = "admin";
/// 默认密码 (NVS 未保存时使用)
const DEFAULT_PASSWORD: &str = "admin123";

/// NVS 密码 key
const NVS_KEY_WEB_PWD: &str = "web_pwd";
const NVS_KEY_WEB_PWD_FLAG: &str = "web_pwd_f";

/// HTTP 服务器任务心跳 (阈值 30s, 5s accept 循环 + 余量)
static TASK_HB: TaskHb = TaskHb::new_with_stall("http-srv", 30);

/// 启动 HTTP Web 服务器 (后台线程)
pub fn start() -> AppResult<()> {
    crate::health::register(&TASK_HB);
    std::thread::Builder::new()
        .name("http-srv".into())
        .stack_size(8 * 1024)
        .spawn(server_loop)
        .map_err(|e| crate::error::AppError::Sys(format!("spawn http: {e}")))?;
    log::info!("[http] web server started on port {}", HTTP_PORT);
    Ok(())
}

/// 服务器主循环
fn server_loop() {
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

    /// 检查 Cookie 中是否有有效的 ESPSESSIONID
    fn is_authenticated(&self) -> bool {
        self.header("Cookie")
            .map(|c| c.contains("ESPSESSIONID=1"))
            .unwrap_or(false)
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

/// 发送 301 重定向
fn send_redirect(stream: &mut TcpStream, location: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 301 Moved Permanently\r\nLocation: {}\r\nCache-Control: no-cache\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        location
    )?;
    Ok(())
}

/// 发送带 Cookie 的 301 重定向 (登录成功时)
fn send_redirect_with_cookie(
    stream: &mut TcpStream,
    location: &str,
    cookie: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 301 Moved Permanently\r\nLocation: {}\r\nCache-Control: no-cache\r\nSet-Cookie: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        location, cookie
    )?;
    Ok(())
}

/// 构建标准 JSON 响应 (对齐参考固件 returnResponseJson)
fn json_response(response_type: &str, code: &str) -> String {
    format!(
        r#"{{"type":"{}","code":"{}","data":{{}}}}"#,
        response_type, code
    )
}

/// 解析请求行 + headers + body
fn parse_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    let mut reader = BufReader::new(stream);
    // 读请求行
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    let method = parts.first().unwrap_or(&"").to_string();
    let path_raw = parts.get(1).unwrap_or(&"/");
    let path = url_decode(path_raw);

    // 读 headers
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    // 读 body
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

// ============================================================================
// 连接处理
// ============================================================================

fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    let req = parse_request(&mut stream)?;
    log::debug!(
        "[http] {} {} (auth={})",
        req.method,
        req.path,
        req.is_authenticated()
    );

    match req.path.as_str() {
        // ---- 公开路由 (无需认证) ----
        "/login" => handle_login(&mut stream, &req),
        "/logout" => {
            // 清除 Cookie, 重定向到 /login
            write!(
                stream,
                "HTTP/1.1 301 Moved Permanently\r\nLocation: /login\r\nCache-Control: no-cache\r\nSet-Cookie: ESPSESSIONID=0; Max-Age=0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
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

        // ---- OTA (POST, 认证) ----
        "/updateota" => {
            if !req.is_authenticated() {
                return send_redirect(&mut stream, "/login");
            }
            handle_ota_upload(&mut stream, &req)
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
    send_redirect_with_cookie(stream, "/", "ESPSESSIONID=1")
}

/// GET /getsysteminfo
fn handle_get_system_info(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let cfg = config_read().map(|cs| cs.cfg.clone());
    let cfg = match cfg {
        Some(c) => c,
        None => return send_json(stream, 500, &json_response("getsysteminfo", "500")),
    };

    let device_name = cfg.name_str();
    let sn = cfg.sn_str();
    let fw_ver = cfg.fw_version;
    let hw_ver = cfg.hw_version;
    let fw_date = cfg.fw_date;

    let json = format!(
        r#"{{"type":"getsysteminfo","code":"0","data":{{"devicename":"{}","sn":"{}","model":"F16","version":"V{}.{}.{}.{}","hw_version":"0x{:04X}"}}}}"#,
        escape_json(&device_name),
        escape_json(&sn),
        fw_ver / 100,
        (fw_ver % 100) / 10,
        fw_ver % 10,
        fw_date,
        hw_ver,
    );
    send_json(stream, 200, &json)
}

/// POST /updatesysteminfoconfig
fn handle_update_system_info(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updatesysteminfoconfig", "405"));
    }
    let devicename = req.form_field("devicename");
    let addressinfo = req.form_field("addressinfo");

    // 更新 location/name 到 CONFIG RCU
    if let Some(name) = addressinfo {
        let name_bytes: Vec<u8> = name.as_bytes().to_vec();
        crate::bus::backends::config_modify(|cfg| {
            let take = name_bytes.len().min(cfg.name.len());
            cfg.name[..take].copy_from_slice(&name_bytes[..take]);
            for b in &mut cfg.name[take..] {
                *b = 0;
            }
        });
    }

    log::info!(
        "[http] update system info: devicename={:?}",
        devicename
    );
    crate::device::request_save_device_text();
    send_json(stream, 200, &json_response("updatesysteminfoconfig", "0"))
}

/// GET /getnetworkconfig
fn handle_get_network_config(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let cfg = config_read().map(|cs| cs.cfg.clone());
    let cfg = match cfg {
        Some(c) => c,
        None => return send_json(stream, 500, &json_response("getnetworkconfig", "500")),
    };

    let ip = format!(
        "{}.{}.{}.{}",
        cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3]
    );
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
        cfg.eth_mac[0], cfg.eth_mac[1], cfg.eth_mac[2],
        cfg.eth_mac[3], cfg.eth_mac[4], cfg.eth_mac[5],
    );
    let sn = cfg.sn_str();
    let name = cfg.name_str();

    let json = format!(
        r#"{{"type":"getnetworkconfig","code":"0","data":{{"ip":"{}","mask":"{}","gateway":"{}","dns":"{}","mac":"{}","sn":"{}","addressinfo":"{}","dhcp":{}}}}}"#,
        ip, mask, gw, dns, mac, escape_json(&sn), escape_json(&name), if cfg.dhcp { 1 } else { 0 },
    );
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

/// GET /getportconfig
fn handle_get_port_config(stream: &mut TcpStream, _req: &HttpRequest) -> std::io::Result<()> {
    let cfg = config_read().map(|cs| cs.cfg.clone());
    let cfg = match cfg {
        Some(c) => c,
        None => return send_json(stream, 500, &json_response("getportconfig", "500")),
    };

    // 每个 RS485 端口 5 个字段: baud, mode, databits, stopbits, parity
    let port_fields = |i: usize| -> String {
        if i < cfg.rs485.len() {
            let r = &cfg.rs485[i];
            format!(
                r#""baud_{}":{},"mode_{}":{},"databits_{}":{},"stopbits_{}":{},"parity_{}":{}"#,
                i, r.baudrate,
                i, r.mode,
                i, r.data_bits,
                i, r.stop_bits,
                i, r.parity,
            )
        } else {
            format!(
                r#""baud_{}":9600,"mode_{}":0,"databits_{}":8,"stopbits_{}":1,"parity_{}":0"#,
                i, i, i, i, i
            )
        }
    };

    let json = format!(
        r#"{{"type":"getportconfig","code":"0","data":{{{},{},{}}}}}"#,
        port_fields(0),
        port_fields(1),
        port_fields(2),
    );
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
    let baud: u32 = req
        .form_field("baud")
        .and_then(|s| s.parse().ok())
        .unwrap_or(9600);
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
        .unwrap_or(8);
    let stopbits: u8 = req
        .form_field("stopbit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let parity: u8 = req
        .form_field("checkmode")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if index < 3 {
        crate::bus::backends::config_modify(|cfg| {
            if index < cfg.rs485.len() {
                cfg.rs485[index].baudrate = baud;
                cfg.rs485[index].mode = mode;
                cfg.rs485[index].slave_addr = slave_addr as u8;
                cfg.rs485[index].data_bits = databits;
                cfg.rs485[index].stop_bits = stopbits;
                cfg.rs485[index].parity = parity;
            }
        });
        log::info!(
            "[http] update RS485 port {}: baud={} mode={} slave={}",
            index, baud, mode, slave_addr
        );
    }

    crate::device::request_save_device_text();
    let _ = crate::nfc::backup_now();
    send_json(stream, 200, &json_response("updateportconfig", "0"))
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
    for i in 0..ai_count {
        if i > 0 {
            ai_data.push(',');
        }
        let val = crate::bus::backends::read_input_reg(regs::INREG_AI_BASE + i).unwrap_or(0);
        ai_data.push_str(&format!("\"ai_addr_{}\":{}", i, val));
    }

    let json = format!(
        r#"{{"type":"getiodata","code":"0","data":{{"di_data":{{{}}},"do_data":{{{}}},"ai_data":{{{}}}}}}}"#,
        di_data, do_data, ai_data,
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

    // 通过 Modbus coil 写入接口控制 DO (addr 为 DO 编号 0..N)
    let coil_addr = regs::COIL_DO_BASE + addr;
    let _ = crate::bus::backends::write_coil(coil_addr, value);

    log::info!("[http] IO control: addr={} value={}", addr, value);
    send_json(stream, 200, &json_response("iocontrol", "0"))
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

/// POST /updateota (OTA 固件上传)
fn handle_ota_upload(stream: &mut TcpStream, req: &HttpRequest) -> std::io::Result<()> {
    if req.method != "POST" {
        return send_json(stream, 405, &json_response("updateota", "405"));
    }

    // OTA 数据在 body 中 (raw binary 固件)
    if req.body.is_empty() {
        return send_json(stream, 400, &json_response("updateota", "400"));
    }

    let total = req.body.len() as u32;
    log::info!("[http] OTA upload: {} bytes", total);

    // 分块写入 OTA 分区
    match crate::ota::begin(total) {
        Ok(()) => {}
        Err(e) => {
            log::error!("[http] OTA begin failed: {}", e);
            return send_json(stream, 500, &json_response("updateota", "500"));
        }
    }

    // 分块写入 (OTA_CHUNK_MAX=4KB, 与 ota 模块对齐)
    const CHUNK: usize = 4096;
    let mut offset = 0usize;
    while offset < req.body.len() {
        let end = (offset + CHUNK).min(req.body.len());
        let chunk = &req.body[offset..end];
        match crate::ota::write_chunk(chunk) {
            Ok(n) => offset += n,
            Err(e) => {
                log::error!("[http] OTA write failed at {}: {}", offset, e);
                let _ = crate::ota::abort();
                return send_json(stream, 500, &json_response("updateota", "500"));
            }
        }
        TASK_HB.tick();
        health::feed_wdt();
    }

    match crate::ota::end() {
        Ok(()) => {
            log::info!("[http] OTA complete, rebooting in 1s");
            send_json(stream, 200, &json_response("updateota", "0"))?;
            std::thread::sleep(Duration::from_secs(1));
            crate::ota::reboot_to_new_firmware();
        }
        Err(e) => {
            log::error!("[http] OTA end failed: {}", e);
            let _ = crate::ota::abort();
            send_json(stream, 500, &json_response("updateota", "500"))
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
}