//! 设备类型定义 — 对齐参考固件 PROTOCOL_CONFIG_* 常量

/// 设备功能类型 (参考固件 PROTOCOL_CONFIG_TRAFFIC_SIGNAL_00 ~ PROTOCOL_CONFIG_NO2_02)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DeviceType {
    TrafficSignal2Pt,       // 0x00 两显车道指示器 2点控制
    TrafficSignalGreenLock, // 0x01 两显车道指示器 正绿自锁
    TrafficSignalRedLock,   // 0x02 两显车道指示器 正红自锁
    TrafficSignal4Pt,       // 0x03 四显车道指示器 4点控制
    TrafficSignal4PtInter,  // 0x04 四显 正绿反红互锁 2点
    TrafficSignal4PtDual,   // 0x05 四显 双面红灯互锁 2点
    TrafficSignal6Pt,       // 0x06 六显车道指示器 6点
    TrafficSignal6Pt5,      // 0x07 六显 5点控制
    TrafficTurnSingle,      // 0x08 单面左转指示器
    TrafficTurnDual,        // 0x09 双面左转指示器
    CrossHoleSignal,        // 0x0A 横洞指示器
    Signal3Light,           // 0x0B 3显信号灯
    Signal4Light,           // 0x0C 4显信号灯
    JetFan2Pt,              // 0x0D 射流风机 2点
    JetFan3Pt,              // 0x0E 射流风机 3点
    JetFan2PtInterlock,     // 0x0F 射流风机 2点互锁
    Blower2Pt,              // 0x10 排送风机 2点
    Blower1Pt,              // 0x11 排送风机 1点
    Lighting2Pt,            // 0x12 照明 2点
    Lighting1Pt,            // 0x13 照明 1点
    Pump2Pt,                // 0x14 水泵 2点
    Pump1Pt,                // 0x15 水泵 1点
    RollingShutter,         // 0x16 车通卷帘门
    FireDoor1Pt,            // 0x17 人通防火门 1点
    FireDoor2Pt,            // 0x18 人通防火门 2点
    CoviSensor,             // 0x19 COVI传感器
    No2Sensor,              // 0x1A NO2传感器
    CoviNo2Sensor,          // 0x1B COVI+NO2
    WindDirSpeed,           // 0x1C 风速风向
    HoleLightIntensity,     // 0x1D 洞内光强
    OutLightIntensity,      // 0x1E 洞外光强
    CarCheck,               // 0x1F 车检器
    Rs485Config1,           // 0x20 RS485-1配置
    Rs485Config2,           // 0x21 RS485-2配置
    PositionNumber,         // 0x22 位置编号
    Unknown(u8),
}

impl DeviceType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0x00 => Self::TrafficSignal2Pt,
            0x01 => Self::TrafficSignalGreenLock,
            0x02 => Self::TrafficSignalRedLock,
            0x03 => Self::TrafficSignal4Pt,
            0x04 => Self::TrafficSignal4PtInter,
            0x05 => Self::TrafficSignal4PtDual,
            0x06 => Self::TrafficSignal6Pt,
            0x07 => Self::TrafficSignal6Pt5,
            0x08 => Self::TrafficTurnSingle,
            0x09 => Self::TrafficTurnDual,
            0x0A => Self::CrossHoleSignal,
            0x0B => Self::Signal3Light,
            0x0C => Self::Signal4Light,
            0x0D => Self::JetFan2Pt,
            0x0E => Self::JetFan3Pt,
            0x0F => Self::JetFan2PtInterlock,
            0x10 => Self::Blower2Pt,
            0x11 => Self::Blower1Pt,
            0x12 => Self::Lighting2Pt,
            0x13 => Self::Lighting1Pt,
            0x14 => Self::Pump2Pt,
            0x15 => Self::Pump1Pt,
            0x16 => Self::RollingShutter,
            0x17 => Self::FireDoor1Pt,
            0x18 => Self::FireDoor2Pt,
            0x19 => Self::CoviSensor,
            0x1A => Self::No2Sensor,
            0x1B => Self::CoviNo2Sensor,
            0x1C => Self::WindDirSpeed,
            0x1D => Self::HoleLightIntensity,
            0x1E => Self::OutLightIntensity,
            0x1F => Self::CarCheck,
            0x20 => Self::Rs485Config1,
            0x21 => Self::Rs485Config2,
            0x22 => Self::PositionNumber,
            _ => Self::Unknown(v),
        }
    }

    pub fn as_u8(&self) -> u8 {
        match self {
            Self::TrafficSignal2Pt => 0x00,
            Self::TrafficSignalGreenLock => 0x01,
            Self::TrafficSignalRedLock => 0x02,
            Self::TrafficSignal4Pt => 0x03,
            Self::TrafficSignal4PtInter => 0x04,
            Self::TrafficSignal4PtDual => 0x05,
            Self::TrafficSignal6Pt => 0x06,
            Self::TrafficSignal6Pt5 => 0x07,
            Self::TrafficTurnSingle => 0x08,
            Self::TrafficTurnDual => 0x09,
            Self::CrossHoleSignal => 0x0A,
            Self::Signal3Light => 0x0B,
            Self::Signal4Light => 0x0C,
            Self::JetFan2Pt => 0x0D,
            Self::JetFan3Pt => 0x0E,
            Self::JetFan2PtInterlock => 0x0F,
            Self::Blower2Pt => 0x10,
            Self::Blower1Pt => 0x11,
            Self::Lighting2Pt => 0x12,
            Self::Lighting1Pt => 0x13,
            Self::Pump2Pt => 0x14,
            Self::Pump1Pt => 0x15,
            Self::RollingShutter => 0x16,
            Self::FireDoor1Pt => 0x17,
            Self::FireDoor2Pt => 0x18,
            Self::CoviSensor => 0x19,
            Self::No2Sensor => 0x1A,
            Self::CoviNo2Sensor => 0x1B,
            Self::WindDirSpeed => 0x1C,
            Self::HoleLightIntensity => 0x1D,
            Self::OutLightIntensity => 0x1E,
            Self::CarCheck => 0x1F,
            Self::Rs485Config1 => 0x20,
            Self::Rs485Config2 => 0x21,
            Self::PositionNumber => 0x22,
            Self::Unknown(v) => *v,
        }
    }
}

/// 设备功能元数据 (每个设备类型的静态描述)
pub struct DeviceFunctionMeta {
    pub io_count: u8,   // 需要的 IO 点数
    pub has_sensor: bool, // 是否关联传感器
    pub name: &'static str,
}

/// 设备功能 trait: 每种设备类型实现自己的轮询和控制逻辑
pub trait DeviceFunction: Send + Sync {
    fn meta(&self) -> DeviceFunctionMeta;
    /// 构建 RS485 轮询请求帧
    fn build_poll_request(&self, slave: u8) -> heapless::Vec<u8, 16>;
    /// 处理轮询响应, 更新总线状态
    fn process_response(&self, data: &[u8]) -> AppResult<()>;
    /// 处理控制命令 (写线圈/寄存器)
    fn handle_control(&self, channel: u8, value: u16) -> AppResult<()>;
}

use crate::error::AppResult;
