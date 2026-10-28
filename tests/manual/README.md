# 手工集成测试

这些脚本连接真实设备，默认执行只读检查。网络写入只由 BLE 脚本的
`--write-ip` 显式开启，并要求人工再次输入目标 IP；写入后设备会计划复位。

## 依赖

- Python 3.10 或更新版本。
- Modbus TCP 脚本只使用 Python 标准库。
- BLE 脚本需要 Bleak：`python3 -m pip install bleak`。

## Modbus TCP 长时间只读浸泡

```bash
python3 tests/manual/stability_soak.py 192.168.51.225 --duration 1800 --interval 1 --expected-version 0x00E4
```

脚本在 502/503/504/5002 四个端口各保持一个连接，每秒读取 AI、AI 状态、校准保持区和
固件版本寄存器；指定 `--expected-version` 后也会验证每轮读到目标固件版本。它不写设备。
持续输出每轮读数和结束统计，适合重定向保存为硬件测试日志。
压测电脑设置了 HTTP 代理时，Modbus 脚本不受影响；Web API 探测应将设备地址设为代理直连。

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
