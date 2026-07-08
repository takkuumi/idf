# 架构设计

## 硬件平台

- **主控**：ESP32-S3R8 (Xtensa LX7 双核 32-bit 240MHz, 512KB SRAM, **8MB Octal SPI PSRAM**)
  - 内置 Wi-Fi 802.11 b/g/n + BLE 5.0 + Bluetooth Mesh
  - 3 个 UART (UART0/1/2), 4 个 SPI (SPI0/1 Flash/PSRAM, SPI2/3 外设), 2 个 ADC, 8 通道 LEDC PWM
  - 45 个 GPIO (GPIO0~48, GPIO26~32 被 Octal SPI Flash/PSRAM 占用)
- **以太网**：WIZnet W5500 (硬wired TCP/IP + 10/100 MAC/PHY, 32KB 缓冲, 8 socket, SPI 接口)
- **Flash**：8MB Octal SPI (分区表见 `partitions.csv`)

## 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                          应用层 (main.rs)                    │
│              主循环 / 复位请求 / 状态上报 (100ms)             │
└──────────────────────────┬──────────────────────────────────┘
                           │
        ┌──────────────────┴──────────────────┐
        │           bus::BUS (全局总线)         │
        │  parking_lot::Mutex<Bus> 全局单例   │
        │  DI / DO / AI / AO / Sys 状态       │
        │  + Modbus 寄存器读写 API            │
        └──────┬──────┬──────┬──────┬─────────┘
               │      │      │      │
   ┌───────────┘      │      │      └───────────────┐
   │                  │      │                      │
┌──▼───────┐  ┌───────▼──┐ ┌─▼────────┐  ┌──────────▼──────┐
│ IO 模块  │  │ Channel │ │ Modbus   │  │   BLE Mesh     │
│ (DI/DO)  │  │ (AI/AO)  │ │ RTU/TCP  │  │  Proxy + Node  │
└────┬─────┘  └────┬─────┘ └────┬─────┘  └────────┬───────┘
     │             │            │                 │
     └─────────────┴────────────┴─────────────────┘
                       │
              ┌────────▼────────┐
              │     HAL 层      │
              │ GPIO/UART/      │
              │ ADC/LEDC         │
              └────────┬────────┘
                       │
       ┌───────────────┴────────────────┐
       │       ESP-IDF v5.5.2            │
       │  (esp_idf_sys + esp_idf_hal)    │
       └───────────────┬────────────────┘
                       │
              ┌────────▼────────┐
              │   硬件外设      │
              │ ESP32-S3R8 SoC │
              │ + W5500 (SPI)  │
              └─────────────────┘
```

## 模块设计

### 1. HAL 硬件抽象层 (`src/hal/`)

统一封装 ESP32-S3 的 GPIO/UART/ADC/LEDC 资源。**SPI2_HOST 由 ethernet 模块独占管理**（W5500 是 SPI 总线上唯一外设）。

| 文件 | 职责 |
|------|------|
| `mod.rs` | `Hal` 总结构，调用各子模块 init |
| `pins.rs` | `HalPins` 引脚号容器，按 config::pins 分组 |
| `uart.rs` | UART0/UART1/UART2 参数容器（不实际打开，3 路 UART） |
| `adc.rs` | ADC1 6 通道，`sample(idx) -> u16` |
| `ledc.rs` | LEDC 4 通道 PWM，`set_duty(idx, duty)` |
| `gpio.rs` | 8 DI + 8 DO + ETH_INT/RST + 2 RS485_DE（默认版本）<br>F3/F4 版本: 仅 ETH_INT/RST + 2 RS485_DE（DI/DO 释放给 I2C）<br>默认版本实现 `DigitalIo` trait |
| `digital_io.rs` ★ 阶段七 | `DigitalIo` trait 定义 (DI/DO 统一抽象接口) |
| `i2c_bus.rs` ★ 阶段六 | I2C 总线封装 (仅 F3/F4) |
| `mcp23017.rs` ★ 阶段六 | MCP23017 单芯片驱动 (仅 F3/F4) |
| `io_ext.rs` ★ 阶段六 | IO 扩展聚合器，统一管理多片 MCP23017 (仅 F3/F4) |

**设计要点**：
- 引脚号集中在 `config::pins`，硬件改版只需改一处
- `Hal` 不再持有 SpiBus：SPI 总线由 ethernet::w5500 模块独占初始化和管理
- 自引用类型用 `Box::leak`（LEDC）固定为 `'static`
- ETH_RST 由 `hal.gpio.eth_reset()` 提供，ethernet 模块复用避免重复 `gpio_config`
- F3/F4 版本下 `Hal` 新增 `io_ext: IoExtender` 字段，DI/DO 通过 I2C MCP23017 扩展
- **`Hal::dio()` 方法 (阶段七)**: 返回 `&dyn DigitalIo`，根据编译期版本自动选择实现（默认 GpioBank / F3-F4 IoExtender），上层无需 `#[cfg]` 分支

### 2. 以太网模块 (`src/ethernet/`)

W5500 over SPI2_HOST，单口 + 应用层简单冗余。

**SPI 资源管理**：
- W5500 是 SPI2 总线上唯一设备
- `ethernet::w5500::start()` 内部调用 `spi_bus_initialize` + `spi_bus_add_device` 完整初始化 SPI 总线
- 引脚号从 `config::pins::ETH_SPI_*` 读取

**C API 调用流程**：
```
1. spi_bus_initialize(SPI2_HOST, &bus_cfg, dma_chan=1)
2. spi_bus_add_device(spi_host, &dev_cfg, &mut spi_handle)
   // W5500 SPI: mode 0, 20MHz, command_bits=0, address_bits=0
3. hal.gpio.eth_reset()  // 复用 HAL 的 ETH_RST 引脚, 拉低 50ms + 等待 50ms
4. esp_eth_mac_new_w5500(&w5500_cfg, &mut mac_cfg)
5. esp_eth_phy_new_w5500(&phy_cfg)  // W5500 内部 PHY, addr=0
6. esp_eth_driver_install(&eth_cfg, &mut eth_handle)
7. esp_netif_create_default_eth_mac() + esp_eth_new_netif_glue + esp_netif_attach
8. esp_eth_start(eth_handle)
9. esp_event_handler_register(IP_EVENT, IP_EVENT_ETH_GOT_IP, ip_event_cb)
10. spawn_heartbeat() // 5s 周期心跳，连续 3 次失败 esp_restart
```

**W5500 vs DM9051 差异**：
- SPI 帧格式：W5500 用 2 字节地址段 (bit15=R/W)，DM9051 用 1 bit R/W + 7 bit reg
- ESP-IDF 驱动内部处理帧格式，外部仅需 `command_bits=0, address_bits=0`
- W5500 PHY 地址固定为 0（内部 PHY），DM9051 PHY 地址 1
- W5500 驱动需作为外部 IDF Component 添加 (`espressif/w5500`, 见 `idf_component.yml`)

### 3. RS485 模块 (`src/rs485/`)

利用 ESP-IDF UART 内置 RS485 半双工模式，自动控制 DE/RE。

ESP32-S3 有 3 个 UART：
- **UART0** (GPIO43/44)：下载/日志，115200-N-8-1
- **UART1** (GPIO40/41)：RS485 #0 (Modbus RTU 主站)
- **UART2** (GPIO17/18)：RS485 #1 (Modbus RTU 从站)

不再需要 RS485 与下载串口复用，稳定性更好。

| 文件 | 职责 |
|------|------|
| `config.rs` | `Rs485Config` + `from_rtu_master/from_rtu_slave` 构造函数 |
| `port.rs` | `Rs485Port::open/send_recv/write/read` + 3.5 字符帧间静默 + RX 超时 |

**关键 API**：
- `uart_param_config` 配置波特率/数据位/校验/停止位
- `uart_set_pin` 把 RTS 接到 DE 引脚
- `uart_driver_install` 安装驱动
- `uart_set_mode(UART_MODE_RS485_HALF_DUPLEX)` 切换为 RS485 模式
- `uart_set_rx_timeout(port, 3)` 设置 RX 超时（3.5 字符时间，9600bps≈4ms）
- `uart_write_bytes` / `uart_read_bytes` 收发

### 4. Modbus 模块 (`src/modbus/`)

手写 RTU/TCP，**不依赖 umodbus crate**（避免 API 不匹配问题）。

| 文件 | 职责 |
|------|------|
| `shared.rs` | `BusBackend` 实现 + `modbus_crc16` + 异常码 |
| `rtu_master.rs` | 主站轮询任务（200ms 周期，硬编码轮询表） |
| `rtu_slave.rs` | 从站响应 FC=01/02/03/04/05/06/0F/10 |
| `tcp_server.rs` | MBAP header 解析 + 多连接 (MAX=4) |

**CRC16 实现**：标准 Modbus 算法，polynomial 0xA001，init 0xFFFF，LSB first。

### 5. IO 模块 (`src/io/`)

| 文件 | 周期 | 实现要点 |
|------|------|---------|
| `di.rs` | 1ms | DI 采样 + 软件去抖（连续 3 次相同才更新）<br>默认版本: 8 路 GPIO；F3/F4: 16/48 路 I2C MCP23017 |
| `do_.rs` | 10ms | DO 输出，XOR diff 检测变化后才写硬件<br>默认版本: 逐通道 GPIO；F3/F4: 整体 I2C 写入 |

### 6. AI/AO 通道 (`src/channel/`)

| 文件 | 周期 | 实现要点 |
|------|------|---------|
| `ai.rs` | 100ms | 6 路 ADC + 滑动平均（位移 AVG_SHIFT=3, 窗口 8）+ 4-20mA 转换 |
| `ao.rs` | 100ms | 4 路 PWM，scaled(0-10000) → duty(0-4095) 转换 |

**4-20mA 转换**：`scaled = 4000 + avg * 16000 / 4095`（结果范围 4000-20000，单位 mA×1000）

### 7. BLE Mesh (`src/blemesh/`)

ESP32-S3R8 内置 BLE 5.0 + Bluetooth Mesh。ESP-IDF 的 BLE Mesh 主要以 C API 暴露，esp-idf-svc 未完整封装，直接通过 esp_idf_sys 调用。

**字段布局严格对齐 ESP-IDF v5.5.2 源码**（`/Users/ling/workspace/esp-idf/components/bt/esp_ble_mesh/api/`）。

| 文件 | 职责 |
|------|------|
| `bindings.rs` | esp_bt_controller/bluedroid/esp_ble_mesh_init C API 封装, 结构体布局 |
| `models.rs` | Generic OnOff Server/Client 模型定义, 消息收发 |
| `provisioning.rs` | 静态 OOB + Provisioner 配网流程, 事件解析 |

**三类回调注册** (v5.5.2 事件值空间不同, 必须独立):
- `esp_ble_mesh_register_prov_callback` → `prov_event_cb` (NODE_PROV_COMPLETE=10 / PROVISIONER_PROV_COMPLETE=31)
- `esp_ble_mesh_register_custom_model_callback` → `custom_model_event_cb` (MODEL_OPERATION=0 / SEND_COMP=1)
- `esp_ble_mesh_register_generic_client_callback` → `generic_client_event_cb` (GET_STATE=0 / SET_STATE=1 / PUBLISH=2 / TIMEOUT=3)

**关键 API** (v5.5.2):
- `esp_ble_mesh_init(prov, comp)` 双参数
- `esp_ble_mesh_model_publish(model, opcode, length, data, role)` 5 参数 (无 ctx)
- `esp_ble_mesh_server_model_send_msg(model, ctx, opcode, length, data)` 5 参数 (含 ctx)
- `esp_ble_mesh_client_model_send_msg(model, ctx, opcode, length, data, timeout, need_rsp, role)` 8 参数
- `esp_ble_mesh_proxy_gatt_enable()` (非 proxy_proxy_enable)

**关键结构体**:
- `EspBleMeshProv` (128 bytes, NODE+PROVISIONER 双段, uuid 是指针非数组)
- `EspBleMeshMsgCtx` (44 bytes, 17 字段 + enh 12 bytes 占位)
- `EspBleMeshModel` (40 bytes, keys[3]/groups[3] Kconfig 默认值)
- `ModelOpParam` (20 bytes, opcode+model+ctx+length+msg, 无 errcode)

**Mesh ↔ 总线交互**：
- 收到 OnOff Set → 更新 `BUS.do_.bits` bit0 → io 任务刷新 GPIO
- DO 状态变化 → 通过 OnOff Status 上报

### 8. 设备模块 (`src/device/`) ★ 阶段一+二

管理两类 NVS 持久化数据：
- **协议存储区** (`ProtoStore`)：单段连续 1500 个 U16（3000 字节），作为"用户自定义协议"的载体。系统不解析协议语义，只负责 RAM 镜像 + NVS 持久化 + 读写接口。
- **系统配置** (`SystemConfig`)：SN/MAC/IP/网关/RS485/BLE 等结构化字段，提供 Modbus 寄存器映射 + AT 命令 + NVS 持久化 + 应用生效入口。

| 文件 | 职责 |
|------|------|
| `mod.rs` | NVS 全局句柄 + ProtoStore/SystemConfig 加载/保存 + 监听线程 (commit/reload/apply_config) + AT/Modbus 读写 API + 复位计数持久化 |
| `system_config.rs` ★ 阶段二新增 | SystemConfig 结构 + 编解码 + NVS 持久化 + Modbus 寄存器读写映射 |

**持久化策略**：

| 数据 | RAM 镜像 | NVS namespace/key | 触发方式 |
|------|----------|---------------------|----------|
| ProtoStore | `bus.proto.data` (3000 字节) | `gateway`/`proto_data` | 写 COMMIT=0xC5C5 → 异步线程持久化 |
| SystemConfig | `bus.cfg` (~128 字节) | `gateway`/`sys_cfg` | 写 APPLY=0xB5B5 或 AT+CFGAPPLY → 异步线程持久化 |
| 复位计数 | `bus.sys.reset_count` | `gateway`/`rst_cnt` | 启动时读取并 +1，每次启动写回 |

**核心 API**：

| 函数 | 调用方 | 说明 |
|------|--------|------|
| `device::init()` | main.rs | 启动时加载 NVS + 启动监听线程 |
| `device::request_commit()` | Modbus write_hold_reg | 异步触发协议持久化 |
| `device::request_reload()` | Modbus write_hold_reg | 异步触发协议重载 |
| `device::commit_sync()` | AT+COMMIT | 同步协议持久化 |
| `device::reload_sync()` | AT+RELOAD | 同步协议重载 |
| `device::request_apply_config()` | Modbus write_hold_reg | 异步触发配置应用（持久化+生效） |
| `device::apply_config_sync()` | AT+CFGAPPLY / AT+CFGRESET | 同步应用配置 |
| `device::proto_read/write/bulk` | ble_at | AT 命令直接读写协议区 |
| `device::nvs_partition()` | blemesh | 共享 NVS 句柄 |
| `device::load_reset_count / save_reset_count` | main.rs | 复位计数持久化 |

**SystemConfig 字段布局**（128 字节固定布局）：

```
偏移   字段                长度   默认值
0      SN (ASCII)          32    "ESP32S3-UNKNOWN-0001"
32     name (ASCII)        16    "GW-ESP32S3"
48     hw_version           2    0x0100
50     fw_version           2    0x0100 (从 CARGO_PKG_VERSION 解析)
52     cfg_version          2    0 (每次 APPLY 自增)
54     eth_mac              6    (从 esp_read_mac(ESP_MAC_ETH) 读取)
60     dhcp                 1    1
61     ip                   4    192.168.1.100
65     mask                 4    255.255.255.0
69     gateway              4    192.168.1.1
73     dns                  4    192.168.1.1
77     ble_mac              6    (从 esp_read_mac(ESP_MAC_BT) 读取)
83     ble_name (ASCII)     8    "GW-S3"
91     ble_mesh_enable      1    1
92     rs485[0]             9    baud=9600/slave=1/mode=Slave
101    rs485[1]             9    同上
```

NVS magic=0x4757_4346 ("GWCF")，校验通过则解码，否则使用 defaults()。

### 9. BLE AT 命令通道 (`src/ble_at/`) ★ 阶段一+二

在 BLE Mesh 之外额外提供 GATT 自定义服务，用于设备配置阶段（手机 APP 直连设备写入协议/配置）。

| 文件 | 职责 |
|------|------|
| `mod.rs` | GATT 服务注册 + 输入输出缓冲区 + 处理线程 |
| `parser.rs` | AT 命令字符串解析 + 命令路由分发 |
| `handlers.rs` | 协议区 AT 命令处理 (READ/WRITE/BULKR/BULKW/COMMIT/RELOAD/INFO/STATUS/RESET/VERSION) |
| `cfg_handlers.rs` ★ 阶段二新增 | 系统配置 AT 命令处理 (15 个 CFG* 命令) |

**GATT 服务结构**：
- Service UUID: `0xFF01`
- RX Char (Write): `0xFF02` (主机 → 设备, AT 命令)
- TX Char (Notify): `0xFF03` (设备 → 主机, AT 响应)

**AT 命令集 - 协议区**（阶段一）：

| 命令 | 说明 | 响应 |
|------|------|------|
| `AT+READ=<addr>` | 读协议区地址 | `OK <value>,0xXXXX` |
| `AT+WRITE=<addr>,<value>` | 写协议区地址 | `OK` |
| `AT+BULKR=<start>,<len>` | 批量读 | `OK 0xXXXX,0xXXXX,...` |
| `AT+BULKW=<start>,<v1>,<v2>,...` | 批量写 | `OK <N> words` |
| `AT+COMMIT` | 同步提交到 NVS | `OK committed` |
| `AT+RELOAD` | 从 NVS 重载 | `OK reloaded` |
| `AT+INFO` | 查询存储区信息 | `OK cap=1500,ver=...,len=...` |
| `AT+STATUS` | 查询系统状态 | `OK uptime=...,fw=...,di=...` |
| `AT+RESET` | 触发复位 | `OK resetting in 100ms` |
| `AT+VERSION` | 固件版本 | `OK <name> v<x.x>` |

**AT 命令集 - 系统配置**（阶段二新增）：

| 命令 | 说明 | 响应 |
|------|------|------|
| `AT+CFGSN[=<sn>]` | 设置/读 SN (32 字符) | `OK <sn>` / `OK` |
| `AT+CFGNAME[=<name>]` | 设置/读设备名称 (16 字符) | `OK <name>` / `OK` |
| `AT+CFGIP[=<ip>,<mask>,<gw>[,<dns>]]` | 设置/读网络配置 | `OK ip=...,mask=...` |
| `AT+CFGDHCP=<0\|1>` | 启用/禁用 DHCP | `OK dhcp=N` |
| `AT+CFGMAC` | 读以太网 MAC | `OK AA:BB:CC:DD:EE:FF` |
| `AT+CFGBTMAC` | 读蓝牙 MAC | `OK AA:BB:CC:DD:EE:FF` |
| `AT+CFGBTNAME[=<name>]` | 设置/读 BLE 名称 (8 字符) | `OK <name>` / `OK` |
| `AT+CFGMESH=<0\|1>` | 启用/禁用 BLE Mesh | `OK ble_mesh=N` |
| `AT+CFG485=<idx>,<baud>,<data>,<stop>,<parity>,<slave>,<mode>` | 配置 RS485 | `OK` |
| `AT+CFG485=<idx>` | 读 RS485 配置 | `OK idx=...,baud=...,...` |
| `AT+CFGAPPLY` | 应用配置 (持久化+生效) | `OK applied (restart to take effect)` |
| `AT+CFGRESET` | 恢复默认配置 | `OK reset to defaults` |
| `AT+CFGINFO` | 列出所有配置 | `OK sn=...,name=...,ip=...,...` |
| `AT+CFGREAD=<addr>` | 按 Modbus 地址读 U16 | `OK N,0xXXXX` |
| `AT+CFGWRITE=<addr>,<value>` | 按 Modbus 地址写 U16 | `OK` / `OK apply requested` |

### 10. 健康监控 (`src/health.rs`)

提供两层保护：

1. **任务看门狗** — ESP-IDF Task Watchdog (CONFIG_ESP_TASK_WDT_TIMEOUT_S=10)
   - `subscribe_wdt()` 把当前任务加入看门狗监控
   - `feed_wdt()` 喂狗 (必须在 10s 内调用，main_loop 100ms 喂一次)

2. **任务心跳** — 软件心跳，main_loop 周期检查各任务是否存活
   - `register(&'static TaskHb)` 启动时注册（最多 16 个任务）
   - `tick(&'static TaskHb)` 任务 loop 中递增（无锁 AtomicU32）
   - `check_all()` 返回停滞任务名列表（心跳连续未变化次数超过 max_stall）
   - 阻塞型任务（TCP 监听/RTU 从站）用 `new_with_stall(name, 10)` 允许较长时间无活动

## 全局总线 (`src/bus.rs`)

```rust
pub static BUS: Lazy<Mutex<Bus>> = Lazy::new(|| Mutex::new(Bus::new()));
```

**Modbus 寄存器映射**：

| 类型 | 地址范围 | 说明 |
|------|---------|------|
| 线圈 Coil | 0x0000-0x0007 | 8 路 DO (FC=01/05/0F) |
| 离散输入 | 0x0000-0x0007 | 8 路 DI (FC=02) |
| 输入寄存器 | 0x0000-0x0005 | 6 路 AI 原始 ADC (FC=04) |
| 输入寄存器 | 0x0010-0x0015 | 6 路 AI 工程量 (FC=04) |
| 保持寄存器 | 0x0000-0x0003 | 4 路 AO 工程量 (FC=03/06/10) |
| 保持寄存器 | 0x0100 | 固件版本 (BCD) |
| 保持寄存器 | 0x0101 | 运行时长（秒） |
| 保持寄存器 | 0x0102 | 复位计数 |
| 保持寄存器 | 0x0103 | 写 0xA5A5 触发复位 |
| 保持寄存器 | 0x0104 | 复位原因 (RO, esp_reset_reason_t) |
| 保持寄存器 | 0x0105 | 任务健康位图 (RO, bit=1 表示该任务停滞) |
| 保持寄存器 | 0x0106 | 日志级别 (0=Err 1=Warn 2=Info 3=Debug 4=Trace, RW) |
| 保持寄存器 | 0x0200-0x025F | 系统配置区 (SN/NAME/IP/MAC/RS485/BLE 等) |
| 保持寄存器 | 0x4000-0x45DB | 协议存储区 1500 字 (FC=03/06/10) |
| 保持寄存器 | 0x45DC | 写 0xC5C5 触发 COMMIT |
| 保持寄存器 | 0x45DD | 写 0xA5A5 触发 RELOAD |
| 保持寄存器 | 0x45DE | 协议版本 (RW) |
| 保持寄存器 | 0x45DF | 协议长度 (RW) |
| 保持寄存器 | 0x45E0 | 状态 (RO, 0=空闲 1=写入中 2=加载中 3=校验失败) |
| 保持寄存器 | 0x45E1 | 魔数 (RO=0x4757 'GW') |

## 并发模型

- **使用 `std::thread`**，不用 embassy（更简洁稳定）
- **`parking_lot::Mutex`** 替代 `std::sync::Mutex`，提供 `try_lock_for(100ms)` 超时
- **`once_cell::Lazy`** 全局单例
- 各任务独立线程：
  - `eth-heartbeat` (5s, 阈值 10)
  - `mesh-heartbeat` (60s)
  - `mb-rtu-master` (200ms 周期)
  - `mb-rtu-slave` (1s 监听, 阈值 10)
  - `mb-tcp-listen` + `mb-tcp-conn-{n}` (每连接一线程)
  - `io-di-scan` (1ms)
  - `io-do-output` (10ms)
  - `ai-sample` (100ms)
  - `ao-output` (100ms)
  - `proto-store` (50ms 轮询 commit/reload/apply 标志)
  - `ble-at` (10ms 轮询 AT 命令缓冲区)
- **main_loop** (100ms): 喂狗 + 每 1s 更新 uptime/检查任务心跳/上报状态

## 启动顺序

1. 日志初始化 (默认 Info 级别，运行时可通过 0x0106 调节)
2. ESP-IDF 基础设施（Peripherals / SystemEventLoop / TimerService）
3. HAL 初始化（GPIO/UART/ADC/LEDC）
4. 设备协议存储初始化（NVS 加载，失败回退空 ProtoStore）
5. 复位原因记录 + 复位计数持久化
6. 启动以太网 (W5500)
7. 启动 Wi-Fi (Station, 可选, `--features wifi`)
8. 启动 BLE Mesh + BLE AT 命令通道
9. 启动 IO 扫描 (DI/DO)
10. 启动 AI/AO 通道
11. 启动 RS485 + Modbus RTU/TCP
12. 进入主循环（喂狗 + 健康检查 + 状态上报）

## 存储区域划分 (Flash 8MB)

ESP32-S3R8 8MB Flash 分区表 (`partitions.csv`)，符合 ESP-IDF 生产分区惯用做法：

| 分区 | 类型 | 偏移 | 大小 | 用途 |
|------|------|------|------|------|
| `nvs` | data,nvs | 0x10000 | 24KB | 系统配置 (加密) |
| `phy_init` | data,phy | 0x16000 | 4KB | PHY 校准 |
| `nvs_keys` | data,nvs_keys | 0x17000 | 4KB | NVS 加密密钥 (encrypted) |
| `otadata` | data,ota | 0x18000 | 8KB | OTA 选择 |
| `factory` | app,factory | 0x20000 | 2.25MB | 工厂固件 (与 OTA 等大) |
| `ota_0` | app,ota_0 | 0x260000 | 2.25MB | OTA 槽 0 |
| `ota_1` | app,ota_1 | 0x4A0000 | 2.25MB | OTA 槽 1 |
| `coredump` | data,coredump | 0x6E0000 | 64KB | 崩溃转储 (ELF 格式) |
| `ble_mesh` | data,nvs | 0x6F0000 | 64KB | BLE Mesh 独立 NVS |
| `storage` | data,fat | 0x700000 | 1MB | FAT 大文件存储 |

**关键设计**：
- `factory` 与 `ota_0/ota_1` 等大 (2.25MB)，保证 factory 固件可 OTA 升级
- `nvs_keys` 分区存储 NVS 加密密钥，配合 `CONFIG_NVS_ENCRYPTION=y`
- `coredump` 分区用于 panic 时转储任务上下文，便于工业现场故障分析
- `ble_mesh` 分区通过 `CONFIG_BT_BLE_MESH_SPECIFIC_PARTITION=y` 启用，BLE Mesh 栈自动使用独立 NVS，与系统 nvs 隔离
- Flash 配置: QIO + 80MHz + 8MB (显式配置，避免 ESP-IDF 默认 40MHz 性能损失)

## ESP32-S3R8 硬件潜力实施 (阶段四)

针对 ESP32-S3R8 的硬件特性评估并实施到系统中。按"已实施 / 评估后不实施"两类记录。

### 已实施的硬件特性

#### 1. 双核 SMP 任务绑定

ESP32-S3R8 双核 Xtensa LX7 (Core 0 + Core 1)，默认 Rust `std::thread::spawn` 不绑定核，任务在核间漂移导致 cache miss 抖动。

通过 `vTaskCoreAffinitySet` (ESP-IDF FreeRTOS SMP v10.5+) 把任务固定到指定核：

| 核 | 任务 | 设计原则 |
|----|------|----------|
| Core 0 (CORE_NET) | main_loop / eth / mb-tcp / mb-rtu-master / mb-rtu-slave / ble-at / proto-store | 与 LwIP/Bluedroid/FreeRTOS 系统任务同核, 减少 IPC |
| Core 1 (CORE_RT) | io-di-scan / io-do-output / ai-sample / ao-output | 避开网络/协议栈抖动, 1ms DI 周期更稳定 |

实现位置：`src/health.rs::pin_current_to_core()`，11 个任务文件中调用。

sdkconfig：
```ini
CONFIG_FREERTOS_UNICORE=n
CONFIG_FREERTOS_NO_AFFINITY_HIGHEST_BOUND=y   # 允许任务无亲和性 (默认 0x3)
CONFIG_FREERTOS_TLSP_DELETION_CALLBACKS=y     # Rust thread::JoinHandle 需要
```

#### 2. ADC Continuous + DMA

ESP32-S3 ADC1 支持 Continuous + DMA 模式：6 通道后台 DMA 连续采样 (10kHz/通道)，CPU 仅读环形缓冲区，零 CPU 占用。

实现位置：`src/hal/adc.rs`，通过 `feature_adc_continuous` feature flag 切换：

- **默认** (`adc-continuous` feature)：Continuous + DMA 模式
  - `esp_adc_continuous_new_handle` 创建 handle (内部环形缓冲区 1KB)
  - 6 通道 `adc_digi_pattern_config_t` (12-bit, 11dB 衰减 = 0-3.1V)
  - 10kHz/通道采样频率
  - `sample_all()` 从 DMA 缓冲区非阻塞读取，按通道平均
  - 输出格式：每样本 2 字节 `[data:12 | channel:4]`
- **Fallback** (`--no-default-features`)：OneShot + Mutex (兼容性回退)

#### 3. ICache / DCache 32KB

ESP32-S3 内置 32KB ICache + 32KB DCache，提升代码执行和数据访问速度。

sdkconfig：
```ini
CONFIG_ESP32S3_INSTRUCTION_CACHE_32KB=y
CONFIG_ESP32S3_INSTRUCTION_CACHE_LINE_32B=y
CONFIG_ESP32S3_DATA_CACHE_32KB=y
CONFIG_ESP32S3_DATA_CACHE_LINE_64B=y
```

#### 4. 硬件加密加速器

ESP32-S3 内置 AES/SHA/MPI/GCM/ECC 硬件加速器，启用后 TLS/BLE Mesh/NVS 加密走硬件，CPU 卸载。

sdkconfig：
```ini
CONFIG_MBEDTLS_HARDWARE_AES=y
CONFIG_MBEDTLS_HARDWARE_MPI=y    # RSA/DH
CONFIG_MBEDTLS_HARDWARE_SHA=y
CONFIG_MBEDTLS_HARDWARE_GCM=y
CONFIG_MBEDTLS_HARDWARE_ECC=y
```

#### 5. PSRAM 8MB Octal 分配策略

ESP32-S3R8 内置 8MB Octal SPI PSRAM，与 512KB 内部 SRAM 协同分配：

| 内存类型 | 用途 | 配置 |
|---------|------|------|
| 内部 SRAM (512KB) | 栈/DMA 描述符/中断/Mutex/小 struct | `MALLOC_ALWAYSINTERNAL=4096` (<4KB 走内部) |
| PSRAM (8MB) | ProtoStore 缓冲/NVS blob/BLE Mesh 配置/大数据 | `MALLOC_RESERVE_INTERNAL=16384` (保留 16KB 给 DMA) |

```ini
CONFIG_SPIRAM=y
CONFIG_SPIRAM_MODE_OCT=y            # Octal SPI 模式
CONFIG_SPIRAM_SPEED_80M=y           # 80MHz
CONFIG_SPIRAM_USE_AHB_DBUS3=y       # 通过 DBUS3 访问 PSRAM
CONFIG_SPIRAM_XIP_FROM_DATA=y       # 允许 PSRAM 代码执行
CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=4096
CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=16384
CONFIG_SPIRAM_MALLOC_FAIL_INTERNAL=y
```

#### 6. Wi-Fi (ESP32-S3 内置)

ESP32-S3R8 内置 Wi-Fi 802.11 b/g/n，作为**以太网冗余链路**或**AP 配置入口**。与 BLE 共用 2.4GHz 射频，通过 ESP-IDF coexistence 软件分时调度。

实现位置：`src/wifi/mod.rs`（feature flag `wifi`，默认不启用）：

- Station 模式：连接上游 AP，作为以太网故障备份链路
- AP 模式 (TODO)：自身作为 AP，提供手机直连配置入口
- SSID/password 当前硬编码在 `config::wifi`，TODO 从 SystemConfig 动态加载
- Wi-Fi 启动失败仅记日志，不阻断主流程（备份链路可缺失）

sdkconfig：
```ini
CONFIG_ESP_WIFI_ENABLED=y
CONFIG_ESP_WIFI_SOFTAP_SUPPORT=y       # 允许 AP 模式
CONFIG_ESP_COEX_SW_COEXIST_ENABLE=y    # Wi-Fi/BLE 共存
CONFIG_ESP_WIFI_NVS_ENABLED=y
```

启用方式：`cargo build --release --features wifi`

### 评估后不实施的硬件特性

#### USB OTG (ESP32-S3 内置 USB-OTG)

ESP32-S3 内置 USB 2.0 OTG (D+/D- 在 GPIO19/GPIO20)，但本系统的 DI 占用 GPIO19/20：

| GPIO | USB OTG 用途 | 本系统占用 |
|------|-------------|----------|
| GPIO19 | USB D+ | DI 1 (8 路 DI 之一) |
| GPIO20 | USB D- | DI 2 (8 路 DI 之一) |

**结论**：USB OTG 不可用。如需启用 USB，需硬件改版把 DI1/DI2 迁移到其他 GPIO（如 GPIO35/36/37，但已被 DI5/6 占用）。当前 8 路 DI 全占用可用 GPIO，无可用替代引脚。

工业网关场景下，配置与日志通过以太网 + BLE 完成，无需 USB，故不实施。

#### ULP 协处理器 (Ultra-Low Power)

ESP32-S3 内置 ULP-RISC-V 协处理器，可在主 CPU 深睡时执行简单任务（ADC 采样、I2C 读写、唤醒主 CPU）。

**不适用原因**：本系统为工业网关，**常供电**（无电池供电场景），主 CPU 始终运行，无深睡需求。ULP 价值在于低功耗场景（电池供电的传感器节点），与工业网关的"持续在线、毫秒级响应"目标冲突。

#### 其他未实施项

| 硬件特性 | 评估结论 |
|---------|---------|
| RMT (红外/LED 矩阵) | 与 LEDC PWM 类似，AO 已用 LEDC，无独立需求 |
| I2S (音频) | 工业网关无音频需求 |
| PCNT (脉冲计数) | DI 当前用 GPIO 中断 + 软件去抖，PCNT 适合高速脉冲 (≥10kHz)，当前 DI 场景未达此频率，不实施 |
| TWAI (CAN 2.0) | 系统已用 Modbus RTU/TCP + BLE Mesh，无 CAN 需求；如需 CAN 可后续扩展 |
| 42 通道 DMA | 已用于 ADC Continuous 和 SPI/UART，无新增需求 |
| AES-NI 指令 | 已通过硬件加密加速器覆盖 |

## Rust + ESP32-S3R8 最佳实践 (阶段五)

在阶段四硬件潜力实施后，针对"Rust 语言特性 + ESP32-S3R8 硬件"的协同优化项，进一步发挥双方潜力。

### 已实施项

#### 1. OTA 升级 (Rust + ESP-IDF esp_ota_*)

分区表已预留 ota_0/ota_1/otadata，实现 OTA 升级 API 与触发入口：

- **API**：`src/ota/mod.rs` 提供 `begin() / write_chunk() / end() / abort() / status() / reboot_to_new_firmware()`
- **状态机**：Idle → Receiving → DonePendingReboot → (reboot) Idle
- **触发方式 1 (BLE AT)**：`AT+OTA=BEGIN,<total>` / `AT+OTA=WRITE,<hex>` / `AT+OTA=END` / `AT+OTA=STATUS` / `AT+OTA=REBOOT` / `AT+OTA=ABORT`
- **触发方式 2 (Modbus)**：寄存器 0x0107-0x010F (status RO / total RW / written RO / begin WO=0x0B0A / end WO=0x0E0D / abort WO=0x0AB0 / reboot WO=0x0F0E)
- **数据传输**：BLE AT 用 hex 编码单帧 ≤ 512 字节 binary（heapless::Vec 零分配）
- **回滚支持**：sdkconfig 启用 `CONFIG_APP_ROLLBACK_ENABLE` + `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`，OTA 失败可回到原分区
- **错误类型**：新增 `AppError::Ota` 变体，与其它子系统并列

文件：[src/ota/mod.rs](../src/ota/mod.rs), [src/ble_at/ota_handlers.rs](../src/ble_at/ota_handlers.rs), [src/ble_at/parser.rs](../src/ble_at/parser.rs) (handle_ota), [src/bus.rs](../src/bus.rs) (寄存器读写), [src/config.rs](../src/config.rs) (寄存器号)

#### 2. Rust panic hook + backtrace

ESP-IDF 默认 panic handler 打印 CPU 寄存器 + Xtensa backtrace，但 Rust panic 信息缺失。新增 Rust 层 panic hook：

```rust
std::panic::set_hook(Box::new(move |info| {
    log::error!("RUST PANIC: {}", info);
    log::error!("  at {}:{}:{}", loc.file(), loc.line(), loc.column());
    log::error!("backtrace:\n{}", std::backtrace::Backtrace::force_capture());
    default_hook(info);  // → 触发 ESP-IDF panic handler → 复位
}));
```

sdkconfig：
```ini
CONFIG_ESP_SYSTEM_PANIC_PRINT_REBOOT=y   # Panic 时打印寄存器 + backtrace + 重启
CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=n         # GDB stub (开发调试用, 默认关)
```

注意：release profile `strip = true` 移除符号，backtrace 显示地址需用 `xtensa-esp32s3-elf-addr2line` 在 host 解析。

文件：[src/main.rs](../src/main.rs) `install_panic_hook()`

#### 3. 关键路径 #[inline]

热路径函数加 `#[inline` 让编译器内联，减少函数调用开销：

| 函数 | 文件 | 调用频率 |
|------|------|---------|
| `modbus_crc16` | [src/modbus/shared.rs](../src/modbus/shared.rs) | 每条 Modbus 帧 2 次 (build + verify) |
| `AdcHandle::sample_all` / `sample` | [src/hal/adc.rs](../src/hal/adc.rs) | AI 任务 10Hz 周期 |
| `parse_u16` | [src/ble_at/parser.rs](../src/ble_at/parser.rs) | 每条 AT 命令多次 |

#### 4. build.rs feature 互斥校验

`build.rs` 新增 `check_feature_compatibility()`，编译期校验 feature 组合：

| 规则 | 行为 |
|------|------|
| `modbus-rtu` 与 `modbus-tcp` 同时关闭 | panic 中断编译 (至少一个通信通道) |
| `ethernet-w5500` 与 `wifi` 同时关闭 | cargo:warning (允许, 仅 BLE 通信) |
| `wifi` + `ble-mesh` 共存 | cargo:warning (提示依赖 COEX) |

避免运行时诡异行为，编译期就暴露配置错误。

文件：[build.rs](../build.rs) `check_feature_compatibility()`

#### 5. 任务栈溢出检测

sdkconfig 启用 FreeRTOS 栈溢出 canary 检测：

```ini
CONFIG_FREERTOS_CHECK_STACKOVERFLOW_CANARY=y   # Canary 模式, 每任务 +1 word 开销
CONFIG_FREERTOS_TASK_PRE_DELETE_CALLBACK=y      # 任务退出时回收 TCB/栈
```

栈溢出时触发 panic，由 panic handler 打印 backtrace + 复位。

#### 6. CPU 频率锁定 240MHz

ESP32-S3 最高频 240MHz，关闭动态调频 (DFS) 保持性能稳定：

```ini
CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ_240=y
CONFIG_PM_ENABLE=n   # 关闭电源管理 (工业网关常供电, 不需节能)
```

避免 DFS 切换导致的实时性抖动。

### 评估后不实施的 Rust 优化

| Rust 特性 | 评估结论 |
|----------|---------|
| async/await | 当前 `std::thread` + `Mutex` 在 ESP-IDF 上稳定, async 会引入 runtime 复杂度, 收益不明显 |
| embedded-hal trait 抽象 | 直接用 esp_idf_hal, trait 抽象需大改, 性价比低 (硬件固定不变) |
| defmt 替代 log | esp_idf_svc::log::EspLogger 已够用, defmt 收益主要在 no_std 环境 |
| cargo workspace 拆分 | 单 crate 编译时间可接受, 拆分增加维护成本 |
| RTIC 框架 | 与 std::thread + ESP-IDF 任务模型冲突, 不适用 |
| 静态内存池 | heapless 已覆盖热点 (Vec/String), 全静态池收益有限 |

## 硬件版本 F3/F4 支持 (阶段六)

通过编译期 feature flag 支持多种硬件版本，F3/F4 用 I2C MCP23017 扩展芯片扩展 DI/DO 通道数。

### 版本对比

| 版本 | feature | DI | DO | MCP23017 数量 | I2C 地址 |
|------|---------|----|----|--------------|---------|
| Default | (无) | 8 (GPIO) | 8 (GPIO) | 0 | - |
| F3 | `f3` | 16 (I2C) | 16 (I2C) | 2 | DI=0x20, DO=0x21 |
| F4 | `f4` | 48 (I2C) | 16 (I2C) | 4 | DI=0x20/0x21/0x22, DO=0x23 |

> F3 与 F4 互斥（build.rs 编译期校验），用法：`cargo build --features f3` 或 `--features f4`

### MCP23017 IO 扩展芯片

MCP23017 是 Microchip 的 16 通道 I2C IO 扩展芯片：
- 2 个 8 位端口（PORTA + PORTB），共 16 个 GPIO
- 地址范围 0x20-0x27（A0/A1/A2 引脚组合，最多 8 片共线）
- 支持内部上拉（GPPU 寄存器），省外部上拉电阻
- 最高速率 1.7MHz，本系统用 400kHz Fast Mode

### 架构层次

```
┌─────────────────────────────────────────┐
│  io/di.rs + io/do_.rs (任务层)           │
│  条件编译选择 GPIO 直驱 / IoExtender    │
└──────────────┬──────────────────────────┘
               │
   ┌───────────┴────────────┐
   │                        │
   ▼ 默认版本                ▼ F3/F4 版本
┌─────────────┐    ┌──────────────────┐
│ hal/gpio.rs │    │ hal/io_ext.rs    │
│ GpioBank    │    │ IoExtender       │
│ (8 DI+8 DO) │    │ (聚合多片芯片)   │
└─────────────┘    └────────┬─────────┘
                            │
              ┌─────────────┴─────────────┐
              │                           │
              ▼                           ▼
    ┌──────────────────┐       ┌──────────────────┐
    │ hal/mcp23017.rs  │       │ hal/i2c_bus.rs   │
    │ Mcp23017 (单片) │◄──────│ I2cBus           │
    └──────────────────┘ 读写  │ (esp_idf_hal I2C)│
                            └──────────────────┘
```

### 关键设计决策

#### 1. 编译期 feature flag（非运行时切换）

硬件版本在编译期确定，不支持运行时切换：
- `build.rs` 把 `CARGO_FEATURE_F3/F4` 转为 `feature_f3/feature_f4` cfg
- 源码用 `#[cfg(any(feature_f3, feature_f4))]` 条件编译
- F3 与 F4 互斥，build.rs panic 中断编译

**理由**：硬件固定不变，编译期确定可省去运行时分支和动态分发开销。

#### 2. DI/DO 位宽统一为 u64

`bus::DoState.bits` 和 `bus::DiState.bits` 从 u8 改为 u64：
- 默认版本: 用低 8 位
- F3: 用低 16 位
- F4: 用低 48 位（u64 够用）

所有版本共用同一数据结构，避免 `enum` 带来的分支开销。

#### 3. GpioBank 条件化 DI/DO 字段

`hal/gpio.rs::GpioBank` 用 `#[cfg]` 条件包含 DI/DO 字段：
- 默认版本: 含 `di: [PinDriver; 8]` + `do_: [Mutex<PinDriver>; 8]`
- F3/F4: 不含 DI/DO（由 `IoExtender` 管理），释放 GPIO21/33 给 I2C

`GpioBank::init` 也有两个版本签名，参数列表不同。

#### 4. IoExtender 聚合多片 MCP23017

`hal/io_ext.rs::IoExtender` 统一管理多片 MCP23017：
- 内部 `di_chips: [Mcp23017; 3]`（F4 最多 3 片 DI）
- `read_di() -> u64`: 遍历 DI 芯片，每片 16 位，按顺序拼接
- `write_do(value: u64)`: 单次 I2C 写入 DO 芯片（16 位）
- `do_cache`: 缓存上次写入值，避免重复 I2C 写

#### 5. Modbus 寄存器通道数版本相关

`config::regs` 中 `COIL_DO_COUNT` 和 `DISC_DI_COUNT` 改为引用 `hw_version::DO_COUNT/DI_COUNT`：
- 默认版本: 8 / 8
- F3: 16 / 16
- F4: 16 / 48

Modbus 主站/从站读写自动适配版本。

### I2C 引脚分配

F3/F4 版本下，原 DI 占用的 GPIO21/GPIO33 改为 I2C 总线：

| 引脚 | 默认版本 | F3/F4 版本 |
|------|---------|-----------|
| GPIO21 | DI2 | **I2C SDA** |
| GPIO33 | DI3 | **I2C_SCL** |

详见 [pinmap.md - 硬件版本 F3/F4](pinmap.md#硬件版本-f3--f4-i2c-mcp23017-扩展)。

### 文件清单

| 文件 | 改动 |
|------|------|
| [Cargo.toml](../Cargo.toml) | 新增 `f3` / `f4` feature（互斥），启用后自动启用 `io-di-do` |
| [build.rs](../build.rs) | `CARGO_FEATURE_F3/F4` → `feature_f3/feature_f4` cfg 桥接 + 互斥校验 |
| [src/config.rs](../src/config.rs) | 新增 `hw_version` 模块（版本常量）+ `io_ext` 模块（MCP23017 寄存器）+ `pins::I2C_*` 引脚 |
| [src/hal/i2c_bus.rs](../src/hal/i2c_bus.rs) ★ 新建 | I2C 总线封装 (I2cDriver + 寄存器读写) |
| [src/hal/mcp23017.rs](../src/hal/mcp23017.rs) ★ 新建 | MCP23017 单芯片驱动 (init/read/write/probe) |
| [src/hal/io_ext.rs](../src/hal/io_ext.rs) ★ 新建 | IoExtender 聚合器 (DI 读取 / DO 写入 / 通道操作) |
| [src/hal/mod.rs](../src/hal/mod.rs) | `Hal` 新增 `io_ext` 字段（条件）+ I2C 初始化 |
| [src/hal/gpio.rs](../src/hal/gpio.rs) | `GpioBank` 条件化 DI/DO 字段 + init 签名分版本 |
| [src/hal/pins.rs](../src/hal/pins.rs) | `HalPins` 条件化 di/do_ 字段 |
| [src/bus.rs](../src/bus.rs) | `DoState.bits` / `DiState.bits` 改为 u64，proto 方法用 `1u64 << addr` |
| [src/io/di.rs](../src/io/di.rs) | 采样逻辑条件编译：默认 GPIO / F3-F4 I2C，状态用 u64 |
| [src/io/do_.rs](../src/io/do_.rs) | 输出逻辑条件编译：默认逐通道 GPIO / F3-F4 整体 I2C |
| [src/ble_at/handlers.rs](../src/ble_at/handlers.rs) | `AT+STATUS` / `AT+VERSION` 适配版本与 u64 格式 |
| [src/blemesh/models.rs](../src/blemesh/models.rs) | do_.bits 操作加 `0x01u64` 显式后缀 |
| [src/blemesh/bindings.rs](../src/blemesh/bindings.rs) | heartbeat do_.bits 加 `0x01u64` 显式后缀 |

## 设备抽象层 (阶段七 - G)

### DigitalIo trait

`DigitalIo` trait 统一了 DI (数字输入) / DO (数字输出) 的访问接口，屏蔽底层硬件差异：

- **默认版本**: GPIO 直驱 (`GpioBank`)
- **F3/F4 版本**: I2C MCP23017 扩展 (`IoExtender`)

```rust
pub trait DigitalIo: Send + Sync {
    fn di_count(&self) -> usize;            // DI 通道数
    fn do_count(&self) -> usize;           // DO 通道数
    fn read_di_all(&self) -> AppResult<u64>;        // 读所有 DI
    fn read_di(&self, idx: usize) -> AppResult<bool>;  // 读单路 DI
    fn write_do_all(&self, value: u64) -> AppResult<()>; // 写所有 DO
    fn write_do(&self, idx: usize, on: bool) -> AppResult<()>; // 写单路 DO
    fn read_do_cached(&self) -> u64;        // 读 DO 缓存 (快)
    fn read_do_actual(&self) -> AppResult<u64>;   // 读 DO 实际 (慢, 诊断用)
}
```

### Hal::dio() 方法

`Hal` 提供 `dio()` 方法返回 `&dyn DigitalIo`，编译期自动选择实现：

```rust
impl Hal {
    pub fn dio(&self) -> &dyn DigitalIo {
        #[cfg(not(any(feature_f3, feature_f4)))]
        { &self.gpio }      // 默认: GpioBank
        #[cfg(any(feature_f3, feature_f4))]
        { &self.io_ext }    // F3/F4: IoExtender
    }
}
```

上层 (`io/di.rs`, `io/do_.rs`) 通过 `hal.dio()` 统一访问，移除了 `#[cfg]` 分支。

### 实现细节

| 实现 | di_count | do_count | read_di_all | write_do_all | read_do_cached |
|------|----------|----------|-------------|--------------|----------------|
| `GpioBank` (默认) | 8 | 8 | 逐通道 GPIO 读 | 逐通道 GPIO 写 | `do_cache` (Mutex\<u64\>) |
| `IoExtender` (F3) | 16 | 16 | 1 片 MCP23017 I2C 读 | 1 片 MCP23017 I2C 写 | `do_cache` (Mutex\<u64\>) |
| `IoExtender` (F4) | 48 | 16 | 3 片 MCP23017 I2C 读 | 1 片 MCP23017 I2C 写 | `do_cache` (Mutex\<u64\>) |

**GpioBank DO 缓存**: `PinDriver<Output>` 不支持 `is_high()` 读回，用 `do_cache: Mutex<u64>` 记录已写入电平。`write_do_all()` / `write_do()` / `do_write()` 均同步更新缓存。

**trait object 开销**: vtable 调用约 1-2ns，相对 I2C 600μs (F4 3 片 MCP23017) 完全可忽略。

### 文件清单

| 文件 | 改动 |
|------|------|
| [src/hal/digital_io.rs](../src/hal/digital_io.rs) ★ 新建 | `DigitalIo` trait 定义 (8 个方法) |
| [src/hal/gpio.rs](../src/hal/gpio.rs) | `GpioBank` 实现 `DigitalIo` (默认版本) + 新增 `do_cache` 字段 |
| [src/hal/io_ext.rs](../src/hal/io_ext.rs) | `IoExtender` 实现 `DigitalIo` (F3/F4, 委托现有方法) |
| [src/hal/mod.rs](../src/hal/mod.rs) | 新增 `pub mod digital_io` + `dio()` 方法 |
| [src/io/di.rs](../src/io/di.rs) | 移除 `#[cfg]` 分支，统一用 `hal.dio().read_di_all()` |
| [src/io/do_.rs](../src/io/do_.rs) | 移除 `#[cfg]` 分支，统一用 `hal.dio().write_do_all()` |

## 通信协议插件化 (阶段七 - H)

### Protocol trait + ProtocolRegistry

统一 Modbus RTU/TCP、BLE Mesh 等通信协议的启动/停止/状态查询接口：

```rust
pub trait Protocol: Send + Sync {
    fn name(&self) -> &str;
    fn start(&self) -> AppResult<()>;
    fn stop(&self) -> AppResult<()>;
    fn is_running(&self) -> bool;
    fn stats(&self) -> ProtocolStats;
}
```

`ProtocolRegistry` 管理所有已注册协议，支持批量启动 (`start_all`)、按名查找 (`find`)、状态查询 (`stats`)。

### 适配器

每个协议用一个适配器结构体封装现有的启动函数：

| 适配器 | 协议名 | 封装的启动函数 | Feature flag |
|--------|--------|---------------|--------------|
| `ModbusRtuProtocol` | `modbus-rtu` | `modbus::start_rtu(hal)` | `feature_modbus_rtu` |
| `ModbusTcpProtocol` | `modbus-tcp` | `modbus::start_tcp()` | `feature_modbus_tcp` |
| `BleMeshProtocol` | `ble-mesh` | `blemesh::start(hal, nvs)` | `feature_ble_mesh` |

适配器持有 `Arc<Hal>` (RTU/Mesh) 或无状态 (TCP)，通过 `ProtocolState` 管理运行标志 (`AtomicBool`)、启动时间 (`Mutex<Option<Instant>>`)、错误计数 (`AtomicU32`)。

### main.rs 集成

```rust
let mut protocols = protocol::ProtocolRegistry::new();
// BLE Mesh 先启动 (ble_at 依赖 BLE 协议栈)
protocols.register(Box::new(protocol::BleMeshProtocol::new(hal.clone())));
protocols.find("ble-mesh").unwrap().start()?;
ble_at::start()?;
// Modbus 注册 + 批量启动
protocols.register(Box::new(protocol::ModbusRtuProtocol::new(hal.clone())));
protocols.register(Box::new(protocol::ModbusTcpProtocol::new()));
protocols.start_all()?;
```

`start_all()` 采用**尽力而为策略**：单个协议启动失败仅记日志，不影响其它协议启动。可通过 `protocols.stats()` 查询各协议运行状态。

### 扩展新协议

新增协议只需 3 步：
1. 实现 `Protocol` trait (封装启动/停止逻辑)
2. 在 `main.rs` 中 `protocols.register(Box::new(...))`
3. 无需修改其它代码

### 文件清单

| 文件 | 改动 |
|------|------|
| [src/protocol/mod.rs](../src/protocol/mod.rs) ★ 新建 | `Protocol` trait + `ProtocolRegistry` + `ProtocolStats` + 3 个适配器 |
| [src/main.rs](../src/main.rs) | `mod protocol` + 用 `ProtocolRegistry` 替换直接调用 `modbus::start_*` / `blemesh::start` |




