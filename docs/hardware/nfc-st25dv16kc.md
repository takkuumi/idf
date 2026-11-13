# NFC 芯片与 RF Mailbox

## 结论

本固件的主控是 **Espressif ESP32-S3**。NFC 是通过 I²C 连接的 ST25DV 双接口芯片。设计料号为 **ST25DV16KC-IE6T3**，但软件必须以真实器件的只读身份和容量寄存器决定驱动路径，不能用库名、BOM 描述或历史日志推定代际。

2026-11-08 的旧 F3 板确认 NFC 未焊接，当时无 ACK 只代表旧板状态。2026-11-11 用户接入 NFC 模块后，本次串口探测在 GPIO38/37 读到 `IC_REF=0x26`。ST 官方 DS10925 与 AN5633 确认该值属于 **ST25DVxxK**；它与 KC 的 `0x51` 不同，但并非错误身份。原先仅接受 `0x51` 的代码会错误拒绝合法 K 器件，本文撤销此前把 `0x26` 一概判作日志错误的结论。

驱动现在按如下完整签名识别：

| 器件路径 | IC_REF | MEM_SIZE | BLK_SIZE | 用户容量 |
| --- | --- | --- | --- | --- |
| ST25DV16K | `0x26` | `0x01FF` | `0x03` | 2048 字节 |
| ST25DV16KC | `0x51` | `0x01FF` | `0x03` | 2048 字节 |

身份值正确但容量不符的 4K/64K 器件、未知身份和签名读取失败均拒绝，不向其写入配置。容量和块大小实测结果见本轮硬件测试记录。未检测到可用芯片时 NFC 降级运行，不阻断主服务。

官方依据：

- [ST25DV04K/16K/64K 数据手册 DS10925](https://www.st.com/resource/en/datasheet/st25dv16k.pdf)：K 代器件身份与寄存器布局。
- [ST25DV04KC/16KC/64KC 数据手册 DS13519 Rev.8](https://www.st.com/resource/en/datasheet/st25dv16kc.pdf)：KC 代器件身份、FTM、Mailbox 与 I²C 时序。
- [ST AN5633：从 ST25DVxxK 迁移至 ST25DVxxKC](https://www.st.com/resource/en/application_note/an5633-migrating-from-st25dvxxk-to-st25dvxxkc-stmicroelectronics.pdf)：`IC_REF` 和看门狗寄存器的代际差异。

另外核对 ST 的 [ES0616：ST25DVxxK 器件限制](https://www.st.com/resource/en/errata_sheet/es0616-st25dv04k-st25dv16k-and-st25dv64k-device-limitations-stmicroelectronics.pdf)：第 6.1 节要求 256 字节 RF 消息在读取长度后直接读取数据，不再用已提前释放的 RF_PUT_MSG 判断；第 6.3 节要求主机写 Mailbox 后 ACK polling。生产驱动遵循这两条顺序，寄存器模型分别注入提前清标志和写后 NACK 进行回归。勘误还包含供电波动造成 MB_MODE 失效、RF 命令与 I²C 并发等限制；签名和空闲测试不能覆盖全部电气/RF 条件，仍需真实手机和供电测试。K 寄存器地址/掩码另与 [ST 官方驱动 st25dv_reg.h](https://github.com/STMicroelectronics/stm32-st25dv/blob/main/st25dv_reg.h) 交叉核对。

芯片丝印/BOM 与真实读值不符仍需硬件侧核对；软件识别不能代替料号检查、供电和天线验收。

## I2C 接口

ST25DV 使用两个 7 位地址：

- 数据/用户 EEPROM：`0x53`
- 系统寄存器：`0x57`

这是默认 `I2C_DEVICE_CODE=0b1010`、`I2C_E0=1` 下的 7 位地址；KC 可配置 E0，而 K 使用固定默认设备码/地址。`I2C_CFG.E0=0` 时对应地址为用户 `0x52`、系统 `0x56`；固件启动只读探测这两组 E0 地址，发现后在整个会话中沿用实际应答地址。设备码本身不是默认值时仍需板级配置明确，不对可能与 PCA9555 冲突的任意地址做盲扫。

I²C 的 SDA/SCL 都按开漏方式驱动并由外部上拉为高电平。当前软件 I²C 曾使用推挽输出，现已改为 ESP32-S3 `GPIO_MODE_INPUT_OUTPUT_OD`：写 1 是释放总线，不主动驱高；写 0 才拉低。数据手册 §2.1 要求 SDA 开漏并要求总线控制器具备开漏 SCL。实际板上仍必须确认外部上拉阻值及波形；串口芯片签名只验证数字链路，不能替代示波器验收。探测不到目标地址时，固件还会记录释放总线后的 SDA/SCL 电平，用于区分总线被拉低/无上拉与总线空闲但目标未 ACK；该诊断只读采样，不会向未知地址发送数据。

旧固件先访问 `SDA=38/SCL=37`，失败再回退到 `SDA=35/SCL=36`。当前固件保留该顺序，并与 PCA9555 软件 I2C 共享物理总线锁。软件 I²C 当前使用开漏模式；修复前使用推挽输出，与 I²C 电气要求不符，现已纠正。板上外部上拉尚未取得示波器/原理图证据。

用户 EEPROM 布局仍与旧固件兼容：

- NDEF 区：`0x0000..0x011F`
- 原始 holding 快照：`0x0120..0x07FF`，共 1760 字节

## RF Mailbox/FTM

FTM 和 Mailbox 只在检测到标签后启用，EEPROM/NDEF 维护时暂时关闭。进入使能状态时读取动态 `EH_CTRL_Dyn.VCC_ON`，设置 FTM/`MB_EN` 后对 `MB_MODE`、`MB_WDG` 和 `MB_EN` 状态转移读回核验；FTM 的 `MB_WDG` 明确清零，因为固件邮箱 worker 以 100 ms 周期服务，保留非零 watchdog 可能在软件读取前触发 `HOST_MISS_MSG`/`RF_MISS_MSG`；稳定运行期间只读检查，不重复写入相同 EEPROM 值。K 的 `0x000E` 和 KC 的 `0x000D bits3:1` 必须分别处理，KC 的 `0x000E` 是 `I2C_CFG`，不得当作看门狗改写：

| 项目 | 地址/位 |
| --- | --- |
| FTM 开关 | K：`MB_MODE 0x000D bit0`；KC：`FTM 0x000D bit0` |
| Mailbox watchdog | K：独立 `MB_WDG 0x000E bits2:0`；KC：`FTM 0x000D bits3:1` |
| `MB_CTRL_Dyn` | `0x2006` |
| `MB_LEN_Dyn` | `0x2007`，值为实际长度减一 |
| Mailbox 数据 | `0x2008` 起，最多 256 字节 |
| `MB_EN` | `MB_CTRL_Dyn bit0` |
| `HOST_PUT_MSG` | `bit1` |
| `RF_PUT_MSG` | `bit2` |
| `HOST_MISS_MSG` | `bit4` |
| `RF_MISS_MSG` | `bit5` |
| `HOST_CURRENT_MSG` | `bit6` |
| `RF_CURRENT_MSG` | `bit7` |

I2C 读取 RF Mailbox 的最后一个字节会释放 RF 消息状态；状态位不能当作普通可写位清除。worker 先读 `MB_CTRL_Dyn` 判断 `RF_PUT_MSG`，再读 `MB_LEN_Dyn` 和完整数据；`MB_WDG=0` 时不会因旧 watchdog 自动置 `RF_MISS_MSG`。I2C 向 `0x2008` 写完一条消息后，ST25DV 在 STOP 时更新邮箱内容、长度和 `HOST_PUT_MSG`。固件随后按 DS13519 Rev.8 附录 B.3.2 做设备地址 ACK polling，收到 ACK 后才返回成功；不会重发 payload，避免同一消息重复发布。EEPROM/NDEF 维护时只暂时清除动态 `MB_EN`，保留静态 FTM 授权位，完成后恢复 Mailbox。MCU 启动的新会话会先清除旧邮箱消息；任何分片/最终应答超过 10 秒未取走，会丢弃旧会话并重探测。已持久化但尚未应答的重启请求仍按既定策略应用，避免配置已落盘却运行态未生效。初始化或轮询失败会进入退避重探测，不再静默忽略。

手机侧通过 ISO15693 自定义命令访问 Mailbox：

- `0xAA`：写 RF Mailbox
- `0xAD`：读取 `MB_CTRL_Dyn`
- `0xAB`：读取 Mailbox 长度
- `0xAC`：读取 Mailbox 数据

RF 命令参数遵循 ST25DV16KC 数据手册的 FTM 命令表。`Read Message Length` 返回长度减一；App 将其还原成自然长度，仅用于边界校验。完整读取命令必须编码 `MBpointer=0, NumberOfBytes=0`；此特殊组合返回整条消息，字段不能填为 `length-1`。原实现对大于 1 字节的响应错误地编码为显式字节数，已更正并补充 1/32/256 字节命令向量测试。接口不暴露任意偏移/部分范围读取。Android `NfcV.transceive` 接收完整 ISO15693 原始帧，需由调用方提供 ST 厂商码 `0x02`；iOS Core NFC 的 `customCommand` 会从标签信息中自动插入 IC 厂商码。因此 App 在 iOS 适配层会剥离 Android 原始帧里的 `0x02`，并在会话建立时验证 ST 厂商码；Android 额外检查 ISO15693 UID 的 `E0 02`（兼容反向字节序），防止请求帧重复或发给非 ST 标签。

ST25DV 的 I²C 安全会话密码帧采用 `password[7..0] + 0x09 + password[7..0]` 共 17 字节；固件与旧 SparkFun 驱动的实现一致，并有非对称密码向量用于防止端序误改。

NFC 直接复用 BLE 的二进制协议分发器，不设独立的配置功能白名单。因此 BLE 手持机使用的标准 Modbus 写入、自定义 C0～CF 命令以及 D0～D2 组态命令，在 NFC 入口走相同地址映射、校验和落盘路径。IP、网关和以太网 MAC 写入在 NFC 侧同步确认配置落盘，随后等待手机读走最终成功应答再请求重启，最多等待 10 秒；如果标签提前离场，也会按已落盘配置请求重启，避免新网络配置一直未应用。BLE 仍保持原有应答与复位行为。

## IMC 协议

NFC 传输的业务帧仍是 BLE 使用的完整 IMC 帧，保留事务号、协议号、长度、功能码和 Modbus CRC。固件调用同一个业务分发器，因此以下能力使用相同的校验和地址语义：

- 读取/修改 IP、子网掩码和网关
- SN、位置、以太网 MAC、蓝牙 ID、RS485 和设备文本配置写入
- DO、AO、holding 地址及组态逻辑写入/删除
- RS485 配置与读取
- 组态逻辑读写/删除
- holding、输入、线圈和离散量访问
- 设备文本、设备信息、版本和状态

当 IMC 帧超过 Mailbox 单次容量时，使用 `IMCN` 有界分片封装；每片带版本、序号、总长度、偏移和 CRC16。固件和 Flutter NFC transport 都会拒绝越界、乱序和 CRC 错误的分片。

## 2026-11-08 串口板级检查

- 串口识别到 ESP32-S3 v0.2，当前工厂分区运行 IMC 2.7.0 F3；启动完成，BLE、网络、Modbus TCP 和主循环服务均启动。
- F3 调试固件按完整分区参数烧录后复位，启动未见 panic、assert 或 watchdog 错误。
- 旧固件 `RFID_Init` 先用 GPIO38/37 调 `tag.begin(Wire)`，失败才切 GPIO35/36，并用默认 `Wire` 调 `tag.begin()`。对照旧仓库随附的 `MCA_F16V2_1_F48_BLE.ino.elf` 反汇编，SparkFun `isConnected()` 实际只执行 `Wire.beginTransmission(87)`（7 位地址 `0x57`）和 `endTransmission()`；`begin()` 把 ACK 成功转换为 true，没有读取 `IC_REF`，也没有检查用户地址 `0x53` 或 E0=0 的 `0x56/0x52`。因此旧固件只证明 `0x57` 有 ACK，不能证明 ACK 设备就是 ST25DV16KC-IE6T3。旧代码回退分支却写成 `if (tag.begin() == 0)`：GPIO35/36 成功时反而 `return 0`，失败时打印“35 System Init done!”并继续访问；且 `setup()` 调用 `RFID_Init(0)` 时忽略返回值。这使旧固件的 35/36 回退逻辑反转且启动状态报告不可信。旧版 v2.2.5 Rust 日志中的 `IC_REF=0x26` 也与 ST25DV16KC-IE 官方身份不符，不能作为芯片识别证据。
- 旧 `setup()` 在 `RFID_Init(0)` 前先把 GPIO33 拉低，GPIO21/33 的板卡供电拉高发生在后续网络初始化阶段；因此旧固件本身没有建立“先给 NFC/扩展板供电、再探测 ST25DV”的可靠时序。当前固件在 `Hal::init()` 中先打开 GPIO21/33，再启动 NFC worker，已避免该时序缺陷。
- 修正身份并增加 `I2C_CFG.E0` 两种地址只读探测后，F3 在系统地址 `0x57/0x56`（数据地址 `0x53/0x52`）均无应答；SDA/SCL 38/37 与 35/36 两组候选引脚结果一致。E0 地址位不匹配已排除；仍无法区分芯片供电、实际 PCB 引脚/连线、可配置的 `I2C_DEVICE_CODE` 被更改或芯片未装/故障。没有扫描其它设备码，因为共享总线上存在 PCA9555/LED 设备，盲扫可能向非目标器件发送寄存器指针字节。
- 修正固件启动 120 秒以上未见 panic/watchdog，NFC 任务最小剩余栈约 3760B，任务栈巡检 `low=0`；网络 IP 在启动后约 2.5 秒获得。该结果证明基本系统启动健康，不证明 NFC 芯片已被访问。
- PCA9555 worker 在此次日志中正常初始化且没有 I²C NACK 告警；这只能证明其自身访问正常，不能证明 ST25DV 的总线映射和连接正确。当前驱动已探测系统地址 `0x57` 与 `0x56`（默认 `I2C_DEVICE_CODE=0xA`、E0=1/0）；设备码若被改写，需原理图/芯片配置明确后再定向探测，不能对共享 PCA9555 总线做任意地址盲扫。
- 仅探测默认地址的早期采集保存在 `log/hardware/nfc/st25dv16kc-f3-probe-2026-11-08.log`；加入 E0 双地址后的采集保存在 `log/hardware/nfc/st25dv16kc-f3-e0-probe-2026-11-08.log`。两次启动后连续观察均超过 120 秒，未见 panic/watchdog；当前未进行真实手机 RF 射频验收。

## 当前验收边界

源码、协议常量、F3/F4 编译和 macOS Flutter 构建已验证。真实手机 NFC 射频、iOS 真机授权、Android NFC-V 兼容性以及板上 ST25DV 天线距离仍需要使用真实标签和手机完成最后验收；没有实机数据时不把这些项目标记为已通过。

## 2026-11-08 协议回归补充

`tests/host/run_nfc_mailbox.py` 提取固件实际的 Mailbox 编解码、芯片身份判定与状态辅助逻辑并执行 8 条主机测试，其中固定 golden vector 与 Flutter 端测试向量逐字节一致，同时覆盖芯片身份 `0x51`、拒绝错误 `0x26`、E0 双地址计算、CRC 损坏、乱序恢复、最大帧、非法长度和响应释放判定。记录见 [NFC Mailbox 主机验证](../testing/nfc-mailbox-host-verification-2026-11-08.md)。该结果证明软件协议向量，不代表电气/真实射频通过。
