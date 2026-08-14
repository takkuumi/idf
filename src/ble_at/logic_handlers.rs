//! BLE 逻辑配置子协议处理器 (MCA 0xD0/0xD1/0xD2/0xD3)
//!
//! 移植自参考固件 BleMsgDeal.cpp 的 BLE 逻辑配置协议:
//! - 0xD0 LOGIC_CONFIG: 写入设备功能配置 (按 sub-command 存储到 NVS)
//! - 0xD1 LOGIC_RETRIEVE: 读取已存储的设备功能配置
//! - 0xD2 DELETE_CONFIG: 删除所有设备功能配置
//! - 0xD3 COM_REQUEST: 查询当前 DI/DO 状态位图
//!
//! 帧格式 (对齐参考固件 vComboPacket):
//!   length(1) + mac(6) + cmd(1) + sub(1) + data(N) + crc(2 LE)
//!
//! 配置存储: 每个设备类型 (sub-command 0x00-0x22) 的配置以 NVS blob 持久化.

use heapless::Vec as HVec;

/// 配置项 NVS 命名空间
const NVS_NAMESPACE: &str = "logic_cfg";
/// 配置有效标志字节
const CFG_VALID_FLAG: u8 = 0x55;

/// 设备功能子命令定义 (对齐参考固件 PROTOCOL_CONFIG_*)
/// 0x00-0x22: 35 种设备类型的配置写入/读取
/// 0x23-0x28: IO 状态查询/响应 (0xD3 专用)
pub const SUB_TRAFFIC_00: u8 = 0x00;
pub const SUB_DEVICE_01: u8 = 0x01;
pub const SUB_TRAFFIC_02: u8 = 0x02;
pub const SUB_TRAFFIC_03: u8 = 0x03;
pub const SUB_TRAFFIC_04: u8 = 0x04;
pub const SUB_TRAFFIC_05: u8 = 0x05;
pub const SUB_TRAFFIC_06: u8 = 0x06;
pub const SUB_TRAFFIC_07: u8 = 0x07;
pub const SUB_TRAFFIC_08: u8 = 0x08;
pub const SUB_TRAFFIC_09: u8 = 0x09;
pub const SUB_CROSS_HOLE: u8 = 0x0A;
pub const SUB_3LIGHT: u8 = 0x0B;
pub const SUB_4LIGHT: u8 = 0x0C;
pub const SUB_JET_FAN_2: u8 = 0x0D;
pub const SUB_JET_FAN_3: u8 = 0x0E;
pub const SUB_JET_FAN_2IL: u8 = 0x0F;
pub const SUB_BLOWER_2: u8 = 0x10;
pub const SUB_BLOWER_1: u8 = 0x11;
pub const SUB_LIGHT_2: u8 = 0x12;
pub const SUB_LIGHT_1: u8 = 0x13;
pub const SUB_PUMP_2: u8 = 0x14;
pub const SUB_PUMP_1: u8 = 0x15;
pub const SUB_SHUTTER: u8 = 0x16;
pub const SUB_FIRE_DOOR_1: u8 = 0x17;
pub const SUB_FIRE_DOOR_2: u8 = 0x18;
pub const SUB_COVI: u8 = 0x19;
pub const SUB_NO2: u8 = 0x1A;
pub const SUB_COVI_NO2: u8 = 0x1B;
pub const SUB_WIND: u8 = 0x1C;
pub const SUB_HOLE_LIGHT: u8 = 0x1D;
pub const SUB_OUT_LIGHT: u8 = 0x1E;
pub const SUB_CAR_CHECK: u8 = 0x1F;
pub const SUB_RS485_01: u8 = 0x20;
pub const SUB_RS485_02: u8 = 0x21;
pub const SUB_POSITION: u8 = 0x22;
pub const SUB_COM_INPUT_MSG: u8 = 0x23;
pub const SUB_COM_OUTPUT_MSG: u8 = 0x24;
pub const SUB_COM_REQUEST: u8 = 0x26;
pub const SUB_COM_INPUT_ANS: u8 = 0x27;
pub const SUB_COM_OUTPUT_ANS: u8 = 0x28;

/// 子命令名称表 (用于日志)
fn sub_name(sub: u8) -> &'static str {
    match sub {
        0x00 => "TrafficSignal00",
        0x01 => "DeviceSignal01",
        0x02 => "TrafficSignal02",
        0x03 => "TrafficSignal03",
        0x04 => "TrafficSignal04",
        0x05 => "TrafficSignal05",
        0x06 => "TrafficSignal06",
        0x07 => "TrafficSignal07",
        0x08 => "TrafficSignal08",
        0x09 => "TrafficSignal09",
        0x0A => "CrossHole",
        0x0B => "3Light",
        0x0C => "4Light",
        0x0D => "JetFan2",
        0x0E => "JetFan3",
        0x0F => "JetFan2IL",
        0x10 => "Blower2",
        0x11 => "Blower1",
        0x12 => "Light2",
        0x13 => "Light1",
        0x14 => "Pump2",
        0x15 => "Pump1",
        0x16 => "Shutter",
        0x17 => "FireDoor1",
        0x18 => "FireDoor2",
        0x19 => "COVI",
        0x1A => "NO2",
        0x1B => "COVI+NO2",
        0x1C => "WindDirSpeed",
        0x1D => "HoleLight",
        0x1E => "OutLight",
        0x1F => "CarCheck",
        0x20 => "RS485-1",
        0x21 => "RS485-2",
        0x22 => "Position",
        0x23 => "ComInputMsg",
        0x24 => "ComOutputMsg",
        0x26 => "ComRequest",
        0x27 => "ComInputAns",
        0x28 => "ComOutputAns",
        _ => "Unknown",
    }
}

/// NVS key for a given sub-command
fn nvs_key_for(sub: u8) -> String {
    format!("cfg_{:02x}", sub)
}

/// 存储配置到 NVS
///
/// 复用 device 模块的全局 NVS 句柄 (gateway namespace), 用 key 前缀 "lg_" 隔离.
/// 避免 open 新 namespace (EspDefaultNvsInstance 在 esp-idf-svc 0.52 不存在).
fn store_config(sub: u8, data: &[u8]) -> bool {
    // blob = [valid_flag][len][data...]
    let mut blob: HVec<u8, 256> = HVec::new();
    if blob.push(CFG_VALID_FLAG).is_err() {
        log::error!("[logic_cfg] blob overflow (flag)");
        return false;
    }
    if blob.push(data.len() as u8).is_err() {
        log::error!("[logic_cfg] blob overflow (len)");
        return false;
    }
    if blob.extend_from_slice(data).is_err() {
        log::error!("[logic_cfg] blob overflow (data len={})", data.len());
        return false;
    }
    let key = nvs_key_for(sub);
    match crate::device::try_with_nvs_mut(|nvs| {
        nvs.set_blob(&key, &blob)
            .map_err(|e| format!("nvs set_blob {key}: {e:?}"))
    }) {
        Some(Ok(())) => {
            log::info!(
                "[logic_cfg] stored sub=0x{:02X} ({}): {} bytes",
                sub,
                sub_name(sub),
                data.len()
            );
            true
        }
        Some(Err(msg)) => {
            log::error!("[logic_cfg] NVS write failed sub=0x{:02X}: {}", sub, msg);
            false
        }
        None => {
            log::warn!("[logic_cfg] NVS unavailable, sub=0x{:02X} not stored", sub);
            false
        }
    }
}

/// 从 NVS 读取配置
fn load_config(sub: u8) -> Option<Vec<u8>> {
    let key = nvs_key_for(sub);
    let mut buf = [0u8; 256];
    let blob = crate::device::try_with_nvs(|nvs| nvs.get_blob(&key, &mut buf).ok())
        .flatten()
        .flatten()?;
    if blob.len() < 2 || blob[0] != CFG_VALID_FLAG {
        return None;
    }
    let len = blob[1] as usize;
    if blob.len() < 2 + len {
        return None;
    }
    Some(blob[2..2 + len].to_vec())
}

/// 删除所有配置 (NVS erase)
fn delete_all_configs() -> bool {
    let mut deleted = 0;
    for sub in 0x00..=0x28u8 {
        let key = nvs_key_for(sub);
        if let Some(Ok(_existed)) = crate::device::try_with_nvs_mut(|nvs| nvs.remove(&key)) {
            deleted += 1;
        }
    }
    log::info!("[logic_cfg] deleted {} config entries", deleted);
    true
}

/// 构建 ACK 帧 (对齐参考固件 vNormalAckPacket)
/// 返回: [cmd=0xD0, sub, status=0x00(success)]
fn build_ack(cmd: u8, sub: u8, status: u8) -> heapless::Vec<u8, 256> {
    let mut rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
    let _ = rsp.push(cmd);
    let _ = rsp.push(sub);
    let _ = rsp.push(status);
    rsp
}

/// 处理 0xD0 LOGIC_CONFIG (写入设备功能配置)
///
/// 对齐参考固件 DealLogicConfigCMD: 接收 sub=0x00-0x28 的配置数据并存储.
/// data 已剥离 BLE 帧头 (从 length 字节之后开始).
pub fn handle_logic_config(data: &[u8]) -> Option<heapless::Vec<u8, 256>> {
    if data.len() < 2 {
        log::warn!("[logic_cfg] D0: data too short ({} bytes)", data.len());
        return None;
    }
    // data[0] = sub-command (设备类型)
    // data[1..] = 配置有效载荷
    let sub = data[0];
    let payload = &data[1..];

    log::info!(
        "[logic_cfg] D0 LOGIC_CONFIG: sub=0x{:02X} ({}), payload={} bytes",
        sub,
        sub_name(sub),
        payload.len()
    );

    let success = if sub <= 0x22 || sub == 0x23 || sub == 0x24 || sub == 0x26 {
        // 通用配置存储 (0x00-0x22, 0x23, 0x24, 0x26)
        store_config(sub, payload)
    } else if sub == 0x01 {
        // 0x01: 设备功能配置 (写入 PRegBuf SLAVE_DEVICE_CONFIG 区域)
        // 对齐参考固件 vConfigDeviceSingle01: payload[0..2] = count (LE),
        // 然后 N 个变长条目写入寄存器 2300+
        store_config(sub, payload)
    } else {
        log::warn!("[logic_cfg] D0: unsupported sub=0x{:02X}", sub);
        false
    };

    if success {
        Some(build_ack(0xD0, sub, 0x00))
    } else {
        Some(build_ack(0xD0, sub, 0x84)) // DEAL error
    }
}

/// 处理 0xD1 LOGIC_RETRIEVE (读取设备功能配置)
///
/// 对齐参考固件 DealLogicRetrieveCMD: 根据 sub 查询已存储的配置并返回.
pub fn handle_logic_retrieve(data: &[u8]) -> Option<heapless::Vec<u8, 256>> {
    if data.is_empty() {
        log::warn!("[logic_cfg] D1: empty data");
        return None;
    }
    let sub = data[0];
    log::info!(
        "[logic_cfg] D1 LOGIC_RETRIEVE: sub=0x{:02X} ({})",
        sub,
        sub_name(sub)
    );

    match load_config(sub) {
        Some(cfg_data) => {
            let mut rsp: heapless::Vec<u8, 256> = heapless::Vec::new();
            let _ = rsp.push(0xD1);
            let _ = rsp.push(sub);
            let _ = rsp.extend_from_slice(&cfg_data);
            log::info!(
                "[logic_cfg] D1: sub=0x{:02X} ({}) → {} bytes",
                sub,
                sub_name(sub),
                cfg_data.len()
            );
            Some(rsp)
        }
        None => {
            log::info!(
                "[logic_cfg] D1: sub=0x{:02X} ({}) not configured",
                sub,
                sub_name(sub)
            );
            // 参考固件: 未配置时返回错误帧 (cmd 字段保持 0xD1 与请求匹配)
            Some(build_ack(0xD1, sub, 0x84))
        }
    }
}

/// 处理 0xD2 DELETE_CONFIG (删除所有设备功能配置)
///
/// 对齐参考固件 vDeleteAllLogic: 清除所有配置文件.
pub fn handle_delete_config() -> Option<heapless::Vec<u8, 256>> {
    log::info!("[logic_cfg] D2 DELETE_CONFIG: clearing all configs");
    let success = delete_all_configs();
    if success {
        Some(build_ack(0xD2, 0x00, 0x00))
    } else {
        Some(build_ack(0xD2, 0x00, 0x84))
    }
}

/// 处理 0xD3 COM_REQUEST (查询 DI/DO 状态)
///
/// 对齐参考固件 vConfigComReuest: 返回当前 DI/DO 位图.
/// 返回两帧: 第一帧 DI 位图 (sub=0x27), 第二帧 DO 位图 (sub=0x28).
pub fn handle_com_request() -> Option<heapless::Vec<u8, 256>> {
    log::info!("[logic_cfg] D3 COM_REQUEST: querying DI/DO status");

    // 读取当前 DI 状态 (离散输入位图)
    let di_bitmap = read_io_bitmap(true);
    // 读取当前 DO 状态 (线圈位图)
    let do_bitmap = read_io_bitmap(false);

    let mut rsp: heapless::Vec<u8, 256> = heapless::Vec::new();

    // 第一帧: DI 位图 (cmd=0xD3, sub=0x27)
    let _ = rsp.push(0xD3);
    let _ = rsp.push(SUB_COM_INPUT_ANS);
    let _ = rsp.extend_from_slice(&di_bitmap);
    // 第二帧: DO 位图 (cmd=0xD3, sub=0x28)
    let _ = rsp.push(0xD3);
    let _ = rsp.push(SUB_COM_OUTPUT_ANS);
    let _ = rsp.extend_from_slice(&do_bitmap);

    Some(rsp)
}

/// 读取 IO 位图 (DI 或 DO)
fn read_io_bitmap(is_di: bool) -> heapless::Vec<u8, 8> {
    let mut bitmap: heapless::Vec<u8, 8> = heapless::Vec::new();
    let count = if is_di {
        crate::config::hw_version::DI_COUNT
    } else {
        crate::config::hw_version::DO_COUNT
    };
    let io = &crate::bus::IO;

    let mut byte = 0u8;
    let mut bit_pos = 0;
    for ch in 0..count {
        let bit = if is_di {
            io.di.get_bit(ch)
        } else {
            io.do_.get_bit(ch)
        };
        if bit {
            byte |= 1 << bit_pos;
        }
        bit_pos += 1;
        if bit_pos >= 8 {
            let _ = bitmap.push(byte);
            byte = 0;
            bit_pos = 0;
        }
    }
    // 剩余位 (不足 8 的倍数)
    if bit_pos > 0 {
        let _ = bitmap.push(byte);
    }
    bitmap
}

/// 入口: 根据 func 分发到对应处理函数
///
/// 返回 Some(response_data) 表示已处理, 调用方用 send_ble_frame 发送.
/// 返回 None 表示未处理 (fallback 到标准 Modbus 路径).
pub fn dispatch_logic_cmd(func: u8, data: &[u8]) -> Option<heapless::Vec<u8, 256>> {
    match func {
        0xD0 => handle_logic_config(data),
        0xD1 => handle_logic_retrieve(data),
        0xD2 => handle_delete_config(),
        0xD3 => handle_com_request(),
        _ => None,
    }
}
