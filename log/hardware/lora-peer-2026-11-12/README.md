# E22-400T22D V2 对端串口探测

日期：2026-11-12（Asia/Shanghai）  
板载模块：用户确认为 EBYTE E22-400T22D V2；实际模块固件仍未知。  
测试板：F3，固件 2.7.0，IP `192.168.51.226/24`。  
USB-UART：ESP32 控制台 `/dev/cu.usbserial-1430`；新增串口 `/dev/cu.usbserial-14210`（WCH CH340，VID `0x1A86`、PID `0x7523`）。

## 本轮操作与结果

1. 新串口以 9600 8N1 被动监听 1 秒：0 字节。
2. 对新串口依序发送三个只读探测：E22 手册十六进制寄存器读取 `C1 00 09`、`AT+DEVTYPE=?\r\n`、`AT+FWCODE=?\r\n`。每项等待 2.5 秒，均收到 0 字节。原始时间戳、TX/RX 十六进制及结果见 `readonly-probe-102618.jsonl`。
3. 两端同时监听 35 秒：设备 UART0/控制台收到 14,752 字节，外接串口收到 0 字节。串口日志快照见 `dual-listen-102746.jsonl`；便于阅读的设备输出见 `board-102746.log`。
4. 启动日志显示 ESP32-S3 GPIO43/44 已用作控制台，且固件明确报告 `RS485-3 runtime is implemented but disabled: UART0 is the active console`。因此，已刷入固件没有从 GPIO43/44 启动接收服务。双监听的 0 字节不能证明外接模块 TX 故障，也不能证明无线链路坏。
5. 同一启动报告 `POWERON`，reset count=24，未见本次会话内 `PANIC`、`abort()` 或 WDT 标记。打开 USB-UART 与启动日志时间相邻，自动复位线是否引起此次 POWERON 无法确认。之后没有再打开控制台端口，避免更多扰动。

## 早期探测结论（历史记录）

这一节记录的是早期探测：当时刷入的是 UART0 控制台 profile，外接端未观察到应答，双向 LoRa 通信尚未形成有效的端到端测试。该结果不代表当前生产固件状态；后续已刷入 F3 `lora` profile，并完成下方的透明链路测试。

`docs/F3-GPIO.xlsx` 显示 GPIO43=U0TXD/USB烧录口/LORA串口，GPIO44=U0RXD；GPIO21 标为 LORA_RST，GPIO26 标为 LORA_M0。早期探测使用控制台 profile，因此 GPIO43/44 被控制台占用；F3 `lora` profile 已关闭控制台并由第三路 Modbus RTU worker 独占 UART0。GPIO26 因 Quad Flash/PSRAM 资源冲突仍未驱动。

本轮只向新增端口发送了上述三项只读探测，没有写 E22 寄存器、发复位/恢复默认命令、刷机或改动固件。由于串口查询可能被模式脚状态解释为透明模式下的普通 RF 数据，本次不开 AT 命令重试。

## 证据文件 SHA-256

- `readonly-probe-102618.jsonl`: `72e02302ba578ad1b570742e949383baa1a5f2939b9f33778d6ff3270cad022b`
- `dual-listen-102746.jsonl`: `20f28634ece34a4f4b3896149d36bd1f4d423a397e880ce6608af76bea55ce71`
- `board-102746.log`: `c69f417e434441bd05955fd40458984be612483ba39809d0a4d86e982bc17e3f`

## 外置 E22 对端确认后的实测结果

用户澄清电脑 USB 接外置 E22 接收端，设备 USB 不连接电脑；设备 UART0 仅接板载 E22 透传。当前 `/dev/cu.usbserial-1430` 即外置 E22 对端。

- 单帧 `01 04 08 7E 00 02 13 B3` -> `01 04 04 01 0E 06 15 59 D4`，CRC 正确。
- 串口保持打开并排空上电残留后，100 次相同请求 100 次成功，成功率 100%；完整应答平均 234.88 ms、P95 241.48 ms，首字节平均 234.87 ms、P95 241.47 ms。
- FC03 读取 15 个 RS485 配置寄存器完整且 CRC 正确；FC04 固件版本 `[270,1557]`；FC04 RS485-3 错误计数 `[0,0]`。
- 30 次/未排空的 100 次测试各有首帧失败，接收到了启动期随机字节；排空后稳定 100/100。该现象属于上电 UART 残留，不等于 LoRa RF 丢包。原始记录：`external-usb-e22-soak100-drained-20261112.jsonl`、`external-usb-e22-registers-20261112.jsonl`。

结论：**已通过真实 E22 透明模式双向 Modbus 业务收发测试**。当前已验证的业务是电脑对设备的 FC03/FC04 读请求和设备响应；尚未覆盖大帧、长时间数小时射频 soak、设备掉电瞬间首帧和多节点信道冲突。

## 2026-11-12 下午只读复验

电脑当前串口仍枚举为 `/dev/cu.usbserial-1430`，设备 `192.168.51.226:502` 可访问。Modbus TCP 只读 FC04 返回固件 `[270,1557]`，恢复计数和环日志计数均为 0；FC03 读取第三路 RTU 配置为 9600 8N1、Slave。随后通过外置 E22 串口执行 100 次 FC04 `0x087E` 读取：100/100 CRC 与帧格式通过，成功率 100%，平均 229.408 ms、P50 230.723 ms、P95 232.509 ms、最大 233.383 ms。开串口后先排空 18 字节启动/残留数据；业务过程全程只读，没有写配置或控制点。原始逐帧证据：`external-usb-e22-soak100-repeat-20261112.jsonl`。
