# LoRa 硬件分析与验收边界

## 结论

用户已确认模块型号为 EBYTE E22-400T22D，Version V2。板载模块工作在透明传输模式：设备侧由 UART0（GPIO43/44）承载 E22 与 Modbus RTU 的字节流，电脑 USB 接入的是外置 E22 对端，不是设备的 UART0。F3 接线表把 GPIO21 标为 `LORA_RST`，但已确认的 E22 D 型接口没有独立 RESET 脚；固件只保持 GPIO21 旧版启动拉高行为，板级网络的真实用途仍需原理图确认。当前生产验证以 `lora` profile 为准；标准调试 profile 仍不能把控制台和透明无线数据共用。型号、官方资料版本及引脚核对事项见 `e22-400t22d-v2-review.md`。

接线表另标出 `GPIO26=LORA_M0`，但本工程使用 Quad Flash/PSRAM，GPIO26~32 属于存储总线保留脚；源码将 `LORA_M0_PIN` 置为无效值并禁止驱动，除非确认实际模组和 PCB 复用后重新验证。

## 旧固件得到的证据

旧工程 `MCA_F16V2_1_F48_BLE` 将第三路配置称作 `RoLa`，实现是：

- `Serial` / UART0，默认控制台引脚 TX=GPIO43、RX=GPIO44；
- 启动时读取第三路 RS485 的主/从、波特率、地址、超时等配置；`Lora reset` 只是读取从站 ID 旁的注释，并不构成可靠的 LoRa 复位实现证据；
- 从站模式读取 UART0 字节并交给 Modbus RTU 解析；主站模式通过同一个 UART0 发送 Modbus 请求。

旧工程没有可确认的 LoRa 芯片型号、无线寄存器读写、频率/扩频因子/带宽/功率配置或 LoRaWAN 入网流程。源码能证明的范围是 UART0 上承载第三路 Modbus 数据；它是否由外部透明无线模块完成空中传输，不能仅从固件推断。

旧工程没有检测 UART0 连接器是否插入的逻辑。它启动 `Serial` 后会持续通过同一 UART 打印日志，同时在主循环中收发第三路 Modbus 数据；因此“插入 UART0 就进入下载、没插入就切换 LoRa”不是旧固件的模式检测行为。ESP32-S3 ROM 下载器在应用启动前响应下载握手，和应用运行期判断线缆插入是两件事。

## 当前设备能安全探测什么

可以安全确认：

1. GPIO43/44 UART0 与 E22 的板级连线，以及 GPIO21 `LORA_RST` 网络实际落点/用途是否与 PCB 一致。
2. 设备是否在 GPIO43/44 接收到外部串口活动（需要示波器或逻辑分析仪，不能仅凭软件空闲读取区分“无模块”和“模块无数据”）。
3. 如果确认外部模块协议，再通过其只读 AT/状态命令读取型号和固件版本。

不能安全推断：

- 仅凭串口没有响应就判断“没有 LoRa”；
- 在未知协议下盲发命令；
- 把 Modbus `mode=3` 当作 LoRa 驱动状态。

## 现场确认步骤

1. 断电查看板卡 BOM、丝印和模块照片，确认是否存在独立 LoRa 模块及型号。
2. 查模块数据手册，确认它接的是 UART、RS485 还是 SPI，并记录供电、波特率、默认地址、复位和 IRQ。
3. 若为 UART/RS485 透明模块，使用隔离 USB-UART/RS485 转换器在 GPIO43/44 对应接口做只读监听；先确认空闲电平、波特率和帧格式，再发送数据手册规定的查询命令。
4. 若为 SPI LoRa 芯片，必须确认 CS、RESET、DIO1/DIO0、BUSY 和电源控制引脚后，才能编写不会影响 W5500 的独立探测驱动。
5. 只有完成型号识别、回环/点对点收发、断电重启和连续运行测试后，才能把 LoRa 标为生产功能。

## 当前产品决策

设备的 UART0 是板载 E22 的透明传输通道。这里的“RS485-3”只是沿用的配置/Modbus 逻辑端口名，不表示 UART0 接有物理 RS485 收发器。F3 的 `lora` profile 关闭 UART0 应用/bootloader 控制台，并由统一的 per-port Modbus RTU runtime 独占 UART0，使电脑外置 E22 → 空中链路 → 板载 E22 → UART0 → Modbus RTU 的业务数据不被日志污染。该 runtime 按已保存的第三端口 Master/Slave 模式运行，出厂默认是 Slave；本次实测为 Slave。标准调试 profile 仍将 UART0 用作控制台，因此不能用标准调试固件验收透明无线业务。板级 BCT4227 按 USB VBUS 将 UART0 路由到 USB-UART 或板载 E22；应用层不依据 USB 线缆插入状态切换模式。

## 2026-11-12：F3 原理图复核与 UART0 LoRa build profile

用户补充 F3 的两张 LoRa 电路图后，修正了“无串口插入检测/选择信号”的判断。BCT4227 的选择网络连接 `VBUS_TYPEC`，图中主控侧 `UART0RX/UART0TX` 经 22Ω 电阻进入开关，开关两路分别接板载 LoRa UART 与 USB-UART。按图纸的设计意图，USB VBUS 负责硬件路由，应用固件无需自行判断 USB 是否插入。切换芯片真实逻辑极性/有效电平仍应以 BCT4227 官方真值表及 PCB 走线为准；电路图网名足以证明有硬件切换链，不足以推断开关真值表。

`GPIO26=LORA_M0` 与芯片 Flash/PSRAM 复用网络冲突，固件不驱动该脚。用户确认板载 E22 处在透明传输模式，故当前 LoRa 业务按第三路 Modbus RTU 透明承载实现，不发送 E22 AT 配置命令、不将 UART0 当作独立 LoRaWAN/SPI 芯片驱动。

为避免 UART0 的日志成为无线数据，将 `lora` Cargo feature 纳入默认编译能力；运行时仅 `f3 + lora` 组合启用 UART0 第三逻辑端口，并叠加 `sdkconfig.lora.defaults` 专用配置：关闭应用控制台及 bootloader 日志，并允许统一 per-port RTU runtime 打开 UART0 第三端口；标准默认/F4 构建仍保留现有 UART0 控制台策略。F3 发布构建流程与 CI 增加该 profile 的构建检查。

2026-11-12 实测：F3 LoRa release `cargo build --release --features f3,lora` 通过；生成 ESP-IDF sdkconfig 确认 `ESP_CONSOLE_NONE=y`、`BOOTLOADER_LOG_LEVEL_NONE=y`，BLE Bluedroid 与 W5500 仍启用。app image 1,826,608 字节；使用 esptool 4.7.0 写入 factory app offset `0x20000`，报告 1,826,608 字节写入，flash readback digest 匹配。复位后 Modbus TCP FC04 读取固件字段返回 `010e 0615`（现有协议版本格式）。

这一段是早期“外置对端尚未枚举、尚未启用 UART0 worker”的探测记录，仅保留作历史背景，不代表当前状态。后续已确认电脑 USB 接的是外置 E22 对端，并使用 F3 `lora` profile 完成透明链路验收；当前有效结果见下方“外置 USB-E22 对端确认后的实测结果”。

## 2026-11-12：外置 USB-E22 对端实测通过

用户澄清电脑 USB 接的是外置 E22 接收端，设备本身没有接电脑 USB；设备 UART0 仅连接板载 E22 透明透传。当前电脑串口 `/dev/cu.usbserial-1430`（CH340，VID `1A86`/PID `7523`）按 9600 8N1 使用。

- 单帧验证：电脑端发送 `01 04 08 7E 00 02 13 B3`，收到 `01 04 04 01 0E 06 15 59 D4`，CRC 正确。
- 100 次连续验证（每帧间隔 100 ms）：先保持串口打开并排空启动期残留字节后，100/100 成功，成功率 100%；首字节平均 234.87 ms，P95 241.47 ms；完整应答平均 234.88 ms，P95 241.48 ms。原始数据见 `external-usb-e22-soak100-drained-20261112.jsonl`。
- 多寄存器验证：FC03 读取 RS485 配置返回 15 个寄存器且 CRC 正确；第三路配置为 9600 8N1、从站语义、地址字段为 0（固件运行时按旧协议将 0 回退为地址 1）；FC04 固件版本返回 `[270,1557]`（`0x010E/0x0615`）；RS485-3 错误计数返回 `[0,0]`。原始记录见 `external-usb-e22-registers-20261112.jsonl`。

因此，本次已形成真实 E22 空中双向业务证据：电脑 → 外置 E22 → 空中透明链路 → 板载 E22 → ESP32 UART0 → Modbus RTU 解析/应答 → 原链路返回。此前 30 次和未排空的 100 次各出现首帧失败，均为打开/启动期 UART0 残留字节干扰；保持对端串口打开并在业务开始前排空后，100 次全通过。启动残留来自 ESP32 ROM/上电阶段 UART 输出，不能用应用 `CONFIG_ESP_CONSOLE_NONE` 完全消除；生产主站应在设备上电稳定后再发首帧并按 Modbus 超时/重试处理。若要求上电第一帧绝对无噪声，需要单独评估 ESP32-S3 ROM UART 输出关闭 eFuse（不可逆）或硬件在启动期保持 E22 reset，不能未经风险评估直接烧 eFuse。
