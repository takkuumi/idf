# 系统持续开发集成 (LOOP.md)

> 最后更新: 2026-08-13 (LOOP34: LwIP 内存余量与遥测调度)
> 详细进度: `log/SUMMARY_2026-07-22.md`

## 项目背景

此系统是开发一款基于ESP-IDF的 工业控制系统。
原有一套C++开发的系统（MCA_F16V2_1_F48_BLE），运行不稳定，现基于 rust + esp-idf 重构。

- ESP-IDF 源码: `/Users/takumi/Workspace/esp-idf` (禁止修改)
- 原 C++ 系统: `/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE` (禁止修改)
- 手持机源码: `/Users/takumi/Workspace/metuory-wireless-management-app-1.0.78` (禁止修改)

## LOOP34 LwIP 内部 SRAM 余量与遥测调度（2026-08-13）

- [x] 启用 `CONFIG_SPIRAM_TRY_ALLOCATE_WIFI_LWIP`，LwIP 通用动态对象优先使用
  PSRAM、失败时回退内部 SRAM；W5500 DMA 与任务栈的内存属性保持不变。
- [x] 真实设备完成 8 并发客户端、400 次标准最大 FC03（125 words）回归，
  `400/400` 成功；四个 Modbus TCP 端口均参与测试。
- [x] 同期完成 20 次认证 Web 状态查询，`20/20` 成功，无 W5500/LwIP、panic、
  stack canary、pthread 或复位异常。
- [x] 内部 SRAM 历史最低值由约 `6.3KB` 提升到约 `39KB`，为 7x24 运行中的
  瞬时网络分配、ISR/DMA 和系统任务保留了更可靠的余量。
- [x] 全部 7 个用户任务高水位满足 `min_free >= 4300B`、`used <= 40%`，本轮不
  继续缩栈，避免短时测量不足以覆盖 OTA、BLE 重连和异常恢复路径的风险。
- [x] 栈遥测由健康时逐任务多行输出改为单行汇总；低于 1024B 或使用率达到 90%
  的任务仍逐项 ERROR，消除 debug 串口日志制造的约 66ms 调度尖峰。

## LOOP33 实机热点定向收敛（2026-08-13）

- debug 九阶段遥测在 8 连接、400 次 FC03 125 words + Web 查询压力下定位到：
  DI 峰值 `14,064us`、TCP 峰值 `12,254us`，首分钟产生 181 次 5ms deadline miss；
  BLE 仅 `282us`、AI/AO `848us`，不是当前瓶颈。
- F16 PCA9555 DI 扫描此前每 20ms 执行 2 次输入读 + 2 次 DI LED 写，共 4 个
  软件 I2C 事务。现利用 PCA9555 寄存器自动递增，将双端口输入合并为一次连续读；
  DI LED 仅在原始值变化时执行一次连续双字节写。采样周期、去抖、位序和 LED 语义不变。
- Modbus TCP 全局业务预算由每 5ms 2 个请求收紧为 1 个；所有客户端每轮仍执行
  发送、超时和公平轮转，四端口、8 连接及 FC03 125 words 标准上限不变。
- internal SRAM 当前值或历史最低值低于 32KB 均告警。首轮压力实测当前约 35KB、
  历史最低 18KB，说明不能用约 2MB 的含 PSRAM 总 heap 代替片上内存监控。
- 默认、F3、F4 check、测试编译和 release 构建通过；真实设备同口径压力对比待本
  提交完整重烧后记录。

## LOOP32 实机运行诊断与构建追溯（2026-08-13）

- 使用完整分区命令将 LOOP31 固件写入真实 ESP32-S3 rev 0.2（8MB Flash、2MB
  PSRAM），factory 镜像最终从 `0x20000` 完整加载，NVS 未擦除。
- 四个 Modbus TCP 端口 502/503/504/5002 均完成 FC03 标准最大 125 words 实机
  读取，返回 125 个寄存器且首尾数据一致；非法连续地址正确返回异常响应。
- Web 默认账户登录成功，`getsysteminfo/getnetworkconfig/getportconfig/getiodata/
  getsystemstatus/getnfcstatus` 六个只读接口均返回 HTTP 200，中文 UTF-8 正常。
- 设备连续运行 240 秒，无重启、stack canary、pthread 创建失败、EMFILE 或服务
  停滞；空闲总 heap 稳定约 2054KB，历史最低约 2038KB。RTU1 未连接从站时告警
  已按 10 秒聚合，通信错误计数仍逐次保留。
- 首次调试时误把 `espflash` 返回会话号当作写入完成并提前终止进程，factory 尾段
  未写完，bootloader 正确拒绝该镜像并回退旧 `ota_0`。后续必须等待烧录进程自然
  退出，并以 bootloader 完整加载全部 segment 为成功判据。
- 新增每分钟 internal SRAM 当前/历史最低水位，直接监控 pthread 与 DMA 依赖的
  片上内存；debug 固件增加 main-loop 九阶段峰值耗时，release 中编译移除计时开销。
- ESP-IDF App version 固定为 `2.2.1`，Rust 固件另嵌入当前 Git HEAD/dirty 标识，
  避免 CMake 子构建缓存显示旧提交号，提升客户现场镜像追溯能力。
- 本轮没有写配置、线圈或 OTA；BLE 手持机交互、真实 RTU 从站和 OTA 闭环仍需单独
  实机验证，不能由上述只读测试替代。

## LOOP31 协议热路径固定缓冲与复制收敛（2026-08-13）

- Modbus FC03/04 将最多 125 个寄存器直接编码到最终 PDU，移除约 250B 中间
  寄存器数组和第二次序列化；FC03 在一次 CONFIG/STORAGE RCU 快照内完成整批读取。
- BLE TX 改为 `8 x 272B` 固定帧环，ATT 分片只推进帧内 offset，不再执行
  `rotate_left` 或复制到 497B 临时分片；队列满时仍按整帧拒绝并记录丢弃计数。
- BLE RX 改为 `4 x 512B` 静态重组槽，BTC 回调完成帧后仅向 main-loop 传递
  1B 槽索引；连接 ID、连接 epoch、CRC 和断线清理规则保持不变。
- Web 方法、路径、Cookie、请求行、header 行和表单解码改为固定容量；只保留业务
  使用的 Cookie/Content-Length。普通 body 仍按声明长度读取但硬限 16KB，OTA 保持流式。
- 未修改寄存器地址、Modbus PDU/MBAP、BLE 外层帧/事务号/CRC、Web 路由/字段名、
  NFC、OTA、IO 或持久化格式，手机、PC、TCP、RTU 的协议表面保持不变。
- 默认、F3、F4 `cargo check`、测试编译和 release 构建通过，0 warning；release
  应用镜像 `1,637,648B`，占 2.25MB OTA 槽约 69.4%，较 LOOP30 增加 10,720B；
  静态内部 DRAM `.data + .bss = 53,501B`，较 LOOP30 减少 352B。
- 本轮未烧录。BLE 手持机、Web、Modbus TCP/RTU 的真实设备回归及 72 小时并发浸泡
  仍是客户发布前硬门槛，构建通过不代表这些实机项目已完成。

## LOOP30 工业架构资源边界收口（2026-08-13）

- `build.rs` 对真实 ESP32-S3R2 硬件建立构建硬门槛：2MB PSRAM、64KB internal
  reserve、20 sockets、DIO/40MHz/8MB Flash、旧 NVS 地址和三个 2.25MB 应用分区。
  任一关键配置漂移都会中止构建，防止未经实机验证的配置进入客户固件。
- Modbus TCP 架构文档与当前实现统一：四监听端口、8 个固定客户端、260B 标准最大
  ADU、5ms 非阻塞轮询、当前每 tick 全局 1 个请求、keepalive 与监听器退避恢复。
- 完整业务 release 应用镜像实测 `1,626,928B`，约占单 OTA 槽 69.0%；静态内部
  DRAM `.data + .bss = 53,853B`。交付文档增加 OTA 槽容量硬检查和体积回归基线。
- 默认、F3、F4 编译检查与 `cargo test --bin gateway --no-run` 通过；本轮不修改
  BLE/Modbus/Web/NFC/OTA 协议和寄存器映射。

## LOOP29 实机纠偏（2026-08-13）

- 真实设备旧分区表只有 `factory@0x10000`，没有 `ota_0/ota_1`，Web OTA 因此返回 500；新布局保留旧 `nvs@0x9000`，增加 `factory/ota_0/ota_1` 和 `otadata`。
- 板载 NFC 实测 `IC_REF=0x26`、`MEM_SIZE=0x01FF`、`BLOCK=0x03`，器件为 ST25DV16KC（2KB），原 C++ 也以 `0x07FF` 为末地址。LOOP12 的 64KC/`0x1FFF` 扩容结论错误并已撤销，快照严格恢复为 `0x0120..0x07FF`、1760 字节、880 words。
- NFC 线程保留已探测的 I2C 实例；空快照只完整读取和记录一次，配置变更、显式备份/恢复仍立即执行，通信错误按 30/120/300 秒退避。
- 截至本记录，编译与短期实机 Web/Modbus/OTA 轮换曾通过；2026-08-13 当前串口未连接，新的兼容 NVS 分区迁移尚待设备重新接入后完成最终烧录与 OTA 闭环复测。

## 系统迭代 - 5 角色

| 角色 | 职责 |
|------|------|
| 产品经理 | 对照 MCA_F16V2_1_F48_BLE + metuory-wireless-management-app-1.0.78 提出缺失功能 |
| 高级 Rust 开发 | 实施功能与修复 BUG |
| 高级测试 | 测试 + 提出问题 |
| 高级系统架构 | 架构把关 (`docs/ARCHITECTURE.md`) |
| 工业软件审计 | 审计每次实施 |

## 任务完成清单

| # | 任务 | 状态 | 关键产出 |
|---|------|------|----------|
| 1 | heapless 升级 0.9.3 | ✅ | `Cargo.toml` |
| 2 | 手持机显示 IP/MAC/BLE_ID | ✅ | `handle_ble_android_read_command` (LOOP2) |
| 3 | 硬件信息确认 | ✅ | `docs/pinmap.md` 重写 (ESP32-S3R2) |
| 4 | 无锁测试 + 栈估算 | ✅ | 固定任务预算 + Modbus 位图热路径去除 2KB 临时栈 |
| 5 | Modbus TCP/RTU 协议兼容 | 🟡 | 编译与帧级回归通过；待目标设备 + tauri-app 实机验证 |
| 6 | log/ 目录 + 详细日志 | ✅ | 7 子目录 + SUMMARY |
| 7 | mesh 清理 | ✅ | 死代码已删 |
| 8 | 引脚核对 | ✅ | pinmap.md 1:1 对齐 |
| 9 | 性能测试 | ✅ | Modbus TCP 11s 全部响应 |
| 10 | 7×24 不间断运行 | 🟡 | 1 小时长稳测试中 |
| 11 | 5 角色协作 | ✅ | 完整推进 |
| 12 | **BLE ID 写入 0x08E2 路由修复** | ✅ (LOOP3) | commit `d9654f7` |
| 13 | **MBAP.length 兼容 pymodbus** | ✅ (LOOP3) | commit `22a6e0a` |

## LOOP3 关键修复 (2026-07-23)

### 问题 1: BLE ID 写入 0x08E2 被错误路由到 BLE MAC

**根因**:
- metuory 1.0.78 WRITE_BLUETOOTH_ID (0x51) 通过 BLE Modbus FC=10 写 `0x08E2` (4 寄存器)
- 期望更新 `cfg.ble_name` (蓝牙 ID 显示字段)
- 旧代码 `HOLD_BT_ADDR_BASE = 0x08E2` 把 0x08E2 路由到 `cfg.ble_mac`
- 写入被错误地修改 BLE MAC, 而 `ble_name` 永远不变
- 读取看似正常, 是因为 `handle_ble_android_read_command` 自定义读 handler 直接返回 `cfg.ble_name`

**修复** (`d9654f7`):
- `HOLD_BLE_NAME_BASE: 4000 → 0x08E2` (metuory 期望地址)
- `HOLD_BT_ADDR_BASE: 2274 → 0x0FA4` (用户区 4004, 保留兼容)
- `read_reg/write_reg`: BLE_NAME 优先 BLE_MAC 检查
- BLE_NAME 写返回 `Persist` (而非 Apply)
- 6 个新增回归测试: `test_ble_name_at_metuory_addr_is_persist` 等

### 问题 2: MBAP.length 响应字段不包含 unit_id, pymodbus 3.x 解析失败

**根因**:
- Modbus TCP 标准 (Modbus_Application_Protocol_V1_1b3 §4.1) 规定:
  `MBAP.length = unit_id(1) + func(1) + data(N) = 2 + N`
- 旧实现 `mbap_len = resp_pdu.len() (= 1+N)` 导致 pymodbus 3.8.6 解析时
  把 func 误认为 unit_id, 报错 `Unable to decode frame: byte_count N > length of packet N`

**修复** (`22a6e0a`):
- `src/modbus/tcp_server.rs: mbap_len = 1 + resp_pdu.len()`
- pymodbus 3.8.6 验证: 5 个 RS485 寄存器读正确 (0x3000, 1, 0, 1000, 20)

## LOOP4 metuory 读路径 length-prefix 格式 (2026-07-23)

### 背景
metuory Android 端所有 `parseXxxItem` 使用 length-prefix 格式:
```
buffer[0] = length
data[1..1+length] = 实际数据
```
但本系统 FC=03/04 走标准 Modbus 路径返回 `[func][byte_count][data]` 格式
导致 metuory 解析失败 → UI 显示空/异常

### 修复 (commit a10a9ef, src/ble_at/mod.rs)
在 `handle_ble_android_read_command` 新增 5 个读 handler:

| 命令 | 地址 | FC | 响应格式 |
|------|------|-----|----------|
| READ_SN         | 0x0894, 9 | 0x03 | [18][SN bytes] |
| READ_LOCATION   | 0x089D, 8 | 0x03 | [16][location bytes] |
| READ_ADC        | 0x0080, n | 0x04 | [2n][BE u16 × n] |
| READ_COM_INPUT  | 0x0000, n | 0x02 | [n/8 bytes][packed bits] |
| READ_COM_OUTPUT | 0x0200, n | 0x01 | [n/8 bytes][packed bits] |
| READ_RS485_VALUE       | 0x1000+, n | 0x04 | [2n][BE u16 × n] |
| READ_RS485_CUSTOM_VALUE| 0x1000+, n | 0x03 | [2n][BE u16 × n] |

### 端到端验证 (11/11 全部通过)
- SN='ESP32S3-UNKNOWN-00' (18 bytes ASCII)
- LOCATION='GW-ESP32S3' (16 bytes ASCII)
- BT_ID='Mesh' (8 bytes ASCII)
- MAC=80:B5:4E:5B:24:E7
- IP=192.168.51.140 / 255.255.255.0 / 192.168.51.1
- FW=00DD0615 (2.2.1.1557) HW=10100402 (F16/16DI/16DO/4AI/2RS485)
- ADC=4 通道, COM I/O=16 位, RS485 idx0=5 寄存器

### 新增测试 (5 个回归测试 + 5 个 test helper)
- test_android_parse_sn_length_prefix
- test_android_parse_location_length_prefix
- test_android_parse_adc_length_prefix
- test_android_parse_com_input_length_prefix
- test_android_parse_rs485_value_length_prefix

### 已知问题 (与本 LOOP 独立)
Modbus TCP 写响应连接重置 (READ 路径正常, 写成功后响应未送达)
源: TCP 服务器 write_all/flush 失败
影响: metuory 写入后无回执, 但数据已落 RCU → 下次读仍能看到新值
LOOP5 待排查 W5500/lwIP + std::net::TcpStream 在 RCU RMW 后的 write 行为

## LOOP5 Modbus TCP 写 panic 修复 (2026-07-23)

### 症状
- FC=06/10 写后: 数据落 RCU 但 response 连接被 RST
- READ 路径完全正常
- 异常响应 (ILLEGAL_DATA_ADDRESS) 正常
- 关键错误: Guru Meditation Error: Core 0 panic'ed (Unhandled debug exception)

### 根因 (深度诊断)
handle_conn (12KB 栈) → write_hold_reg → config_clone() → 触发栈溢出
```
[back] write_hold_reg START addr=0x08bf value=0xaa55
[back] write_hold_reg BEFORE config_clone
Guru Meditation Error: Core  0 panic'ed (Unhandled debug exception)
esp_core_dump_flash: Save core dump to flash...
... 设备重启
```

`config_clone()` 克隆 ConfigSnapshot 含 DeviceConfigTable:
- `heapless::Vec<DeviceEntry, 32>` 32 元素
- 每元素又含 `heapless::Vec<u16, 32> params` 32 元素
- 递归 clone 触发大量栈分配 (估算 >3KB)
- 12KB 栈不够 → Xtensa Unhandled debug exception → 核心转储 → 设备重启
- W5500 TCP 表现为连接被 RST (而非正常 FIN)

### 修复 (commit 25e83f4)
**src/modbus/tcp_server.rs**: TCP 连接线程栈 12KB → 20KB
```rust
.stack_size(20 * 1024) // LOOP5 修复: 12KB 不足以容纳 config_clone 递归克隆
```

### 顺手修复 (LOOP5 关联)
**src/device/system_config.rs**:
1. Rs485Config 漏存 retry_count/timeout_ms/interval_ms (Word3-5)
   旧代码 `2..=4 => {}` 跳过 → NVS 序列化时丢失
2. encode/decode 扩展到 15 字节/entry, 加 retry/timeout/interval
3. OFF_RS485_1: 101→107 修复与 OFF_RS485_0+9 重叠
4. read_reg 同步读取 retry/timeout/interval (不再是常量 0/1000/20)

### 端到端验证 (15/15 路径 + NVS 持久化)

| 路径 | 修复前 | 修复后 |
|------|-------|--------|
| READ_SN/LOCATION/MAC/BT_ID/Product/IP/FW/HWInfo/ADC/COM I/O/RS485 | ✓ | ✓ |
| WRITE_SN/Location/IP/BT_ID/RS485 | ✗ (panic) | ✓ |
| NVS 持久化 (写后重启读) | ✗ (Word3-5 丢) | ✓ (完整 5 字) |

示例 NVS 持久化验证:
- 写 RS485: 460800/N/1S/8D/Master/slave=42/retry=5/timeout=800ms/interval=100ms
- 重启后读: 0x9000, 0x2A, 0x0005, 0x0320, 0x0064 → 完全一致 ✓

### 单元测试
- LOOP4 length-prefix 5 个回归测试编译通过
- RS485 全 5 字段 round-trip 测试通过
- `cargo test --bin gateway --no-run` 编译通过 (19 个 warning, 0 error)

## 待解决问题

### BLE 写入 (metuory → 0x08E2) 仍需端到端验证
- 代码修复 ✅, 单元测试 ✅, BLE 路径需手持机实测
- metuory 写入流程: WRITE_BLUETOOTH_ID (0x51) → BLE Modbus FC=10 → `try_handle_binary_protocol` →
  `handle_modbus_rtu` → `write_multi_regs_pdu` → `backends::write_hold_reg(0x08E2+i, v)`
- 修复后: 写入 `cfg.ble_name[0..8]` (BE), 返回 `Persist`, actor 异步落盘 NVS

## 架构 (Phase 2 完成)

```
main_loop (5ms network tick)
├── tick_tcp_server()            # 5ms, 4 端口/8 连接非阻塞状态机
├── tick_di_scan(&hal)           # 20ms, 合并 di-scan pthread
├── tick_do_output(&hal)         # 5ms dirty 消费 + 10s 兜底
├── tick_ai/ao(&hal)             # 100ms
├── ble_at::process_tick()       # 10ms
└── tick_eth_heartbeat()         # 5s

3 个核心协议/状态 pthread 任务:
- DeviceActor (NVS 持久化, 16KB 栈)
- mb-rtu-master (Modbus RTU 主站)
- mb-rtu-slave (Modbus RTU 从站)
```

详细架构: [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)

## 注意事项

- 烧录: `espflash`, 串口 `/dev/cu.usbserial-1430`, 强制重置
- 禁止修改 esp-idf / MCA / metuory 源码
- 严禁抄袭 MCA/metuory 代码 (只参考业务)
- 所有决策需要我审批时 (用户睡觉中) 自动处理

## 烧录

```bash
just flash
```

详细: [`docs/FLASH.md`](FLASH.md)

## 测试日志

`log/` 目录:
- `log/README.md` - 测试矩阵

## LOOP26 栈容量架构重构 (2026-07-29)

- [x] 最终 map 核对：DRAM 段 333.75KB，静态 data+bss 53.86KB，连续 heap 候选 168.12KB。
- [x] Modbus TCP 最终并入 20ms main-loop，管理 4 端口/8 连接，不再申请 pthread 栈。
- [x] BLE GATT 回调只重组和入队，业务迁移到 main_loop，保持 metuory 1.0.78 帧兼容。
- [x] DeviceActor blob 改固定堆缓冲，任务栈 32KB 降到 16KB。
- [x] OTA/AT/Web 延迟复位不再创建一次性 12KB pthread。
- [x] 所有生产线程使用 `safety::stack_budget`，全功能用户栈编译期上限 128KB。
- [x] 真实 TaskHandle 高水位每 60s 上报，修复原监控把最小值错误取 MAX 的缺陷。
- [x] `cargo build`、默认 `cargo check`：通过，0 warning。
- [ ] 真机 72h 峰值浸泡：BLE 手持机 + 8 TCP + RTU + Web OTA/NFC；见测试日志。

## LOOP27 工业服务链路重塑 (2026-07-29)

- [x] BLE 下行按协商 MTU-3 分片，拥塞/提交失败保留队列，协议帧和 CRC 不变。
- [x] 对照手机 1.0.78 确认 `CommandCodecUtil` 支持跨 notification 重组。
- [x] 正常 BLE 广播不再每 15 秒 stop/start；改名使用 GAP stop-complete 状态链。
- [x] TCP 2243-2246 端口进入 NVS 配置并支持运行时监听器重绑，长连接 idle 为 5 分钟。

## LOOP28 真机 ENOMEM / UDP / NFC 闭环 (2026-07-30)

- [x] 根据真机日志确认单个 16KB `mb-tcp` 任务仍因 internal SRAM 不连续而 ENOMEM。
- [x] 取消 `mb-tcp` pthread；默认/全功能用户任务栈由 106/120KB 降为 90/104KB。
- [x] 主调度改为 20ms，修复此前注释为 20ms、实际为 500ms 的 DI 扫描周期。
- [x] 修复 UDP `ip_mreq` 小端内存字节序，消除组播加入错误 125。
- [x] 软件 I2C 恢复约 100kHz，NFC 两个 4KB 工作区显式迁移到 PSRAM。
- [x] `MainLoopCell` 改为闭包借用，消除 `&self -> &mut T` 未定义行为。
- [x] 默认/Wi-Fi/F4 check、测试目标编译、固件 build 均通过且 0 warning。
- [ ] 用本次固件复测 TCP 四端口、UDP 入组、NFC 完整 4096B 写入及 72h 浸泡。
- [x] Web OTA 使用 `BufReader` 原流式读取，1 秒读超时持续喂 WDT，上传上限 0x240000。
- [x] OTA 读取真实 running/next partition，30 秒全服务健康后才取消回滚。
- [x] NFC 使用 ST25DV DATA/SYSTEM 双地址、17 字节密码流程、4096B 快照和 Type-5 NDEF。
- [x] NFC 写失败不再误报 BackedUp 或清除 holding dirty；spawn 失败允许 supervisor 重试。
- [x] Web 会话服务端 TTL 与 Cookie Max-Age 同为 24 小时，增加 SameSite=Strict。
- [x] UDP 配置变化自动重建 socket；Web socket 参数失败不会落入无超时阻塞。
- [x] 默认/Wi-Fi/F4 `cargo check`、`cargo build`、abort test compile：通过，0 warning。
- [ ] 手持机/TCP/NFC/Web/OTA 并发真机功能与 72h 浸泡（需维护窗口烧录）。

## LOOP7 BLE 名字 GAP 同步 (2026-07-24) - COMPLETE

### 问题
Metuory 写完蓝牙 ID 后搜不到设备. 之前以为是路径覆盖不全 (LOOP4 修复了 length-prefix),
但实测仍搜不到 — 根本原因是 GAP device name 没更新.

### 根因 (LOOP7 真因)
`SystemConfig::ble_name_str()` 实现 bug:
```rust
let end = self.ble_name.iter().position(|&b| b == 0).unwrap_or(8);
String::from_utf8_lossy(&self.ble_name[..end]).to_string()
```
UTF-16 BE 编码下每个字符高字节 = 0x00, `position(&b == 0)` 立即命中 idx=0, 返回空字符串.
`update_gap_device_name` 拿空名调 `esp_ble_gap_set_device_name` → 失败 → 广播名字不变.

### 修复 (commit 6c51359)
1. `ble_name_str` / `sn_str` / `name_str` 全部改为按 BE 字节序手动解码:
   ```rust
   while i + 1 < self.ble_name.len() {
       let hi = self.ble_name[i]; let lo = self.ble_name[i + 1];
       if hi == 0 && lo == 0 { break; }
       if let Some(c) = char::from_u32(((hi as u32) << 8) | (lo as u32)) {
           chars.push(c);
       }
       i += 2;
   }
   chars.into_iter().collect()
   ```
2. GAP 同步机制 (src/ble_at/mod.rs):
   - `static PENDING_GAP_NAME_UPDATE: AtomicBool`
   - `notify_ble_name_changed()` 写完置 true
   - `process_tick()` swap 后调 `update_gap_device_name()`
   - `update_gap_device_name()` 调 `esp_ble_gap_set_device_name`
3. AT 命令路径 (cfg_handlers.rs): `handle_cfgtbtname` 写完也调 `update_gap_device_name()`

### 验证
- Python 模拟: `'XY'` (UTF-16 BE) 修复前→`''` 修复后→`'XY'` ✓
- Python 模拟: `'Mesh'` 修复前→`''` 修复后→`'Mesh'` ✓
- 设备实测 Modbus FC=10 写 0x08E2='XY': cfg.ble_name 读回 `'XY'` ✓
- GAP 设备名更新路径触发, 下次广播用新名字

### 已知问题 (SN/LOCATION 历史数据)
NVS 中残留旧 LE 编码的 SN 数据. 修复后用 BE 解码读 LE 字节会出现乱码.
解决: 重新通过 Modbus/AT 写一次新值即可覆盖. 不影响当前修复.

## LOOP8 架构彻底无锁化 + 7×24 可靠性加固 (2026-07-24) - COMPLETE

### 三大目标 (本 LOOP 完成)
1. **推进系统架构彻底摆脱锁、实现完整无锁化** — BLE AT 热路径 Spin → 原子
2. **行完整的性能测试、确保系统高性能稳定运行** — RCU 多写者修复 + heap 缓解
3. **系统高稳定、高可靠性、支持 7×24 不间断运行不宕机** — 看门狗全覆盖 + 内存监控

### 修复 1: BLE AT 热路径 Spin → 原子化 (无锁化)
**根因**: 3 个 Spin 锁在 BLE 热路径 (每 GATT 回调 + 每 process_tick 100ms) 上竞争:
- `HANDLE_TABLE: Spin<[u16; 4]>` — 属性表 handle
- `GATTS_IF: Spin<Option<esp_gatt_if_t>>` — GATT 接口号
- `CONN_ID: Spin<Option<u16>>` — 连接 ID

**修复** (`src/ble_at/mod.rs`):
| 旧 (Spin) | 新 (原子) | sentinel |
|-----------|----------|----------|
| `HANDLE_TABLE: Spin<[u16;4]>` | `[AtomicU16; 4]` | 0 = 未分配 |
| `GATTS_IF: Spin<Option<i16>>` | `AtomicI16` | -1 = None |
| `CONN_ID: Spin<Option<u16>>` | `AtomicU16` | 0xFFFF = None |

- 所有 `.lock()` 调用改为 `.load(Acquire)` / `.store(Release)`
- 消除 process_tick + GATT 回调中的 Spin 争用, 真无锁

### 修复 2: ble-at 心跳从未 tick
**根因**: `TASK_HB` 在 ble_at/mod.rs:104 注册但 `process_tick()` 从未调用 `TASK_HB.tick()`,
导致 "ble-at" 任务总被 `check_all()` 误判为停滞 (3s 后)。
**修复**: 在 `process_tick()` 开头加 `TASK_HB.tick();`

### 修复 3: TCP CONN_COUNT 永久泄漏 (spawn 失败)
**根因**: `tcp_server.rs` 用 `.ok()` 忽略 `thread::spawn` 结果。
若 spawn 失败 (OOM/资源耗尽), `CONN_COUNT` 已 `fetch_add(1)` 但永不 `fetch_sub(1)`,
**永久泄漏连接名额**, 最终 4 个名额全部被占死。
**修复**:
- spawn 失败时 `CONN_COUNT.fetch_sub(1)` 回滚
- 用 `compare_exchange` 防 TOCTOU 竞态 (4+ 并发 accept 同时通过 cur < MAX 检查)
- 连接线程订阅硬件 WDT + `catch_unwind` 保证 `fetch_sub` 总执行
- 每循环喂狗, 防止连接死锁

### 修复 4: RCU 多写者并发数据丢失 (CRITICAL)
**根因**: `Rcu::write()` 假定单写者, 但实际有 5 个并发写者:
- Modbus TCP 4 连接 (MAX_CONNECTIONS=4)
- Modbus RTU master/slave
- DeviceActor (NVS commit/reload/apply)
- main_loop DHCP 写回

两线程同时 `config_clone()` → 各自修改 → `CONFIG.write()`, 后写覆盖前写 = **丢失更新**。
还会导致 `retire_queue` (4 槽) 溢出 → `mem::forget` 泄漏 ~11KB StorageSnapshot。
**修复** (`src/bus/backends.rs`):
- 新增 `RCU_WRITE_LOCK: Spin<()>` 串行化所有 RMW (clone-mutate-write)
- 覆盖 `write_hold_reg` / `storage_modify` / `config_modify_with_result` / `storage_set_snapshot`
- 持锁时间 = clone + mutate + atomic swap, 约数十微秒 (<< Modbus 响应超时 2s, 不阻塞)
- 读路径完全不受影响 (RCU read 无锁)

### 修复 5: 看门狗全覆盖 + 停滞升级重启
**根因**: 仅 main_loop 订阅硬件 WDT, 11 个其他任务只有软心跳。`check_all()` 检测到停滞
仅 `log::warn`, **从不触发重启**, 死锁任务导致系统无响应。
**修复** (`src/main.rs`):
- `STALL_COUNT: AtomicU32` 跟踪连续停滞次数
- 连续 5 次 (5s) 检测到停滞 → `esp_restart()` 强制恢复
- 连接线程订阅硬件 WDT (tcp_server.rs)
- 每 60s 打印 `free_heap` / `min_heap` (7×24 运维监控)
- heap < 20KB 时 LOW MEMORY 告警

### 修复 6: ringlog 49.7 天时间戳 wrap
**根因**: `timestamp_ms: u32 = as_millis() as u32` 在 ~49.7 天后 wrap, 跨边界日志无法排序。
**修复** (`src/error/ringlog.rs`):
- `timestamp_ms: u32` → `timestamp_s: u32` (秒级, ~136 年不 wrap)
- `backends.rs` 读端同步更新

### 修复 7: 以太网心跳真实实现
**根因**: `heartbeat_once()` 始终返回 `true`, **从不检测链路故障**, 整个以太网降级路径是死代码。
**修复** (`src/ethernet/w5500.rs`):
- 用 `esp_netif_get_handle_from_ifkey("ETH_DEF")` + `esp_netif_get_ip_info`
- IP == 0 → 链路故障 (false); IP != 0 → 链路正 (true)

### 修复 8: heap 碎片缓解
**根因**: `request_save_device_text()` 每次 `storage_read()` clone 整个 ~11KB
StorageSnapshot 到 Arc, 每次 device_text 写入都产生 11KB heap churn。
**修复** (`src/device/mod.rs`):
- 改用 `storage_read_with(|s| save_device_text_to_nvs(&s.device_text))`
- 仅持 RCU reader 访问, 不 clone 整个快照, 消除 11KB 临时分配

### 锁状态盘点 (LOOP8 后)

| 类型 | LOOP2 | LOOP8 | 状态 |
|------|-------|-------|------|
| `std::sync::Mutex::new` | 0 | **0** | 完全消除 |
| `parking_lot::Mutex` | 0 | **0** | 完全消除 |
| BLE AT 热路径 Spin (HANDLE/GATTS_IF/CONN_ID) | 3 | **0** (→ 原子) | 真无锁 |
| HAL 驱动 Spin (PinDriver/LEDC/ADC/I2C) | 11 | 11 | 不可原子化 (&mut 要求) |
| NVS/OTA/RingLog/BUFFER Spin | 5 | 5 | 短临界区非 park |
| `Atomic*` | 高 | **更高** (+3) | 真无锁 |
| `MainLoopCell` | 5 | 5 | 单线程零开销 |
| `RCU_WRITE_LOCK` (新) | 0 | 1 | RMW 串行化 (非热路径) |

**结论**: 所有可在热路径上原子化的 Spin 已消除。剩余 Spin 全部保护 ESP-IDF 硬件驱动
句柄 (`PinDriver`/`LedcDriver`/`AdcDriver`/`I2cBus` 等), 它们本质上需要 `&mut self`,
**无法用原子替代**。这些 Spin 都是**微秒级短临界区, 非 park, 不阻塞 OS 调度**,
且在冷/温路径上, 不影响 7×24 稳定性。

### 待验证 (用户烧录后)
1. 4 并发 Modbus TCP 无数据丢失 (RCU 写者串行化生效)
2. uptime 60min+ 无 Guru Meditation
3. heap 使用量稳定 (无持续增长, 碎片缓解生效)
4. ble-at 心跳正常 (不再误报停滞)
5. 以太网网线拔出 → heartbeat_once 返回 false → 降级路径激活

### 烧录命令
```bash
cargo build --bin gateway
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
espflash reset --port /dev/cu.usbserial-1430
```

## LOOP9 全链路审计 + 关键 bug 全面修复 (2026-07-25) - COMPLETE

### 审计方法
4 个并行审计 agent 分别覆盖: pthread 堆分配、Modbus 全链路、BLE/网络/IO 链路、Web 服务器。
审计后逐一修复 P0/P1 问题，补充回归测试，最终 **0 error / 0 warning** 编译通过。

### 修复清单 (14 项)

| # | 严重度 | 文件 | 问题 | 修复 |
|---|--------|------|------|------|
| 1 | P0 | `web/mod.rs` | OTA body 一次性 `vec![content_length]` → OOM panic | 流式 4KB chunk 读取 + `MAX_OTA_SIZE=2MB` 上限 |
| 2 | P0 | `web/mod.rs` | HTTP header/body 无长度上限 → DoS OOM | `MAX_HEADER_LINE=1024`, `MAX_BODY_SIZE=512KB` |
| 3 | P0 | `udp_multicast/mod.rs` | `from_ne_bytes` 字节序错误 (LOOP9 旧版方向反) | 改回 `from_be_bytes` (网络字节序) |
| 4 | P0 | `udp_multicast/mod.rs` | `read_switch_status` 边界检查 `&&` 应为 `||` | 修正为 `||` |
| 5 | P0 | `bus/backends.rs` | ringlog 地址 0x0887-0x08A6 被 CFG_BASE catch-all 遮蔽, FC=03 恒返回 0 | ringlog 检查提前到 CFG 块内 SystemConfig 之前 |
| 6 | P1 | `modbus/shared.rs` | FC=0F/FC=10 缺少 count 范围校验 (count=0 返回成功) | 添加 `count==0 \|\| count>MAX` → ILLEGAL_DATA_VALUE |
| 7 | P1 | `nfc/mod.rs` | `vec![0u8;1760]` 每次轮询堆分配 (违反 AGENTS.md) | 改为栈数组 `[0u8; NFC_BLOB_DATA_BYTES]` |
| 8 | P1 | `nfc/mod.rs` | `regbuf_equal` 长度不匹配 (2048 vs 880) 恒 false → 每次轮询都 restore | 改为比较前 min(len) 个 word |
| 9 | P1 | `nfc/mod.rs` | `bytes_to_words` 返回 `Vec<u16>` (堆分配) | 改为写入调用方栈缓冲 `&mut [u16]` |
| 10 | P1 | `ble_at/logic_handlers.rs` | D1 未配置响应 cmd=0xD0 (应为 0xD1, 复制粘贴错误) | 修正为 `build_ack(0xD1, sub, 0x84)` |
| 11 | P1 | `ble_at/mod.rs` | 0xC2 GET_SN_CODE push 顺序 [lo,hi]=LE (应 BE) | 修正为 [hi,lo] 大端 |
| 12 | P1 | `io/di.rs` | read_di_all 失败时用旧 candidate 推进去抖计数 → 误报 DI 变化 | 失败时直接 return, 不推进计数 |
| 13 | P1 | `channel/ai.rs` | scaled 未使用 calib 校准值 (SENSOR_MIN/MAX 从不读取) | 读取 holding_buf 校准值, 用 `map_range` 映射; 无校准时回退 4-20mA |
| 14 | P2 | `ethernet/w5500.rs` | eth-heartbeat stall 阈值 6 与 5s 周期不匹配 | 调整为 8 (留 3s 余量) |

### 附加修复 (编译警告清零)
- `src/sync.rs:528`: `for i in 0..1000` → `for _ in 0..1000` (unused variable)
- `src/bus/buffer_pool.rs:163`: `assert!(p.available() >= 0)` → `let _ = p.available()` (usize 恒 >=0)
- `src/ble_at/mod.rs:2298`: `use super::*` → `#[allow(unused_imports)]` (测试模块)
- `src/web/mod.rs:369`: `mut stream` → `stream` (不需要 mut)

### 新增回归测试 (9 个)
- `test_map_range_basic` / `test_map_range_clamp` / `test_map_range_div_zero` / `test_map_range_inverted_output` — AI 校准映射
- `test_write_multi_coils_count_zero_rejected` / `test_write_multi_coils_count_too_large_rejected` — FC=0F 校验
- `test_write_multi_regs_count_zero_rejected` / `test_write_multi_regs_count_too_large_rejected` — FC=10 校验
- `test_regbuf_equal` 更新 — NFC 前缀比较语义

### 编译验证
- `cargo build --bin gateway`: **0 error, 0 warning**
- `cargo test --bin gateway --no-run`: **0 error, 0 warning**, 167 个单元测试编译通过
- 测试用例: 167 个 (较 LOOP8 的 127+ 新增 40 个)

### 已知限制 (不在本 LOOP 范围)
- Web 认证仍为硬编码 Cookie (ESPSESSIONID=1), 无 CSRF/签名 — 仅适合内网
- BLE 二进制协议无分片重组缓冲 (>MTU 命令无法解析)
- NFC blob 仅覆盖 holding_buf 前 880 words (后 1168 words 不备份)，与当时误判的容量记录相关，已在 LOOP29 纠正为 ST25DV16KC 实际容量
- AI 通道数硬编码 6 (F4 设备 8 通道需 HAL 层配合)
- eth heartbeat 仅检测 IP 非零, 拔线后 DHCP lease 保留导致检测延迟

### 烧录命令
```bash
cargo build --bin gateway
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
espflash reset --port /dev/cu.usbserial-1430
```

## LOOP10 内存布局对齐审计 + metuory 业务兼容修复 (2026-07-25) - COMPLETE

### 审计方法
2 个并行调研 agent 分别覆盖:
1. metuory Android 端全部业务交互流程 (BLE/Modbus 命令清单 + 响应格式)
2. MCA C++ 参考固件 vs 本系统寄存器地址逐字节对齐审计

审计后针对 P0/P1 问题逐一修复, 补充回归测试, 最终 **0 error / 0 warning** 编译通过.

### 修复清单 (4 项)

| # | 严重度 | 文件 | 问题 | 修复 |
|---|--------|------|------|------|
| 1 | 🔴 P0 | `bus/backends.rs` | LOOP9 引入 ringlog FC=03 拦截, 0x0887-0x08A6 与 SN(0x0894)/PLACE(0x089D)/HW_VER(0x08A5) 地址重叠, 导致 FC=03 读 SN/PLACE 返回 ringlog 条目而非配置值 | ringlog 数据条目 (32 字) 从 `read_hold_reg` (FC=03) 移到 `read_input_reg` (FC=04), FC=03 恢复正常 SystemConfig 读取 |
| 2 | 🔴 P0 | `device/system_config.rs` | `HOLD_HW_VER` 写入返回 `WriteResult::Ok` (仅内存), 与 MCA PRegBuf→`/MODSPRegSaveBuf.bin` 持久化语义不一致 | 改为 `WriteResult::Persist`, 写入后落 NVS |
| 3 | 🟠 P1 | `device/system_config.rs` | 测试 `test_layout_bt_addr_matches_mca` 断言 `HOLD_BT_ADDR_BASE == 0x08E2`, 但实际常量已是 `0x0FA4` (LOOP3 迁移) — 测试必失败 | 重命名为 `test_layout_bt_addr_matches_design`, 断言 0x0FA4 + 验证 `HOLD_BLE_NAME_BASE == 0x08E2` |
| 4 | 🟠 P1 | `ble_at/mod.rs` | `READ_RS485_INDEX_CONFIG` (0x08A6/0x08AB/0x08B0, 5 regs) 落入 generic FC=03 catch-all, 返回 BE u16 列表; metuory `parseRS485ConfigItem` 期望 packed 10-byte struct (`length==0x0a`), 解析失败 → UI 显示空 | 新增 bespoke handler, 按 metuory packed 格式构造: `[0x0a][combo][mode][slave BE][retry BE][timeout BE][interval BE]`, combo = `(baud_idx<<4)\|(parity<<2)\|(stop<<1)\|data` |

### LOOP9 ringlog 遮蔽 bug 深度诊断
- **根因**: LOOP9 修复 #5 "ringlog 地址 0x0887-0x08A6 被 CFG_BASE catch-all 遮蔽" 时, 把 ringlog 检查提前到 `read_hold_reg` CFG 块内 SystemConfig 之前, 但 ringlog 区 32 字 (0x0887..0x08A6) **物理覆盖** SN(0x0894, 9字)/PLACE(0x089D, 8字)/HW_VER(0x08A5, 1字), 导致这些地址 FC=03 读取被 ringlog 拦截
- **影响**: metuory WRITE_SN (0x21) / WRITE_LOCATION (0x31) 写入正常 (写入不走 ringlog), 但读取 SN/LOCATION 走 BLE custom read path (LOOP4 length-prefix, 不受影响); **Modbus TCP 直连读 0x0894..0x08A5 会返回 ringlog 数据**, 与 MCA 不兼容
- **修复**: ringlog 改为 FC=04 只读 (Input Register, 地址 0x0887-0x08A6 + COUNT/WRITES 0x0885/0x0886 全在 FC=04), FC=03 恢复 SystemConfig 路径, SN/PLACE/HW_VER 读正确

### metuory 业务覆盖矩阵 (调研结果)

| 命令码 | 功能 | 地址 | 状态 | 备注 |
|--------|------|------|------|------|
| 0x00 | HEARTBEAT | – | ✅ | LOOP9 |
| 0x10 | READ_ADC | 0x0080 | ✅ | length-prefix BE u16 |
| 0x20/21 | READ/WRITE_SN | 0x0894 | ✅ | RTU + length-prefix read |
| 0x30/31 | READ/WRITE_LOCATION | 0x089D | ✅ | RTU + length-prefix read |
| 0x40 | READ_MAC | 0x08D7 | ✅ | bespoke arm |
| 0x50/51 | READ/WRITE_BLUETOOTH_ID | 0x08E2 | ✅ | bespoke + notify_ble_name_changed |
| 0x60 | READ_DEVICE_PRODUCT | 0x08A5 | ✅ | bespoke arm |
| 0x70/71 | READ/WRITE_IP | 0x08C7 | ✅ | bespoke read; RTU write |
| 0x80 | READ_FW_VERSION | 0x087E | ✅ | bespoke arm |
| 0x81 | READ_HARDWARE_INFO | 0x087C | ✅ | bespoke arm |
| 0x90/91 | READ_COM_INPUT/OUTPUT_IO | 0x0000/0x0200 | ✅ | bespoke packed bits |
| 0x92 | WRITE_COM_OUTPUT_IO | FC=0x05 | ✅ | RTU → write_coil (0x0200+pos) |
| 0x93 | WRITE_COM_OUTPUT_MULTI_IO | FC=0x0F | ✅ | RTU → write_multi_coils (LOOP9 已加 count 校验) |
| 0xA0-A5 | READ/WRITE_RS485_CONFIG | 0x08A6/AB/B0 | ✅ | **LOOP10 修复** packed 10B 格式 |
| 0xC0 | MODBUS_COMMAND | – | ✅ | metuory 端注释掉, 不用 |
| 0xC2/CF/CE | MCA 自定义读 | – | ✅ | handle_mca_custom_command |
| 0xD0-D3 | LOGIC/COM_REQUEST | – | ✅ | logic_handlers |
| 0xD0-DF | READ_RS485_INDEX_VALUE | 用户址 | ✅ | generic FC=04 arm |
| 0xE0-EF | READ_RS485_CUSTOM_VALUE | 用户址 | ✅ | generic FC=03 arm |
| 0xB0-B3 | DEVICE_FUNCTION_COUNT/CONFIG | 0x08FC | ⚠️ | 见已知限制 (需自定义子协议) |
| 0xB4-B7 | DEVICE_TEXT_COUNT/DATA | 0x1388 | ⚠️ | 见已知限制 (需分片重组) |

### MCA 地址对齐审计结论

| 区段 | MCA 地址 | 本系统地址 | 状态 |
|------|---------|----------|------|
| DI (FC=02) | 0x0000+ | 0x0000+ | ✅ |
| DO (FC=01) | 0x0200+ | 0x0200+ | ✅ |
| AI (FC=04) | 0x0080+ | 0x0080+ | ✅ |
| 组播状态 | 0x0090-0x0100 | 0x0090-0x0100 | ✅ |
| 485 错误 | 0x0880-0x0883 | 0x0880-0x0883 | ✅ |
| SN | 0x0894-0x089C | 0x0894-0x089C | ✅ (LOOP10 修复遮蔽) |
| PLACE | 0x089D-0x08A4 | 0x089D-0x08A4 | ✅ (LOOP10 修复遮蔽) |
| HW_VER | 0x08A5 | 0x08A5 | ✅ (LOOP10 修复遮蔽+持久化) |
| RS485 配置 | 0x08A6-0x08BD | 0x08A6-0x08BD | ✅ |
| TCP COM | 0x08C2-0x08C5 | 0x08C2-0x08C5 | ✅ |
| IP/MASK/GW/DNS/MAC | 0x08C7-0x08DC | 0x08C7-0x08DC | ✅ |
| MASTER | 0x08DD-0x08E1 | 0x08DD-0x08E1 | ✅ |
| 0x08E2 | BLE MAC (MCA) | BLE NAME (metuory) | ⚠️ 设计迁移 (LOOP3) |
| BLE MAC 迁移 | – | 0x0FA4 | 🆕 (LOOP3) |
| SENSOR MIN/MAX | 0x08E8-0x08F7 | 0x08E8-0x08F7 | ✅ (holding_buf 兜底) |
| DEVICE_CONFIG | 0x08FC+ | 0x08FC+ | ✅ |
| 用户区 | 0x0FA0+ | 0x0FA0+ | ✅ |
| device_text | 0x1388-0x1B57 | 0x1388-0x1B57 | ✅ |
| MONITOR_PLC | 0x7531-0x7917 | – | ⚠️ 未实现 (老 SCADA 兼容) |
| CONTROL_PLC | 0x9C41-0xA027 | – | ⚠️ 未实现 (老 SCADA 兼容) |

### 新增回归测试 (3 个)
- `test_ringlog_data_readable_via_input_reg` — ringlog 数据经 FC=04 可读
- `test_ringlog_does_not_shadow_sn_place_hwver` — SN/PLACE/HW_VER 经 FC=03 不被 ringlog 屏蔽
- `test_baud_to_index_alignment` — baud_to_index 编码对齐 metuory decodeSerialBaudRate

### 修改的测试
- `test_write_reg_hw_ver_ok` → `test_write_reg_hw_ver_persist` (断言 Persist)
- `test_layout_bt_addr_matches_mca` → `test_layout_bt_addr_matches_design` (断言 0x0FA4 + BLE_NAME 0x08E2)

### 编译验证
- `cargo build --bin gateway`: **0 error, 0 warning**
- `cargo test --bin gateway --no-run`: **0 error, 0 warning**, 测试编译通过

### 已知限制 (不在本 LOOP 范围)
- Web 认证仍为硬编码 Cookie (ESPSESSIONID=1), 无 CSRF/签名 — 仅适合内网
- BLE 二进制协议无分片重组缓冲 (>MTU 命令无法解析) — 影响 0xB4-B7 DEVICE_TEXT 多包流程
- DEVICE_FUNCTION_COUNT/CONFIG (0xB0-B3) 子协议未实现 — 需自定义 read/write arm
- NFC blob 仅覆盖 holding_buf 前 880 words (后 1168 words 不备份) — 历史记录，器件型号曾误判为 ST25DV64KC
- AI 通道数硬编码 6 (F4 设备 8 通道需 HAL 层配合)
- MONITOR_PLC (30001-30128) / CONTROL_PLC (40001-40300) 区未实现 — 仅老 SCADA 系统需要
- eth heartbeat 仅检测 IP 非零, 拔线后 DHCP lease 保留导致检测延迟

### 烧录命令
```bash
cargo build --bin gateway
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
espflash reset --port /dev/cu.usbserial-1430
```

## LOOP11 Web 登录修复 + 全栈零拷贝防爆栈 (2026-07-25/26) - COMPLETE

### 背景
LOOP10 已知限制 #1: "Web 认证仍为硬编码 Cookie (ESPSESSIONID=1), 无 CSRF/签名 — 仅适合内网".
用户在烧录后实测发现 BLE GATT 回调链触发 BTC_TASK 栈溢出 panic (Stack canary watchpoint),
要求:
1. **WEB 登录、功能必须能够正常使用** (P0)
2. **尽最大可能实现全系统零拷贝, 从根源杜绝爆栈**

### 根因分析 (两个并行审计 agent)

**P0 — Web 登录不可用**:
- `login.html` 用 `fetch('/login', {method:'POST'})` 提交, 期望返回 JSON
- `handle_login` 成功路径调用 `send_redirect_with_cookie()` → 返回 **301 + 空 body**
- `fetch()` 默认 `redirect: 'follow'` → 自动跟随 301 到 `GET /` → 拿到 INDEX_HTML (text/html)
- `r.json()` 解析 HTML → **SyntaxError** → 无 catch → 页面无反应, 登录失败
- MCA 参考固件 `server.send(301, "text/plain", returnResponseJson(...))` 是 301 带 JSON body, 但 Rust 端发的是空 body 301

**P0 — BTC_TASK 栈溢出**:
- `ConfigSnapshot::clone()` 栈成本 **~2700B** (内联 `DeviceConfigTable` = `heapless::Vec<DeviceEntry,32>` ≈ 2.5KB)
- `BTC_TASK` 栈 8KB (现升 12KB) → BLE GATT 写回调 → `config_read()` → `Arc::new(s.clone())` 触发 Stack canary
- Backtrace 关键栈: `gatts_event_cb` → `try_handle_binary_protocol` → `handle_ble_android_read_command` (主嫌疑)
- 次嫌疑: `update_gap_device_name` (process_tick), `with_cfg` helper (11 个 AT handler)

### 修复方案: 三层防御

#### Layer 1: `Arc<DeviceConfigTable>` 结构瘦身 (根治, 全系统级)

把 `ConfigSnapshot.device_config` 从内联改为 `Arc<DeviceConfigTable>`:

```rust
pub struct ConfigSnapshot {
    pub cfg: SystemConfig,                    // 144B, 保留内联
    pub device_config: Arc<DeviceConfigTable>, // Arc 指针 8B, 数据在堆
}
```

- clone 栈成本: **2700B → 152B** (SystemConfig 144B memcpy + Arc 原子 +1, 降 94%)
- 用 `Arc` 不用 `Box`: `Arc::clone` = atomic +1 无 alloc; `Box::clone` 仍要 2.5KB heap memcpy
- 与 `StorageSnapshot` 的 `Box<[u16]>` 模式同源 (LOOP8)
- 写路径用 `Arc::make_mut(&mut cs.device_config).write_reg(...)` 实现 COW
- **所有现有 `config_read()` 调用点自动安全** (无需改动), 写路径仅 backends.rs 一处需 `Arc::make_mut`

#### Layer 2: BLE 回调链全量 `_with` 迁移 (belt-and-suspenders)

| 文件:行 | 改动 | 上下文 |
|---------|------|--------|
| `bus/backends.rs:97-191` | read_input_reg 全部 15 处 → `config_read_with` | Modbus TCP, BTC_TASK |
| `bus/backends.rs:228, 233, 239, 246, 252-253` | read_hold_reg 5 处 → `_with` | 同上 |
| `ble_at/mod.rs:1147, 1165, 1198` | handle_mca_custom_command 3 处 (0xCE/C2/C6) → `_with` | BTC_TASK |
| `ble_at/mod.rs:1261` | `handle_ble_android_read_command` **整个 match 包进闭包** | BTC_TASK 主嫌疑 |
| `ble_at/mod.rs:681, 754` | ble_init / update_gap_device_name → `_with` | 启动/主 loop |
| `ble_at/cfg_handlers.rs:349` | `with_cfg` helper 一行改动 | 11 个 AT handler 受益 |
| `web/mod.rs:611, 667, 777` | 3 个 GET handler → 闭包内构造 JSON | http-srv 8KB 栈 |
| `udp_multicast/mod.rs:57, 255` | read_multicast_config / join_multicast_group | udp-mcast 6KB 栈 |
| `channel/ai.rs:115` | read_sensor_calib → `storage_read_with` | 高频 AI 采样 |
| `sdkconfig.defaults:138-139` | BTC_TASK/BTU_TASK 栈 8KB → 12KB | 留余量 |

#### Layer 3 (Web 登录): 真实会话管理

```rust
const SESSION_TTL_SECS: u64 = 86400;

struct Session {
    token: [u8; 16],   // 16 字节硬件 TRNG (esp_random())
    active: bool,
}
static SESSION: Mutex<Session> = Mutex::new(Session { token: [0u8; 16], active: false });
```

- 登录成功: `esp_random()` 4 次填充 16 字节 → hex 编码为 32 字符 Cookie
- `Set-Cookie: ESPSESSIONID=<32hex>; HttpOnly; Path=/; Max-Age=86400`
- `is_authenticated()`: 解析 Cookie, 与 SESSION.token 做**常量时间比较** (防 timing attack)
- `/logout`: 清零 token + `active=false`, `Set-Cookie: ESPSESSIONID=; Max-Age=0`
- 替代硬编码 `ESPSESSIONID=1` (任何人都可伪造)

修复 P0 登录失败:
```rust
// handle_login 成功: 不再返回 301+空 body, 改为 200 JSON + Set-Cookie
send_json_with_cookie(stream, 200, &json_response("login", "0"), &cookie_str)
```

`login.html` 加固:
- fetch 加 `credentials:'same-origin'` 携带 Cookie
- try/catch 处理网络错误
- 提交期间 button.disabled 防双击
- `finally` 重新启用按钮

`/getsysteminfo` 补充 `addressinfo` 字段 (index.html 依赖但之前缺失)

### 回归测试 (新增 4 个)
- `test_hex_encode_16` — hex 编码正确性
- `test_session_auth_flow` — login → authenticated → logout → unauthenticated
- `test_session_reject_invalid_token` — 拒绝伪造 token
- `test_session_constant_time_compare` — 验证常量时间比较

### 编译验证
- `cargo build --bin gateway`: **0 error, 0 warning**
- `cargo test --bin gateway --no-run`: **0 error, 0 warning**, 全部测试编译通过

### 改造后保证
| 线程 | 栈 | ConfigSnapshot clone 成本 | 风险 |
|------|-----|--------------------------|------|
| BTC_TASK (GATT 回调链) | 12KB | **0B** (全 `_with`) | 极低 |
| udp-mcast 线程 | 6KB | **0B** (全 `_with`) | 低 |
| http-srv 线程 | 8KB | **0B** (全 `_with`) | 低 |
| Modbus TCP conn 线程 | 20KB | **152B** (Arc 瘦身) | 低 |
| DeviceActor | 32KB | **152B** | 极低 |
| 启动一次性调用 | - | 152B | 极低 |

### 新代码规范
**禁止** 在 BLE 回调链/BTC_TASK/小栈线程内使用 `config_read()` / `storage_read()`.
**必须** 用 `config_read_with()` / `storage_read_with()`, 或新增的 `config_read_guard()` (借用 API).
Web 认证继续以 `HttpOnly` Cookie 为内网标准 (LOOP11 不引入 CSRF/HTTPS).

### 已知限制 (LOOP12 后状态)
| # | 原限制 | 状态 | 说明 |
|---|--------|------|------|
| 1 | BLE 分片重组 (>MTU 0xB4-B7) | ⏸️ LOOP13 待办 | 无 MCA 参考协议，需 Android BLE write trace 逆向 |
| 2 | DEVICE_FUNCTION 0xB0/B1 (FUNC_COUNT) | ✅ **LOOP12 已实施** | 复用 `read_hold_reg(0x08FC)`, 25 行 |
| 2b | DEVICE_FUNCTION 0xB2/B3 (TLV config) | ⏸️ LOOP13 待办 | 需 TLV entry 设计 + 0x08FE+ 冲突解决 (3-5 天) |
| 3 | NFC blob 仅 880 words | ✅ **LOOP29 已按实机纠正** | ST25DV16KC: `MEMORY_END=0x07FF`, 0x0120..0x07FF 共 1760 字节/880 words，与原 C++ 对齐 |
| 4 | AI 通道数硬编码 6 (F4 8 通道) | ✅ **LOOP12 已实施** | cfg 切换: f4=8 通道, 其它=6 通道 |
| 5 | MONITOR_PLC/CONTROL_PLC 区未实现 | ✅ **LOOP12 已实施** | DI/DO/AI/holding_buf 别名映射 |
| 6 | eth heartbeat 拔线延迟 | ✅ **LOOP12 已实施** | 订阅 `ETHERNET_EVENT_DISCONNECTED`, 秒级检测 |
| 7 | Web CSRF/HTTPS | ✓ 保留 | LOOP11 内网决策, 不变更 |
| 8 | Web 密码明文 NVS | ✓ 保留 | LOOP11 内网决策, 对齐 MCA `/WebPwd.txt` |

## LOOP12 推进总结

### 实施项 (5 个)
- **ETH 链路秒级检测**: `src/ethernet/w5500.rs` 新增 `ETH_LINK_UP` AtomicBool + `spawn_eth_link_watch` (订阅 ETH_EVENT 2/3), heartbeat 顶部查 flag
- **PLC 别名区**: `config.rs` 新增 4 个常量 (0x7531-0x75B0 / 0x9C41-0x9D6C); `backends.rs` 新增 read/write range arm, 映射到 DI/DO/AI/holding_buf
- **F4 8 通道 AI**: `io_state.rs::AiState` [6]→[8]; `io_global.rs` LazyLock 6→8; `channel/ai.rs::CHANNEL_COUNT` cfg; `read_sensor_calib` 同步返回类型
- **DEVICE_FUNCTION 0xB0/0xB1**: `ble_at/mod.rs` 新增 arm, READ/WRITE FUNC_COUNT (0x08FC) 经 `backends::read_hold_reg/write_hold_reg`
- **NFC EEPROM 容量纠正**: 实机为 ST25DV16KC，撤销此前 0x1FFF/3824 words 的错误扩容结论，恢复 0x07FF/880 words 原 C++ 布局

### 不实施项 (3 个, 文档化保留)
- **BLE 分片重组**: 协议规范未知, 需 Android 端 BLE write trace 逆向
- **0xB2/0xB3 TLV config**: TLV 5-word entry 设计 + 0x08FE+ 寄存器冲突需先决
- **Web CSRF/HTTPS/密码哈希**: LOOP11 内网决策保留

### 编译验证
- `cargo build --bin gateway`: **0 error, 0 warning**
- `cargo build --bin gateway --features f4`: **0 error, 0 warning** (NFC sw_i2c 兼容修复)
- `cargo test --bin gateway --no-run`: **0 error, 0 warning**

### 关键文件
- `src/ethernet/w5500.rs`: ETH_EVENT 订阅 + AtomicBool link cache
- `src/config.rs`: PLC 4 常量 + AI_CHANNELS cfg
- `src/bus/backends.rs`: PLC read/write arm + 4 个回归测试
- `src/bus/io_state.rs`, `src/bus/io_global.rs`: AiState [6]→[8]
- `src/channel/ai.rs`: CHANNEL_COUNT cfg + read_sensor_calib 同步
- `src/ble_at/mod.rs`: 0xB0/B1 arm
- `src/nfc/mod.rs`: ST25DV16KC 容量纠正、ACK polling、I2C 实例复用和栈数组优化
- `src/hal/mod.rs`: sw_i2c 兼容 f3/f4 feature (NFC bit-bang I2C 需要)

### 烧录命令 (跨平台 justfile)
本 LOOP 新增 `justfile` 跨平台烧录脚本:
```bash
just flash           # 标准烧录 Debug 固件 (需手动 BOOT+RST 进下载模式)
just flash-monitor   # 烧录 + 立即监视日志 (Ctrl+R 复位, Ctrl+C 退出)
just full-flash      # 一键全擦 + 烧录 + 硬复位 + 找 IP
just monitor         # 只监视 (烧完后开启)
just ports           # 列出可用串口
just diag            # 工具版本 + 环境诊断
just hard-reset      # DTR 拉低 500ms 硬复位
just rebuild-sys     # 强制重新链接 esp-idf-sys (App version 还是旧时)
just test-compile    # 仅编译测试 binary
```

### 验证记录
| 项 | 值 |
|---|---|
| 时间 | 2026-07-26 |
| Binary | `target/xtensa-esp32s3-espidf/debug/gateway` |
| ESP-IDF | v5.5.4 |
| BTC_TASK 栈 | 12288 bytes (8KB → 12KB) |
| BTU_TASK 栈 | 12288 bytes |
| ConfigSnapshot 栈大小 | 2700B → 152B (Layer 1) |
| BLE 回调链 ConfigSnapshot clone | **0B** (Layer 2 全 _with) |

## LOOP25 稳定性与手持机兼容审计 (2026-07-28)

### 已修复

- 健康心跳停滞不再触发 `esp_restart()`；记录故障并保持可用业务运行。
- `recovery` 的 Severe/Fatal 策略改为本地/最小功能降级，移除延迟软件重启调度器。
- NVS 分区获取失败不再 `panic!`，设备在无持久化模式下继续提供通信与本地控制。
- BLE 二进制响应容量由 32 字节扩大到完整 Modbus PDU 上限；SN、位置及 RS485 读取的长度字段不再与实际负载不一致。
- BLE GATT Write 新增固定 512-byte 无堆分配重组缓冲，兼容 ATT 分片后的 Android 配置/文本命令。
- W5500 自定义 SPI 回调补齐 Rust 2024 必需的 unsafe 边界和空指针检查，编译警告归零。

### 验证

- `cargo check`: 通过，0 warning。
- `cargo test --bin gateway --no-run`: 通过，测试二进制成功生成。
- 新增 Android 兼容边界回归：32-byte SN length-prefix 与 ATT 分片帧长度校验。

### 必须继续的实机验证

静态检查不能证明 7x24 可靠性或 Android 端到端兼容。烧录后需执行至少 72 小时浸泡测试，并覆盖：反复 BLE 连接/断开、低 MTU 配置写、W5500 拔插、RS485 无从站、NVS 写失败注入和 Modbus TCP 持续读写。记录格式见 `log/ble/compat_stability_2026-07-28.md`。

## LOOP30 BLE/NFC/OTA 最终实机回归 (2026-08-13)

### 修复

- BLE 三条心跳路径统一返回 Android 需要的 slave、运行状态和 6-byte BLE MAC，恢复连接后的首次属性同步。
- 新增认证 NFC 状态、备份、恢复 Web API 和维护界面；恢复必须 POST 且界面二次确认。
- NFC 完成状态保持可观测，原始 1760-byte 快照支持跨重启恢复，同时拒绝全 0/全 `0xFF` 空标签。
- factory 分区跳过不支持的 OTA 状态查询，消除每秒 `ESP_ERR_NOT_SUPPORTED` 告警；OTA 分区仍保留 30 秒健康确认与回滚保护。
- 烧录命令显式指定 Bootloader、OTA 分区表、factory 槽和 8MB DIO 参数，禁止只传 ELF 破坏分区布局。

### 实机结果

- 手持机 BLE 18 项读取全部通过，OTA 后再次通过。
- PC Modbus TCP 四端口、标准 FC03 125、83-word 配置写回、逻辑/文本区全部通过，OTA 后再次通过。
- NFC 备份、恢复及跨重启恢复通过；Web 认证和 HTTP 方法限制通过。
- Web OTA 写入 `ota_0@0x260000`，30 秒后确认 VALID，工具复位后仍从 ota_0 启动。
- 栈峰值: NFC 40%、RTU master 39%、UDP 30%、HTTP 23%、main 15%；free heap 约 2.0 MiB，无 pthread/stack canary/panic。

### 未完成

- 缺少独立 USB-RS485 适配器，PC RTU 实物闭环未验证。
- 未完成 72 小时浸泡，不能据短期回归宣称 7x24 已被时间验证。
