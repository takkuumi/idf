# ESP32-S3R2 工业网关 用户手册

> 固件版本：v2.2.1 ｜ 硬件平台：ESP32-S3R2 ｜ 文档版本：1.1
>
> 适用硬件版本：Default / F3 / F4（详见第 2 章）

---

## 目录

1. [产品概述](#1-产品概述)
2. [硬件版本说明](#2-硬件版本说明)
3. [接线指南](#3-接线指南)
4. [通信接口配置](#4-通信接口配置)
5. [Modbus 寄存器映射表](#5-modbus-寄存器映射表)
6. [BLE AT 命令手册](#6-ble-at-命令手册)
7. [OTA 升级流程](#7-ota-升级流程)
8. [故障排查](#8-故障排查)

---

## 1. 产品概述

### 1.1 硬件规格

| 项目 | 规格 |
|------|------|
| 主控 MCU | ESP32-S3R2（Xtensa LX7 双核 32-bit，240MHz） |
| 内部 SRAM | 512 KB |
| 片外 PSRAM | 2 MB Quad SPI（80MHz） |
| 片上 Flash | 8 MB Quad SPI |
| 以太网 | WIZnet W5500（硬件 TCP/IP 协议栈，10/100M，SPI2 @ 20MHz） |
| RS485 | 2 路（UART1 主站 + UART2 从站，独立隔离） |
| 蓝牙 | BLE 5.0 + Bluetooth Mesh（Bluedroid 协议栈） |
| Wi-Fi | 802.11 b/g/n（可选，作为以太网冗余链路，需 `--features wifi` 编译） |
| 数字输入 DI | 8 / 16 / 48 路（按硬件版本，光耦隔离） |
| 数字输出 DO | 8 / 16 路（按硬件版本，开漏输出 100mA） |
| 模拟输入 AI | 6 路（12-bit SAR ADC，0~3.1V，4-20mA 转换） |
| 模拟输出 AO | 4 路（LEDC PWM，5kHz / 12-bit，与 DO0-DO3 共用引脚） |
| 看门狗 | 硬件任务看门狗（10s）+ 软件心跳（双级保护） |
| 加密 | AES/SHA/MPI/GCM/ECC 硬件加速器 |
| 供电 | 5V DC（典型值，工业供电） |

### 1.2 软件功能

| 模块 | 功能描述 |
|------|---------|
| **Modbus RTU 主站** | UART1 上轮询 1~247 号从站，200ms 周期，支持 FC=01/02/03/04 |
| **Modbus RTU 从站** | UART2 上响应主站请求，本机地址 1，支持 FC=01/02/03/04/05/06/0F/10 |
| **Modbus TCP Server** | TCP 502 端口，最多 4 路并发连接，MBAP 协议 |
| **BLE Mesh** | Generic OnOff Server/Client 模型，Proxy 节点，静态 OOB 配网 |
| **BLE GATT AT 通道** | 自定义服务（UUID 0xFF01），用于配置阶段读写参数 |
| **OTA 升级** | BLE AT 与 Modbus 双触发入口，支持分区切换与回滚 |
| **协议存储区** | 1500 个 U16 用户自定义协议数据，NVS 持久化 |
| **健康监控** | 任务看门狗 + 11 个任务心跳 + 任务健康位图上报 |
| **日志系统** | 5 级日志（Error/Warn/Info/Debug/Trace），运行时通过 Modbus 0x0106 调节 |

### 1.3 软件技术栈

- **语言**：Rust（nightly，Xtensa target）
- **HAL**：`esp-idf-hal` v0.45 + `esp-idf-svc`
- **系统**：ESP-IDF v5.5.4 + FreeRTOS SMP
- **并发模型**：`std::thread` + `parking_lot::Mutex` + `once_cell::Lazy`
- **任务绑定**：双核 SMP 亲和性（Core 0 网络/协议，Core 1 实时 IO）

---

## 2. 硬件版本说明

通过编译期 feature flag 切换硬件版本，**不支持运行时切换**。F3 与 F4 互斥（`build.rs` 编译期校验）。

### 2.1 版本对比表

| 项目 | Default | F3 | F4 |
|------|---------|----|----|
| **编译命令** | `cargo build` | `cargo build --features f3` | `cargo build --features f4` |
| **DI 通道数** | 8（GPIO 直驱） | 16（I2C 扩展） | 48（I2C 扩展） |
| **DO 通道数** | 8（GPIO 直驱） | 16（I2C 扩展） | 16（I2C 扩展） |
| **AI 通道数** | 6 | 6 | 6 |
| **AO 通道数** | 4 | 4 | 4 |
| **MCP23017 数量** | 0 | 2 片 | 4 片 |
| **I2C 总线** | 不启用 | 启用（GPIO21/33） | 启用（GPIO21/33） |
| **DI 寄存器范围** | 0x0000-0x0007 | 0x0000-0x000F | 0x0000-0x002F |
| **DO 寄存器范围** | 0x0000-0x0007 | 0x0000-0x000F | 0x0000-0x000F |

> **注意**：AO 通道与 DO0~DO3 共用 GPIO（GPIO8/9/16/38），硬件互斥，由硬件跳线或硬件版本决定使用模式。

### 2.2 F3/F4 MCP23017 I2C 地址分配

| 版本 | 芯片 | I2C 地址 | A2/A1/A0 | 用途 |
|------|------|---------|----------|------|
| F3 | U1 | 0x20 | 0/0/0 | DI 0-15（PORTA=DI0-7，PORTB=DI8-15） |
| F3 | U2 | 0x21 | 0/0/1 | DO 0-15（PORTA=DO0-7，PORTB=DO8-15） |
| F4 | U1 | 0x20 | 0/0/0 | DI 0-15 |
| F4 | U2 | 0x21 | 0/0/1 | DI 16-31 |
| F4 | U3 | 0x22 | 0/1/0 | DI 32-47 |
| F4 | U4 | 0x23 | 0/1/1 | DO 0-15 |

I2C 总线参数：I2C0 端口，SDA=GPIO21，SCL=GPIO33，400kHz Fast Mode，MCP23017 内部上拉使能。

---

## 3. 接线指南

### 3.1 GPIO 引脚分配总表

| 模块 | 引脚 | 功能 | 备注 |
|------|------|------|------|
| **SPI2 (W5500)** | GPIO11 | MOSI | W5500 SPI 主出从入 |
| | GPIO13 | MISO | W5500 SPI 主入从出 |
| | GPIO12 | SCLK | W5500 SPI 时钟 |
| | GPIO10 | CS | W5500 SPI 片选 |
| | GPIO14 | INT | W5500 中断（低有效） |
| | GPIO15 | RST | W5500 复位（低有效，50ms 脉冲） |
| **UART0 (下载/日志)** | GPIO43 | TX | 115200-N-8-1 |
| | GPIO44 | RX | 与下载串口共用 |
| **UART1 (RS485 #0)** | GPIO40 | TX | RTU Master，9600bps |
| | GPIO41 | RX | |
| | GPIO42 | DE/RE | RTS 共控（高=发送，低=接收） |
| **UART2 (RS485 #1)** | GPIO17 | TX | RTU Slave，9600bps，地址 1 |
| | GPIO18 | RX | |
| | GPIO7 | DE/RE | RTS 共控 |
| **DI (默认版本 8 路)** | GPIO19/20/21/33/34/35/36/37 | DI0~DI7 | 光耦隔离，Pull Down |
| **DO (默认版本 8 路)** | GPIO8/9/16/38/39/45/46/48 | DO0~DO7 | OC 输出 |
| **AI (6 路, ADC1)** | GPIO1/2/3/4/5/6 | AI0~AI5 | 12-bit SAR ADC，对应 ADC1_CH0~CH5 |
| **AO (4 路, LEDC)** | GPIO8/9/16/38 | AO0~AO3 | 与 DO0~DO3 共用引脚（硬件互斥） |
| **I2C (仅 F3/F4)** | GPIO21 | SDA | MCP23017 数据线（默认版本作 DI2） |
| | GPIO33 | SCL | MCP23017 时钟线（默认版本作 DI3） |

### 3.2 ADC1 通道映射

| ADC 通道 | 对应 GPIO | 用途 |
|----------|-----------|------|
| ADC1_CH0 | GPIO1 | AI0 |
| ADC1_CH1 | GPIO2 | AI1 |
| ADC1_CH2 | GPIO3 | AI2（同时是 strapping，ADC 高阻输入不影响） |
| ADC1_CH3 | GPIO4 | AI3 |
| ADC1_CH4 | GPIO5 | AI4 |
| ADC1_CH5 | GPIO6 | AI5 |

### 3.3 W5500 以太网接线

```
ESP32-S3R2                W5500 Module
GPIO11 (MOSI) ──────────── MOSI
GPIO13 (MISO) ──────────── MISO
GPIO12 (SCLK) ──────────── SCLK
GPIO10 (CS)   ──────────── SCSn
GPIO14 (INT)  ──────────── INTn   (低有效中断)
GPIO15 (RST)  ──────────── RSTn   (低有效复位)
3V3           ──────────── VCC
GND           ──────────── GND
                          RJ45 → 网线
```

- SPI 时钟 20MHz，Mode 0
- W5500 PHY 地址固定为 0（内部 PHY）
- 默认 DHCP 获取 IP；可改为静态 IP（见 4.2 节）

### 3.4 RS485 接线

#### 3.4.1 RS485 #0（主站，UART1）

```
ESP32-S3R2                RS485 收发器 (如 SP3485/MAX485)
GPIO40 (TX)   ──────────── DI   (Driver Input)
GPIO41 (RX)   ──────────── RO   (Receiver Output)
GPIO42 (DE/RE) ─────────── DE + /RE  (共控，高=发送)
                          A  ────  RS485 A
                          B  ────  RS485 B
                          GND ────  RS485 GND（建议共地）
```

#### 3.4.2 RS485 #1（从站，UART2）

```
ESP32-S3R2                RS485 收发器
GPIO17 (TX)   ──────────── DI
GPIO18 (RX)   ──────────── RO
GPIO7  (DE/RE) ─────────── DE + /RE
                          A  ────  RS485 A
                          B  ────  RS485 B
                          GND ────  RS485 GND
```

> **注意**：ESP-IDF UART RS485 模式自动控制 DE/RE 时序，无需外部方向切换电路。

### 3.5 I2C 扩展接线（仅 F3/F4）

```
ESP32-S3R2                MCP23017 (U1, 0x20)
GPIO21 (SDA) ─┬────────── SDA
GPIO33 (SCL) ─┼────────── SCL
              │            A0=0, A1=0, A2=0
              │            VCC=3V3, GND=GND
              │
              ├────────── MCP23017 (U2, 0x21, F3/F4)
              │            A0=1, A1=0, A2=0
              │
              ├────────── MCP23017 (U3, 0x22, F4 only)
              │            A0=0, A1=1, A2=0
              │
              └────────── MCP23017 (U4, 0x23, F4 only)
                           A0=1, A1=1, A2=0
```

- I2C 频率 400kHz（Fast Mode）
- MCP23017 内部上拉使能，外部可不接上拉电阻
- 建议在 SDA/SCL 上各串接 100Ω 阻尼电阻

### 3.6 供电与接地

- 主电源 5V DC，纹波 ≤ 100mV
- RS485 与 MCU 共地（远距离通信建议单点接地）
- 光耦隔离 DI 输入需独立供电（5V/24V 按现场设备电压）
- AO 输出若驱动电流 > 100mA 需外接功率驱动级

### 3.7 GPIO 占用总览图

```
GPIO0  (strapping)   GPIO26~32 (Flash/PSRAM, 不可用)
GPIO1  AI0 (ADC1_0)  GPIO33 DI3 / F3-F4 I2C SCL
GPIO2  AI1 (ADC1_1)  GPIO34 DI4
GPIO3  AI2 (ADC1_2)  GPIO35 DI5   (strapping)
GPIO4  AI3 (ADC1_3)  GPIO36 DI6
GPIO5  AI4 (ADC1_4)  GPIO37 DI7
GPIO6  AI5 (ADC1_5)  GPIO38 DO3/AO3
GPIO7  RS485_1 DE    GPIO39 DO4
GPIO8  DO0/AO0       GPIO40 RS485_0 TX (UART1)
GPIO9  DO1/AO1       GPIO41 RS485_0 RX
GPIO10 W5500 CS      GPIO42 RS485_0 DE
GPIO11 W5500 MOSI    GPIO43 UART0 TX (下载/日志)
GPIO12 W5500 SCLK    GPIO44 UART0 RX
GPIO13 W5500 MISO    GPIO45 DO5 (strapping)
GPIO14 W5500 INT     GPIO46 DO6 (strapping)
GPIO15 W5500 RST     GPIO47 (未分配)
GPIO16 DO2/AO2       GPIO48 DO7
GPIO17 RS485_1 TX    (UART2)
GPIO18 RS485_1 RX
GPIO19 DI0           (USB Serial/JTAG, 可作 GPIO)
GPIO20 DI1           (USB Serial/JTAG, 可作 GPIO)
GPIO21 DI2 / F3-F4 I2C SDA
```

---

## 4. 通信接口配置

### 4.1 Modbus RTU 主站（UART1）

| 参数 | 默认值 | 说明 |
|------|--------|------|
| 串口 | UART1 (GPIO40/41/42) | DE/RE 自动控制 |
| 波特率 | 9600 | 可配置为 115200 等 |
| 数据位 | 8 | |
| 停止位 | 1 | |
| 校验 | None | 可选 None/Odd/Even |
| 从站地址范围 | 1-247 | 轮询表硬编码 |
| 轮询周期 | 200ms | |
| 超时 | 500ms | 从站无响应超时 |
| 支持功能码 | 0x01/0x02/0x03/0x04 | 读线圈/离散输入/保持/输入寄存器 |

### 4.2 Modbus RTU 从站（UART2）

| 参数 | 默认值 | 说明 |
|------|--------|------|
| 串口 | UART2 (GPIO17/18/7) | DE/RE 自动控制 |
| 波特率 | 9600 | 可配置 |
| 数据位 | 8 | |
| 停止位 | 1 | |
| 校验 | None | 可选 |
| 本机地址 | 1 | 可配置 1-247 |
| 帧间静默 | 3.5 字符时间（9600bps ≈ 4ms） | |
| 支持功能码 | 0x01/0x02/0x03/0x04/0x05/0x06/0x0F/0x10 | |
| 广播支持 | 地址 0 广播执行不返回 | |

### 4.3 Modbus TCP Server

| 参数 | 默认值 | 说明 |
|------|--------|------|
| 监听端口 | 502 | 标准 Modbus TCP 端口 |
| 最大连接数 | 4 | 第 5 个连接被拒绝 |
| MBAP 协议 | 是 | Unit ID = 1 |
| 支持功能码 | 0x01/0x02/0x03/0x04/0x05/0x06/0x0F/0x10 | 与 RTU 从站一致 |
| 连接模式 | 每连接独立线程 | `mb-tcp-conn-{n}` |
| 监听超时 | 1s（阈值 10） | 阻塞型任务允许较长时间无活动 |

### 4.4 BLE Mesh

| 参数 | 默认值 | 说明 |
|------|--------|------|
| 协议栈 | Bluedroid | ESP-IDF 内置 |
| Mesh 模型 | Generic OnOff Server/Client | 控制 DO0 |
| 配网方式 | 静态 OOB | Provisioner 配网 |
| Proxy 节点 | 启用 | 手机远离 mesh 时通过设备转发 |
| 心跳周期 | 60s | `mesh-heartbeat` 任务 |
| 默认 BLE 名称 | "GW-S3" | 可通过 AT+CFGBTNAME 修改 |
| BLE Mesh 使能 | 默认启用 | 可通过 AT+CFGMESH=0 关闭 |
| 默认 MAC | 从 `esp_read_mac(ESP_MAC_BT)` 读取 | |

**Mesh ↔ 总线交互**：
- 收到 OnOff Set → 更新 `BUS.do_.bits` bit0 → io 任务刷新 GPIO
- DO 状态变化 → 通过 OnOff Status 上报

### 4.5 BLE GATT AT 命令通道

| 参数 | 值 | 说明 |
|------|----|------|
| Service UUID | 0xFF01 | 自定义 GATT 服务 |
| RX Characteristic | 0xFF02（Write） | 主机 → 设备，写入 AT 命令 |
| TX Characteristic | 0xFF03（Notify） | 设备 → 主机，推送 AT 响应 |
| 单帧最大长度 | 512 字节 binary（heapless::Vec 零分配） | |
| 处理周期 | 10ms 轮询输入缓冲区 | |

### 4.6 Wi-Fi（可选，需 `--features wifi`）

| 参数 | 默认值 | 说明 |
|------|--------|------|
| 模式 | Station | 连接上游 AP，作为以太网故障备份链路 |
| SSID/Password | 硬编码在 `config::wifi` | TODO: 从 SystemConfig 动态加载 |
| 共存 | 启用 COEX 软件分时调度 | 与 BLE 共用 2.4GHz 射频 |
| 启动失败处理 | 仅记日志，不阻断主流程 | 备份链路可缺失 |

启用方式：`cargo build --release --features wifi`

### 4.7 网络默认参数

| 参数 | 默认值 |
|------|--------|
| DHCP | 启用 |
| 静态 IP | 192.168.1.100 |
| 子网掩码 | 255.255.255.0 |
| 网关 | 192.168.1.1 |
| DNS | 192.168.1.1 |

可通过 AT+CFGIP 或 Modbus 0x0224-0x022A 修改。

---

## 5. Modbus 寄存器映射表

### 5.1 寄存器类型总览

| 类型 | 功能码 | 地址范围 | 说明 |
|------|--------|---------|------|
| 线圈 Coil | 0x01 / 0x05 / 0x0F | 0x0000-0x000F | DO 输出（8/16 路） |
| 离散输入 Disc | 0x02 | 0x0000-0x002F | DI 输入（8/16/48 路） |
| 输入寄存器 | 0x04 | 0x0000-0x0005 / 0x0010-0x0015 | AI 原始 / AI 工程量 |
| 保持寄存器 | 0x03 / 0x06 / 0x10 | 0x0000-0x0003 / 0x0100-0x0106 / 0x0107-0x010F / 0x0200-0x025F / 0x4000-0x45E1 | AO 输出 / 系统 / OTA / 配置 / 协议存储 |

### 5.2 线圈表（Coil, FC=0x01/0x05/0x0F）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x0000-0x0007 | DO0-DO7 | RW | 默认版本 8 路 DO 输出 |
| 0x0008-0x000F | DO8-DO15 | RW | 仅 F3/F4 版本（共 16 路） |

> F4 版本 DO 仍为 16 路（与 F3 一致）。

### 5.3 离散输入表（Disc, FC=0x02）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x0000-0x0007 | DI0-DI7 | RO | 默认版本 8 路 DI |
| 0x0008-0x000F | DI8-DI15 | RO | F3 版本 16 路（共 16） |
| 0x0010-0x002F | DI16-DI47 | RO | F4 版本扩展至 48 路 |

### 5.4 输入寄存器表（FC=0x04）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x0000-0x0005 | AI0-AI5 raw | RO | ADC 原始值（0-4095，12-bit） |
| 0x0010-0x0015 | AI0-AI5 scaled | RO | 工程量（4-20mA 转换，单位 mA×1000，范围 4000-20000） |

> 转换公式：`scaled = 4000 + avg * 16000 / 4095`（结果范围 4000-20000，对应 4-20mA）

### 5.5 保持寄存器表（FC=0x03/0x06/0x10）

#### 5.5.1 AO 输出区（0x0000-0x0003）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x0000 | AO0 | RW | 工程量（单位 mA×1000，4000-20000） |
| 0x0001 | AO1 | RW | 同上 |
| 0x0002 | AO2 | RW | 同上 |
| 0x0003 | AO3 | RW | 同上 |

#### 5.5.2 系统寄存器区（0x0100-0x0106）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x0100 | 固件版本 | RO | BCD 编码，如 0x0102 = v1.02 |
| 0x0101 | 运行时长（秒） | RO | uptime，每秒自增 |
| 0x0102 | 复位计数 | RO | 启动时从 NVS 读取并 +1 |
| 0x0103 | 触发复位 | WO | 写 0xA5A5 触发设备复位 |
| 0x0104 | 复位原因 | RO | `esp_reset_reason_t` 值 |
| 0x0105 | 任务健康位图 | RO | bit=1 表示对应任务停滞 |
| 0x0106 | 日志级别 | RW | 0=Err 1=Warn 2=Info 3=Debug 4=Trace |

#### 5.5.3 OTA 升级区（0x0107-0x010F）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x0107 | OTA 状态 | RO | 0=空闲 1=接收中 2=完成待重启 3=校验失败 4=空间不足 5=已中止 |
| 0x0108 | 升级包总大小（低 16 位） | RW | 字节数，与 0x0109 组合为 32 位 |
| 0x0109 | 升级包总大小（高 16 位） | RW | 字节数 |
| 0x010A | 已写入字节数（低 16 位） | RO | 进度反馈 |
| 0x010B | 已写入字节数（高 16 位） | RO | 进度反馈 |
| 0x010C | 开始升级 | WO | 写 0x0B0A → 开始升级（使用 TOTAL 字段值） |
| 0x010D | 结束升级 | WO | 写 0x0E0D → 结束升级 + 设置启动分区 |
| 0x010E | 中止升级 | WO | 写 0x0AB0 → 中止升级 |
| 0x010F | 重启应用新固件 | WO | 写 0x0F0E → 重启应用新固件 |

#### 5.5.4 系统配置区（0x0200-0x025F）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x0200-0x020F | SN 号 | RW | ASCII 32 字符（16 字） |
| 0x0210-0x0217 | 设备名称 | RW | ASCII 16 字符（8 字） |
| 0x0218 | 硬件版本 | RO | BCD 编码 |
| 0x0219 | 固件版本 | RO | BCD 编码 |
| 0x021A | 配置版本 | RO | 每次修改自增 |
| 0x021B | 应用配置 | WO | 写 0xB5B5 → 应用配置（持久化+生效） |
| 0x021C | 恢复默认 | WO | 写 0xD5D5 → 恢复默认配置 |
| 0x0220-0x0222 | 以太网 MAC | RO | 6 字节（3 字） |
| 0x0223 | DHCP | RW | 0=静态 IP，1=DHCP |
| 0x0224-0x0225 | IP 地址 | RW | 4 字节（2 字） |
| 0x0226-0x0227 | 子网掩码 | RW | 4 字节（2 字） |
| 0x0228-0x0229 | 网关 | RW | 4 字节（2 字） |
| 0x022A-0x022B | DNS | RW | 4 字节（2 字） |
| 0x0230-0x0232 | BLE MAC | RO | 6 字节（3 字） |
| 0x0233-0x0236 | BLE 名称 | RW | ASCII 8 字符（4 字） |
| 0x0237 | BLE Mesh 使能 | RW | 0=禁用，1=启用 |
| 0x0240-0x024F | RS485 #0 配置 | RW | 16 字（见下表） |
| 0x0250-0x025F | RS485 #1 配置 | RW | 16 字（同 0x0240） |

**RS485 通道内偏移**（每通道 16 字，仅前 6 字有效）：

| 偏移 | 字段 | 默认值 | 说明 |
|------|------|--------|------|
| +0 | 波特率 | 96 | ÷100，9600 → 96，115200 → 1152 |
| +1 | 数据位 | 8 | 7 或 8 |
| +2 | 停止位 | 1 | 1 或 2 |
| +3 | 校验 | 0 | 0=None 1=Odd 2=Even |
| +4 | 从站地址 | 1 | 0=主站模式，1-247=从站 |
| +5 | 模式 | 1 | 0=Master 1=Slave 2=Gateway |
| +6 ~ +15 | 保留 | 0 | 保留字段 |

#### 5.5.5 协议存储区（0x4000-0x45E1）

| 地址 | 名称 | 读写 | 说明 |
|------|------|------|------|
| 0x4000-0x45DB | 协议数据 | RW | 用户自定义协议数据，1500 个 U16（3000 字节） |
| 0x45DC | 触发 COMMIT | WO | 写 0xC5C5 → 持久化到 NVS |
| 0x45DD | 触发 RELOAD | WO | 写 0xA5A5 → 从 NVS 重载到 RAM |
| 0x45DE | 协议版本 | RW | 用户自定义协议版本号 |
| 0x45DF | 协议长度 | RW | 用户写入的有效协议长度（U16 数） |
| 0x45E0 | 状态 | RO | 0=空闲 1=写入中 2=加载中 3=校验失败 |
| 0x45E1 | 魔数 | RO | 固定 0x4757（"GW"），用于校验 NVS 数据 |

### 5.6 Modbus 异常码

| 异常码 | 含义 | 触发条件 |
|--------|------|---------|
| 0x01 | 非法功能码 | 不支持的功能码 |
| 0x02 | 非法数据地址 | 访问超出范围的地址 |
| 0x03 | 非法数据值 | 写入的值不在合法范围 |
| 0x04 | 从站设备故障 | 内部处理错误 |
| 0x05 | 确认 | 操作需要较长时间 |

### 5.7 Modbus 寄存器读写示例

#### 示例 1：读取固件版本与运行时长（Modbus TCP / Python pymodbus）

```python
from pymodbus.client import ModbusTcpClient

c = ModbusTcpClient('192.168.1.100', port=502)
assert c.connect()

# 读固件版本 (0x0100)
r = c.read_holding_registers(address=0x0100, count=1)
print('Firmware:', hex(r.registers[0]))   # 0x100 → v1.00

# 读运行时长 (0x0101)
r = c.read_holding_registers(address=0x0101, count=1)
print('Uptime:', r.registers[0], 's')

c.close()
```

#### 示例 2：通过写线圈控制 DO0

```python
# 写线圈 0x0000 (DO0) = ON
c.write_coil(address=0x0000, value=True)
# 读回 DO0~DO7 状态
r = c.read_coils(address=0x0000, count=8)
print('DO0-DO7:', r.bits)
```

#### 示例 3：触发设备复位

```python
# 写保持寄存器 0x0103 = 0xA5A5 触发复位
c.write_register(address=0x0103, value=0xA5A5)
```

#### 示例 4：协议存储区写入并提交

```python
# 写入协议数据 (0x4000 起, 10 个 U16)
data = [0x0001, 0x0002, 0x0003, 0x0004, 0x0005,
        0x0006, 0x0007, 0x0008, 0x0009, 0x000A]
c.write_registers(address=0x4000, values=data)

# 写协议长度 (0x45DF)
c.write_register(address=0x45DF, value=10)

# 写 0x45DC = 0xC5C5 触发 COMMIT
c.write_register(address=0x45DC, value=0xC5C5)
```

---

## 6. BLE AT 命令手册

### 6.1 GATT 服务结构

| 项 | UUID | 属性 |
|----|------|------|
| Service | 0xFF01 | 自定义 GATT 服务 |
| RX Characteristic | 0xFF02 | Write（主机 → 设备，AT 命令） |
| TX Characteristic | 0xFF03 | Notify（设备 → 主机，AT 响应） |

### 6.2 AT 命令响应格式

- 成功：`OK <data>` 或 `OK`
- 失败：`ERROR <code>: <msg>`
- 常用错误码：

| 错误码 | 含义 |
|--------|------|
| 1 | 未知命令 |
| 2 | 参数错误 / 用法错误 |
| 10 | 缺少必需参数 |
| 20 | 内部错误 |
| 30 | 参数越界 |
| 40 | 状态不允许（如 OTA 状态机错误） |

### 6.3 协议区 AT 命令

#### AT+READ - 读协议区地址

**格式**：`AT+READ=<addr>`

**示例**：
```
> AT+READ=0
< OK 0x0001,1
```

#### AT+WRITE - 写协议区地址

**格式**：`AT+WRITE=<addr>,<value>`

**示例**：
```
> AT+WRITE=0,0x1234
< OK
```

#### AT+BULKR - 批量读协议区

**格式**：`AT+BULKR=<start>,<len>`

**示例**：
```
> AT+BULKR=0,4
< OK 0x0001,0x0002,0x0003,0x0004
```

#### AT+BULKW - 批量写协议区

**格式**：`AT+BULKW=<start>,<v1>,<v2>,...`

**示例**：
```
> AT+BULKW=0,0x0001,0x0002,0x0003,0x0004
< OK 4 words
```

#### AT+COMMIT - 同步提交到 NVS

**格式**：`AT+COMMIT`

**示例**：
```
> AT+COMMIT
< OK committed
```

#### AT+RELOAD - 从 NVS 重载

**格式**：`AT+RELOAD`

**示例**：
```
> AT+RELOAD
< OK reloaded
```

#### AT+INFO - 查询存储区信息

**格式**：`AT+INFO`

**示例**：
```
> AT+INFO
< OK cap=1500,ver=1,len=10
```

#### AT+STATUS - 查询系统状态

**格式**：`AT+STATUS`

**示例**：
```
> AT+STATUS
< OK uptime=12345,fw=0x0100,di=0x00ff,do_=0x0001
```

#### AT+VERSION - 查询固件版本

**格式**：`AT+VERSION`

**示例**：
```
> AT+VERSION
< OK esp32s3-iot-gateway v0.1.0
```

#### AT+RESET - 触发复位

**格式**：`AT+RESET`

**示例**：
```
> AT+RESET
< OK resetting in 100ms
```

### 6.4 系统配置 AT 命令

#### AT+CFGSN - 设置/读 SN

**格式**：`AT+CFGSN=<sn>`（写） / `AT+CFGSN`（读）

**示例**：
```
> AT+CFGSN=ESP32S3-GW-0001
< OK
> AT+CFGSN
< OK ESP32S3-GW-0001
```

> SN 最长 32 字符（ASCII）。

#### AT+CFGNAME - 设置/读设备名称

**格式**：`AT+CFGNAME=<name>`（写） / `AT+CFGNAME`（读）

**示例**：
```
> AT+CFGNAME=MyGateway
< OK
```

> 名称最长 16 字符（ASCII）。

#### AT+CFGIP - 设置/读网络配置

**格式**：`AT+CFGIP=<ip>,<mask>,<gw>[,<dns>]`（写） / `AT+CFGIP`（读）

**示例**：
```
> AT+CFGIP=192.168.1.100,255.255.255.0,192.168.1.1,192.168.1.1
< OK
> AT+CFGIP
< OK ip=192.168.1.100,mask=255.255.255.0,gw=192.168.1.1,dns=192.168.1.1
```

#### AT+CFGDHCP - 启用/禁用 DHCP

**格式**：`AT+CFGDHCP=<0|1>`

**示例**：
```
> AT+CFGDHCP=0
< OK dhcp=0
```

#### AT+CFGMAC - 读以太网 MAC

**格式**：`AT+CFGMAC`（只读）

**示例**：
```
> AT+CFGMAC
< OK AA:BB:CC:DD:EE:FF
```

#### AT+CFGBTMAC - 读蓝牙 MAC

**格式**：`AT+CFGBTMAC`（只读）

**示例**：
```
> AT+CFGBTMAC
< OK AA:BB:CC:DD:EE:FF
```

#### AT+CFGBTNAME - 设置/读 BLE 名称

**格式**：`AT+CFGBTNAME=<name>`（写） / `AT+CFGBTNAME`（读）

**示例**：
```
> AT+CFGBTNAME=GW-S3
< OK
```

> BLE 名称最长 8 字符（ASCII）。

#### AT+CFGMESH - 启用/禁用 BLE Mesh

**格式**：`AT+CFGMESH=<0|1>`

**示例**：
```
> AT+CFGMESH=1
< OK ble_mesh=1
```

#### AT+CFG485 - 配置/读 RS485

**格式**（写）：`AT+CFG485=<idx>,<baud>,<data>,<stop>,<parity>,<slave>,<mode>`

**格式**（读）：`AT+CFG485=<idx>`

**参数**：
- `idx`：0 = RS485 #0（UART1），1 = RS485 #1（UART2）
- `baud`：波特率（如 9600、115200）
- `data`：数据位（7/8）
- `stop`：停止位（1/2）
- `parity`：0=None 1=Odd 2=Even
- `slave`：从站地址（0=主站模式，1-247=从站）
- `mode`：0=Master 1=Slave 2=Gateway

**示例**：
```
> AT+CFG485=1,9600,8,1,0,1,1
< OK
> AT+CFG485=1
< OK idx=1,baud=9600,data=8,stop=1,parity=0,slave=1,mode=1
```

#### AT+CFGAPPLY - 应用配置

**格式**：`AT+CFGAPPLY`

**说明**：持久化配置到 NVS 并应用到运行时。

**示例**：
```
> AT+CFGAPPLY
< OK applied (restart to take effect)
```

> 部分配置（如网络、RS485）需重启后生效。

#### AT+CFGRESET - 恢复默认配置

**格式**：`AT+CFGRESET`

**示例**：
```
> AT+CFGRESET
< OK reset to defaults
```

#### AT+CFGINFO - 列出所有配置

**格式**：`AT+CFGINFO`

**示例**：
```
> AT+CFGINFO
< OK sn=ESP32S3-GW-0001,name=MyGateway,ip=192.168.1.100,mask=255.255.255.0,gw=192.168.1.1,dhcp=0,ble=GW-S3,mesh=1,rs485_0=9600/8N1/slave=1,rs485_1=9600/8N1/slave=1
```

#### AT+CFGREAD - 按 Modbus 地址读 U16

**格式**：`AT+CFGREAD=<addr>`

**说明**：按 Modbus 保持寄存器地址读取一个 U16（覆盖整个配置区）。

**示例**：
```
> AT+CFGREAD=0x0223
< OK 547,0x0223   # 547=0x0223 地址处的值
```

#### AT+CFGWRITE - 按 Modbus 地址写 U16

**格式**：`AT+CFGWRITE=<addr>,<value>`

**说明**：按 Modbus 保持寄存器地址写入一个 U16。若写入触发 APPLY（如 0x021B=0xB5B5），返回 `OK apply requested`。

**示例**：
```
> AT+CFGWRITE=0x0223,0
< OK
> AT+CFGWRITE=0x021B,0xB5B5
< OK apply requested
```

### 6.5 OTA AT 命令

详见第 7 章 OTA 升级流程。

---

## 7. OTA 升级流程

### 7.1 概述

OTA（Over-The-Air）升级通过 BLE GATT AT 通道或 Modbus 寄存器触发，将新固件写入备用 OTA 分区，重启后切换到新固件运行。

**分区表**（8MB Flash）：
- `factory`：3MB（出厂固件）
- `ota_0`：2.25MB（备用 OTA 分区 1）
- `ota_1`：2.25MB（备用 OTA 分区 2）
- `otadata`：记录当前启动分区
- 启用 `CONFIG_APP_ROLLBACK_ENABLE`，OTA 失败可回滚到原分区

### 7.2 OTA 状态机

```
                ┌──────────┐
                │   Idle   │ ←─────────────┐
                └────┬─────┘               │
                     │ BEGIN                │
                     ▼                      │
                ┌──────────┐                │
       ┌─────── │ Receiving │              │
       │        └────┬─────┘                │
       │             │ END                  │
       │             ▼                       │ ABORT / 失败
       │        ┌────────────────────┐      │
       │        │ DonePendingReboot  │      │
       │        └────┬───────────────┘      │
       │             │ REBOOT               │
       │             ▼                       │
       │        重启 + 切换到新分区          │
       │             │                       │
       └─────────────┴───────────────────────┘
```

**OTA 状态值**（寄存器 0x0107 / AT+OTA=STATUS 返回字段 `status`）：

| 状态值 | 含义 |
|--------|------|
| 0 | 空闲（Idle） |
| 1 | 接收中（Receiving） |
| 2 | 完成待重启（DonePendingReboot） |
| 3 | 校验失败 |
| 4 | 空间不足 |
| 5 | 已中止（Aborted） |

### 7.3 BLE AT 触发 OTA（推荐）

#### 步骤 1：开始升级

**命令**：`AT+OTA=BEGIN,<total_size>`

```
> AT+OTA=BEGIN,262144
< OK begin total=262144
```

参数 `total_size` 为固件总字节数（与 `ota_0` 分区 2.25MB 容量匹配，即 ≤ 2359296 字节）。

#### 步骤 2：分块写入固件

**命令**：`AT+OTA=WRITE,<hex_chunk>`

```
> AT+OTA=WRITE,E9F0000000000000...
< OK 512
```

- `hex_chunk` 为 hex 编码的二进制数据（每 2 个 hex 字符 = 1 字节）
- 单帧最大 512 字节 binary（即 1024 个 hex 字符）
- 设备返回 `OK <written_bytes>` 表示本帧已写入字节数
- 多次调用直到所有数据写入完成

#### 步骤 3：结束升级

**命令**：`AT+OTA=END`

```
> AT+OTA=END
< OK ended, send AT+OTA=REBOOT to apply
```

END 命令完成校验并设置启动分区。校验失败返回 `ERROR 20: ...`，状态变为 3（校验失败）。

#### 步骤 4：重启应用新固件

**命令**：`AT+OTA=REBOOT`

```
> AT+OTA=REBOOT
< OK rebooting
```

设备将在 100ms 后重启，启动到新固件分区。

#### 查询状态

**命令**：`AT+OTA=STATUS`

```
> AT+OTA=STATUS
< OK status=1,written=131072,total=262144
```

#### 中止升级

**命令**：`AT+OTA=ABORT`

```
> AT+OTA=ABORT
< OK aborted
```

### 7.4 Modbus 触发 OTA（仅状态查询 + 重启）

Modbus 接口**不支持直接写入固件数据**（数据量较大），仅用于查询状态和触发重启。固件数据必须通过 BLE GATT 写入。

**流程**：
1. 通过 BLE AT 写入固件（见 7.3 节步骤 1-3）
2. 通过 Modbus 读 0x0107（状态）、0x010A-0x010B（已写入字节数）、0x0108-0x0109（总大小）查询进度
3. 状态变为 2（DonePendingReboot）后，可写 0x010F = 0x0F0E 触发重启

**Modbus OTA 寄存器触发命令**：

| 操作 | 寄存器 | 写入值 |
|------|--------|--------|
| 开始升级（使用 TOTAL 字段） | 0x010C | 0x0B0A |
| 结束升级 | 0x010D | 0x0E0D |
| 中止升级 | 0x010E | 0x0AB0 |
| 重启应用新固件 | 0x010F | 0x0F0E |

### 7.5 OTA 注意事项

1. **不要中途断电**：升级过程中断电可能导致 OTA 分区损坏，重启后回滚到原分区。
2. **校验固件完整性**：END 命令会自动校验，失败时状态变为 3。
3. **空间限制**：单次升级包大小 ≤ `ota_0` 分区容量（2.25MB = 2359296 字节）。
4. **状态机约束**：`AT+OTA=REBOOT` 仅在状态为 2（DonePendingReboot）时生效，否则返回 `ERROR 40: not done`。
5. **回滚保护**：sdkconfig 启用 `CONFIG_APP_ROLLBACK_ENABLE`，新固件首次启动失败可自动回滚到原分区。
6. **建议保持 BLE 连接**：升级期间保持 BLE 连接稳定，避免连接断开导致升级失败。

### 7.6 OTA 升级 Python 示例脚本（伪代码）

```python
import binascii

# 假设 ble_client 已连接并写入 0xFF02 特征
def send_at(cmd):
    ble_client.write(0xFF02, cmd.encode() + b'\n')
    return wait_notify_response()

# 1. 读取固件文件
with open('firmware.bin', 'rb') as f:
    fw = f.read()
total = len(fw)

# 2. 开始升级
resp = send_at(f'AT+OTA=BEGIN,{total}')
assert 'OK' in resp

# 3. 分块写入（每块 512 字节）
CHUNK = 512
for i in range(0, total, CHUNK):
    chunk = fw[i:i+CHUNK]
    hex_str = binascii.hexlify(chunk).decode().upper()
    resp = send_at(f'AT+OTA=WRITE,{hex_str}')
    assert f'OK {len(chunk)}' in resp
    print(f'Progress: {i+len(chunk)}/{total}')

# 4. 结束升级
resp = send_at('AT+OTA=END')
assert 'OK' in resp

# 5. 重启
resp = send_at('AT+OTA=REBOOT')
print('OTA upgrade done, device rebooting')
```

---

## 8. 故障排查

### 8.1 常见问题

#### 8.1.1 设备无法启动 / 启动失败

**现象**：串口无启动 banner，或反复重启。

**排查**：
1. 检查供电电压是否稳定（5V DC，纹波 ≤ 100mV）
2. 用串口工具连接 UART0（GPIO43/44，115200-8-N-1），查看启动日志
3. 检查复位计数（启动日志 `[main] reset reason=X, count=N`）
4. 若 `count` 快速递增，可能为 panic 复位，查看 backtrace

#### 8.1.2 以太网无法获取 IP

**现象**：`[eth] got IP` 日志未出现，或 ping 不通。

**排查**：
1. 检查网线是否插好（W5500 INT 引脚电平变化）
2. 检查 DHCP 服务器是否可用（可改为静态 IP 测试）
3. 查看 W5500 初始化日志：`[eth] W5500 driver installed and started`
4. 拔网线 15s 后设备会自动复位（连续 3 次心跳失败）

#### 8.1.3 Modbus RTU 通信失败

**现象**：从站无响应，或 CRC 错误。

**排查**：
1. 检查 RS485 接线（A/B 是否接反，GND 是否共地）
2. 检查波特率、数据位、停止位、校验位是否一致
3. 检查从站地址是否匹配
4. 用示波器观察 RS485 总线波形
5. 从站无响应超时 500ms 后会记录 `[mb-rtu-master] timeout` 日志

#### 8.1.4 Modbus TCP 连接被拒绝

**现象**：客户端无法连接到 502 端口。

**排查**：
1. 检查设备 IP 是否正确（ping 测试）
2. 检查是否已达到最大连接数（最多 4 个并发连接）
3. 第 5 个连接会被拒绝，需等待已有连接断开

#### 8.1.5 BLE Mesh 配网失败

**现象**：扫描不到设备，或配网失败。

**排查**：
1. 检查 `AT+CFGMESH` 是否为 1（启用 BLE Mesh）
2. 检查 BLE 名称是否冲突
3. 用 ESP-BLE-Mesh App 或 nRF Mesh 重新配网
4. 查看配网日志 `[blemesh] provisioning ...`

#### 8.1.6 OTA 升级失败

**现象**：`AT+OTA=END` 返回 `ERROR 20`。

**排查**：
1. 检查固件大小是否超过 2.25MB（`ota_0` 分区容量）
2. 检查 BLE 连接是否在升级中途断开
3. 通过 `AT+OTA=STATUS` 查看状态（3=校验失败，4=空间不足）
4. 用 `AT+OTA=ABORT` 重置状态后重新升级

#### 8.1.7 任务停滞 / 看门狗复位

**现象**：设备周期性复位，复位计数递增。

**排查**：
1. 通过 Modbus 读 0x0105（任务健康位图），bit=1 表示对应任务停滞
2. 查看串口日志中的 `[health] task stalled: <name>` 警告
3. 检查任务是否被阻塞（如 I2C 总线死锁、UART 缓冲区满）

#### 8.1.8 I2C MCP23017 通信失败（仅 F3/F4）

**现象**：DI 全部读回 0 或 0xFFFF，DO 输出无效。

**排查**：
1. 检查 I2C 接线（SDA=GPIO21，SCL=GPIO33）
2. 检查 MCP23017 地址引脚（A0/A1/A2）配置是否正确
3. 检查上拉电阻（MCP23017 内部上拉已启用，外部可不接）
4. 用 I2C 扫描工具检查设备是否在线

### 8.2 日志解读

#### 8.2.1 日志级别

| 级别 | 值 | 用途 |
|------|----|----|
| Error | 0 | 错误事件（必须排查） |
| Warn | 1 | 警告事件（建议排查） |
| Info | 2 | 信息事件（默认级别） |
| Debug | 3 | 调试信息（开发用） |
| Trace | 4 | 详细跟踪（仅深度调试用） |

通过 Modbus 写 0x0106 或 BLE AT 命令动态调节。默认 Info 级别。

#### 8.2.2 启动日志预期输出

```
================================================
esp32s3-iot-gateway v0.1.0
ESP32-S3R2 IoT Gateway starting...
================================================
[main] starting device protocol store...
[main] device init ok
[main] reset reason=1, count=1
[main] starting ethernet (W5500)...
[eth] initializing W5500 over SPI2...
[eth] resetting W5500 via GPIO15...
[eth] W5500 driver installed and started, eth_handle=0x...
[main] starting ble mesh...
[main] starting ble at command channel...
[main] starting io scan task...
[main] starting ai/ao task...
[main] starting modbus rtu...
[main] starting modbus tcp server...
[main] entering main loop (period=100ms)
[main] uptime=1s tick=10
```

#### 8.2.3 关键日志关键字

| 日志关键字 | 含义 | 处理建议 |
|-----------|------|---------|
| `[eth] got IP: x.x.x.x` | 以太网获取 IP 成功 | 正常 |
| `[eth] heartbeat failed` | 以太网心跳失败 | 检查网络连接 |
| `[mb-rtu-master] timeout` | RTU 主站轮询超时 | 检查从站设备和接线 |
| `[mb-rtu-slave] CRC error` | RTU 从站 CRC 校验失败 | 检查总线干扰 |
| `[mb-tcp] connection rejected` | TCP 连接被拒绝（满载） | 等待已有连接断开 |
| `[health] task stalled: <name>` | 任务心跳停滞 | 排查任务阻塞原因 |
| `[ota] state: Receiving` | OTA 升级接收中 | 正常 |
| `[ota] state: DonePendingReboot` | OTA 升级完成待重启 | 触发 REBOOT |
| `[ota] verify failed` | OTA 校验失败 | 检查固件完整性 |
| `[device] proto commit ok` | 协议数据持久化成功 | 正常 |
| `[device] proto magic mismatch` | NVS 魔数不匹配，使用默认值 | 协议数据未持久化或损坏 |
| `RUST PANIC: ...` | Rust panic | 查看 backtrace，定位崩溃点 |

#### 8.2.4 Panic 日志解读

```
RUST PANIC: index out of bounds: the len is 48 but the index is 50
  at src/io/di.rs:120:23
backtrace:
  0x42012345 - <core::panicking::panic_fmt>
  0x42012380 - <io::di::scan>
  ...
```

- 第 1 行：panic 原因（数组越界，长度 48 但访问索引 50）
- 第 2 行：源文件 + 行号（`src/io/di.rs:120:23`）
- backtrace：调用栈，由于 release 构建 `strip = true`，地址需用 `xtensa-esp32s3-elf-addr2line` 在 host 上解析：

```bash
xtensa-esp32s3-elf-addr2line -e target/xtensaespidf/release/gateway 0x42012380
```

### 8.3 复位原因（寄存器 0x0104）

参考 `esp_reset_reason_t`：

| 值 | 含义 |
|----|------|
| 1 | 上电复位（Power on） |
| 3 | 软件复位（SW reset） |
| 4 | 看门狗复位（OWDT/TWDT） |
| 5 | 深睡复位（Deep sleep awake） |
| 12 | 任务看门狗复位（Task WDT） |
| 15 | RTC 看门狗复位（RTC WDT） |
| 16 | OTA 失败回滚（Brownout / 不足电压） |

### 8.4 调试工具推荐

| 工具 | 用途 |
|------|------|
| [Modbus Poll](https://www.modbustools.com/modbus_poll.html) | Windows Modbus 主站仿真 |
| [pymodbus](https://pypi.org/project/pymodbus/) | Python Modbus 客户端库 |
| [ESP-BLE-Mesh App](https://www.espressif.com/) | BLE Mesh 配网（iOS/Android） |
| [nRF Mesh](https://www.nordicsemi.com/Products/Development-tools/nrf-mesh) | BLE Mesh 配网（跨平台） |
| [nRF Connect for Mobile](https://www.nordicsemi.com/) | BLE GATT 调试（写 AT 命令） |
| `minicom` / `screen` | 串口监控（UART0 日志） |
| `xtensa-esp32s3-elf-addr2line` | 解析 panic backtrace |

### 8.5 联系技术支持

如以上排查仍无法解决问题，请准备以下信息后联系技术支持：

1. 设备 SN（`AT+CFGSN` 或 Modbus 0x0200）
2. 固件版本（`AT+VERSION` 或 Modbus 0x0100）
3. 硬件版本（Default / F3 / F4）
4. 完整启动日志（串口 UART0 输出）
5. 复位原因（Modbus 0x0104）和复位计数（Modbus 0x0102）
6. 任务健康位图（Modbus 0x0105，若有任务停滞）
7. 复现步骤（如有）

---

## 附录 A：寄存器速查表

| 寄存器 | 类型 | 读写 | 含义 |
|--------|------|------|------|
| 0x0000-0x000F | Coil | RW | DO 输出（8/16 路） |
| 0x0000-0x002F | Disc | RO | DI 输入（8/16/48 路） |
| 0x0000-0x0005 | InReg | RO | AI 原始 ADC（6 路） |
| 0x0010-0x0015 | InReg | RO | AI 工程量（6 路） |
| 0x0000-0x0003 | HoldReg | RW | AO 输出（4 路） |
| 0x0100 | HoldReg | RO | 固件版本 |
| 0x0101 | HoldReg | RO | 运行时长（秒） |
| 0x0102 | HoldReg | RO | 复位计数 |
| 0x0103 | HoldReg | WO | 触发复位（写 0xA5A5） |
| 0x0104 | HoldReg | RO | 复位原因 |
| 0x0105 | HoldReg | RO | 任务健康位图 |
| 0x0106 | HoldReg | RW | 日志级别 |
| 0x0107-0x010F | HoldReg | RW/WO | OTA 升级区 |
| 0x0200-0x025F | HoldReg | RW | 系统配置区 |
| 0x4000-0x45DB | HoldReg | RW | 协议数据区（1500 U16） |
| 0x45DC | HoldReg | WO | COMMIT（写 0xC5C5） |
| 0x45DD | HoldReg | WO | RELOAD（写 0xA5A5） |
| 0x45DE-0x45E1 | HoldReg | RW/RO | 协议版本/长度/状态/魔数 |

## 附录 B：AT 命令速查表

| 命令 | 用途 |
|------|------|
| AT+READ / AT+WRITE | 读/写单个协议区地址 |
| AT+BULKR / AT+BULKW | 批量读/写协议区 |
| AT+COMMIT / AT+RELOAD | 协议区持久化/重载 |
| AT+INFO / AT+STATUS / AT+VERSION | 查询信息 |
| AT+RESET | 触发复位 |
| AT+CFGSN / AT+CFGNAME / AT+CFGBTNAME | SN/设备名/BLE 名 |
| AT+CFGIP / AT+CFGDHCP / AT+CFGMAC | 网络配置 |
| AT+CFGBTMAC / AT+CFGMESH | 蓝牙配置 |
| AT+CFG485 | RS485 配置 |
| AT+CFGAPPLY / AT+CFGRESET / AT+CFGINFO | 配置应用/恢复/查询 |
| AT+CFGREAD / AT+CFGWRITE | 按 Modbus 地址读写 |
| AT+OTA=BEGIN/WRITE/END/ABORT/STATUS/REBOOT | OTA 升级 |
