# 手工集成测试

这些脚本连接真实设备。各 soak、数据流和 `production_acceptance.py` 只读。
`configuration_acceptance.py`、`test_udp_reconfiguration.py` 仅供独占台架，必须显式
`--apply`，会暂改配置并恢复；禁止在生产控制回路运行。BLE IP 写入仍须 `--write-ip`
并确认目标地址。

## DO 写入、读取和连续压测

```bash
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/do_response_stress.py \
  192.168.51.227 --apply --duration 600 --idle-duration 60 \
  --backup log/hardware/do-original-unique.json \
  > log/hardware/do-response-unique.jsonl
```

必须先确认空载、所有 DO 均可安全动作，并独占 Web 会话。脚本实际切换输出：
交替使用 Web、FC05 单点和 FC15 批量写入，逐笔核对 FC01 目标状态与 PCA9555
OUTPUT 寄存器确认代号；包含逐点和全开/全关/交错位图，至少间隔 100ms 发起下一次
切换，不保证固定频率。前 60 秒空闲，随后四个 Modbus 端口持续读并发施压。
分别统计写应答、线圈读取、从写入到首次观察到寄存器确认的延迟，后者包含网络与
HTTP 采样开销，不能当作精确电气延迟或触点反馈。异常/结束会尝试恢复原输出；
必须检查 `restored` 事件及最终 `end`，不能只看成功写入计数。

备份必须使用新路径。中断恢复可用同一脚本加 `--restore-only --apply --backup <原备份>`；
只使用本机当前测试的原备份，禁止恢复其它设备或其它会话的输出。

## 生产核心与联合验收

```bash
python3 tests/host/run_production_core.py
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/production_acceptance.py \
  192.168.51.226 --duration 600 --expected-version 0x010E
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/configuration_acceptance.py \
  192.168.51.226 --apply --backup log/hardware/config-original.json
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/test_udp_reconfiguration.py \
  192.168.51.226 --apply
```

主机测试从实际源码加载核心模块/函数，覆盖并发、回卷、队列、dirty 和保存确认边界；
不连接设备、不烧录。联合压力覆盖四端口、125-word、分片/流水/非法 PDU、Web 认证与
参数检查，报告错误和延迟。单设备只有一个 Web 会话，勿并行登录或另开浏览器登录。

## 四端口 FC05/FC01 延迟基准

```bash
python3 tests/manual/tcp_latency_benchmark.py 192.168.51.227 --apply \
  --duration 60 --interval 0.25 \
  --backup log/hardware/tcp-latency-original-unique.json \
  --output log/hardware/tcp-budget2-latency-60s-YYYY-MM-DD.jsonl
```

脚本在四个长连接上分别使用 Q1/Q2/Q3/Q4 交替执行 FC05，并立即执行 FC01 读取
16 位线圈镜像；结束、异常或收到中止信号后均尝试逐位恢复并复读原位，最后再次检查
完整位图。必须显式 `--apply`，且备份与日志路径不能已存在。必须先确认空载或已获现场授权，不能把
FC01/FC05 网络时延当成物理触点动作时延。脚本只做 Modbus TCP，不登录 Web，因此
不会制造单 Web 会话替换；仍应避免其它手持机或 PLC 同时写 Q 点。

配置工具备份身份、三个串口、BLE、校准、系统信息、组态，以及原始字 `40259`、`6999`，
立即重启验证，再恢复后二次重启。确认两字不参与现场业务后才能运行。异常后可对原备份
执行 `--restore-only --apply`，禁止覆盖备份。UDP 工具恢复原五个配置字，验证热重绑、
LE 镜像和 5 秒新鲜度；不能证明组播跨交换机、光口或环网可用。

另外的显式台架工具均要求 `--apply`：`test_password_roundtrip.py` 暂改并恢复密码，
`test_ota_acceptance.py <host> <app.bin>` 上传镜像并计划重启，
`test_tcp_port_configuration.py` 暂改 Modbus 监听并恢复。密码从同一环境变量读取；
密码中断恢复文件仅在测试期间以私有权限存在，成功恢复后删除。OTA 的 app-valid 与
实际启动分区仍须对照串口证据，不能只看上传 HTTP 成功。

## 依赖

- Python 3.10 或更新版本。
- Modbus TCP 脚本只使用 Python 标准库。
- BLE 脚本需要 Bleak：`python3 -m pip install bleak`。

## Modbus TCP 长时间只读浸泡

```bash
python3 tests/manual/stability_soak.py 192.168.51.225 --duration 1800 --interval 1 --expected-version 0x010E
```

脚本在 502/503/504/5002 四个端口各保持一个连接，每秒读取 AI、AI 状态、校准保持区和
固件版本寄存器；指定 `--expected-version` 后也会验证每轮读到目标固件版本。它不写设备。
持续输出每轮读数和结束统计，适合重定向保存为硬件测试日志。
压测电脑设置了 HTTP 代理时，Modbus 脚本不受影响；Web API 探测应将设备地址设为代理直连。

## 真实 RS485 组态结果只读验收

```bash
python3 tests/manual/test_rs485_dataflow.py 192.168.51.226 \
  --rs485-port 1 --destination 1 --count 2 --duration 120 --interval 0.5
```

`--destination` 必须使用当前组态的主机结果起始地址，`--count` 为结果字数。
脚本经 Modbus TCP 的 FC04 读取 RTU 主站结果，并检查指定物理端口的通信/应用错误状态。
如果测试传感器输入持续变化，可额外指定 `--require-change`；稳定或零输入不应使用该选项。
FC03 低地址 `0..299` 为控制字，不等价于 FC04 监控字；不能用 FC03 的零值判断 RTU 未采集。
测试前需确认组态的端口确实配置为主站，并核对从站地址、波特率、校验位和寄存器地址。
脚本不写设备，也不改变主从角色或持久化配置。

## Web 状态长时间只读浸泡

```bash
GW_STABILITY_WEB_PASSWORD='admin123456' \
  python3 tests/manual/status_soak.py 192.168.51.225 --duration 259200 --interval 60
```

脚本强制绕过系统 HTTP 代理，登录后按间隔读取系统状态，检查 uptime 不回退、重启计数不变、
以太网在线、恢复模式为 Normal、故障计数全为零，并记录堆内存低水位。密码只从环境变量读取。

## UART 启动与长稳日志采集

```bash
python3 tests/manual/serial_soak.py --port /dev/cu.usbserial-1430 --baud 115200 --duration 259200
```

脚本只读 UART，并在端口暂时不可用时退避重连。用串口 monitor 或烧录工具前应先停止该采集器，
因为一个串口不能同时由多个进程读取。
脚本在打开前设 DTR/RTS 为非活动态，避免持续拉住 ESP32 控制脚；部分 USB 串口驱动
仍可能在打开时产生复位脉冲。必须先连接 UART、等待设备网络就绪，再建立测试的
uptime/reset_count 基线；测试中避免重新开关串口。采集器不主动发送软件复位命令。

## NVS 保存时延基准

```bash
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/nvs_save_benchmark.py \
  192.168.51.226 --apply --backup log/hardware/nvs-original-config.json
```

脚本只临时修改设备显示名称和相同的地址备注，不改 IP、RS485、BLE、校准或输出；结束时
自动恢复并逐项复读。备份路径必须不存在。结果是 Web 保存确认时延，包含 HTTP 和
DeviceActor 调度，不代表纯 Flash 时延或擦除次数。

必须在独占台架窗口执行：设备 Web 为单会话，脚本登录会使人工浏览器会话失效。
`--reboot-check` 会主动重启设备两次，不得与人工 Q 点操作或现场控制并发执行。

## Modbus TCP 网络配置读取

```bash
python3 tests/manual/test_modbus_network.py 192.168.51.220
```

脚本只读 holding registers 2247–2258，并按 IP、掩码、网关顺序验证 IPv4 octet。
不会更改设备配置。

## BLE 手持机网络配置读取

```bash
python3 tests/manual/test_ble_network_config.py
```

脚本通过 service UUID 发现网关、读取手持机兼容的 READ_IP 响应并核对 IP、掩码、
网关顺序。也可以用 `--device <BLE address>` 指定设备。

## BLE 只修改 IP

```bash
python3 tests/manual/test_ble_network_config.py --write-ip 192.168.51.220
```

脚本读取现有掩码和网关并原样保留，用户确认目标 IP 后才写入。网关会复位，脚本
等待 BLE 重新广播、重新连接并读取验证。只在可通过 BLE 找回设备、且目标 IP 属于
现场网段时执行。
