# ESP32-S3R8 嵌入式网关 - 项目文档

> Rust + ESP-IDF 开发的 ESP32-S3R8 嵌入式网关系统

## 文档索引

| 文档 | 说明 |
|------|------|
| [architecture.md](architecture.md) | 系统架构、模块设计、技术选型 |
| [todo.md](todo.md) | 已知 TODO 与待完善点 |
| [build.md](build.md) | 构建与烧录步骤 |
| [verification.md](verification.md) | 验证清单 |
| [pinmap.md](pinmap.md) | GPIO 引脚分配表 |

## 系统概览

| 模块 | 方案 |
|------|------|
| 主控 | ESP32-S3R8 (Xtensa LX7 双核 240MHz, 512KB SRAM, **8MB Octal SPI PSRAM**) |
| 开发框架 | ESP-IDF v5.5.4 + Rust (esp-idf-sys 0.35 / hal 0.45 / svc 0.50) |
| 以太网 | WIZnet W5500 over SPI2 (硬wired TCP/IP + 10/100 MAC/PHY, 单口 + 应用层简单冗余) |
| BLE Mesh | Bluedroid + Proxy + Node + Generic OnOff (ESP32-S3R8 内置 BLE 5.0) |
| IO 点 | 8 路 DI (光耦隔离) + 8 路 DO (开漏) |
| 模拟通道 | 6 路 AI (ADC1 12-bit) + 4 路 AO (LEDC PWM) |
| RS485 | 2 路 (UART1 主站 + UART2 从站, 内置半双工模式) |
| Modbus | RTU Master + RTU Slave + TCP Server (手写不依赖 umodbus) |

## 启动顺序

```
1. 日志初始化 (默认 Info, 运行时可通过 0x0106 调节)
2. ESP-IDF 基础设施 (Peripherals / SystemEventLoop / TimerService)
3. HAL 初始化 (GPIO/UART/ADC/LEDC)
4. 设备协议存储初始化 (NVS 加载, 失败回退空 ProtoStore)
5. 复位原因记录 + 复位计数持久化
6. 启动以太网 (W5500 over SPI2 + LwIP)
7. 启动 BLE Mesh + BLE AT 命令通道
8. 启动 IO 扫描 (DI 1ms / DO 10ms)
9. 启动 AI/AO 通道 (100ms)
10. 启动 RS485 + Modbus (RTU Master + Slave + TCP Server)
11. 进入主循环 (喂狗 + 健康检查 + uptime / 复位请求 / 状态上报, 100ms)
```

## 项目结构

```
idf/
├── Cargo.toml              # 项目清单 (包名 esp32s3-iot-gateway)
├── .cargo/config.toml      # xtensaespidf 工具链配置
├── rust-toolchain.toml     # nightly + xtensaespidf target
├── build.rs                # embuild 编排 + feature cfg
├── sdkconfig.defaults      # ESP32-S3 + 8MB PSRAM + BLE Mesh + W5500 + Watchdog 配置
├── partitions.csv          # 8MB Flash 分区表
├── idf_component.yml       # W5500 外部 IDF Component 依赖
├── docs/                    # 本文档目录
└── src/
    ├── main.rs              # 启动入口
    ├── config.rs            # 引脚分配 + Modbus 参数 + 寄存器布局
    ├── error.rs             # AppError + AppResult
    ├── bus.rs               # 全局总线 + Modbus 寄存器映射
    ├── health.rs            # 任务看门狗 + 心跳健康监控
    ├── hal/                 # 硬件抽象层 (GPIO/UART/ADC/LEDC)
    ├── ethernet/            # W5500 驱动 (SPI2 独占管理)
    ├── rs485/               # RS485 收发 (UART1/UART2)
    ├── modbus/              # Modbus RTU/TCP
    ├── io/                  # DI/DO
    ├── channel/             # AI/AO
    ├── device/              # NVS 持久化 (协议存储 + 系统配置 + 复位计数)
    ├── ble_at/              # BLE AT 命令通道 (GATT 服务)
    └── blemesh/             # BLE Mesh (Proxy + Node + Generic OnOff)
```

## 关键特性

- **工业可靠性**：任务看门狗 (10s 超时) + 任务心跳 (停滞检测) + 复位原因持久化
- **3 路 UART**：UART0 下载/日志 + UART1 RS485 主站 + UART2 RS485 从站 (不再与下载串口复用)
- **8MB PSRAM**：Octal SPI 高速 PSRAM, malloc 后备 + 内部 SRAM 保留
- **W5500 硬件协议栈**：32KB 缓冲 + 8 socket, SPI 20MHz (最高 80MHz)
- **BLE Mesh**：Bluedroid + Provisioner + Proxy + Generic OnOff Server/Client
- **运行时配置**：SN/IP/网关/RS485/BLE 等通过 Modbus (0x0200-0x025F) 或 BLE AT 命令修改, NVS 持久化
