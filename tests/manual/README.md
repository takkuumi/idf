# 手工集成测试

这些脚本连接真实设备，默认执行只读检查。网络写入只由 BLE 脚本的
`--write-ip` 显式开启，并要求人工再次输入目标 IP；写入后设备会计划复位。

## 依赖

- Python 3.10 或更新版本。
- Modbus TCP 脚本只使用 Python 标准库。
- BLE 脚本需要 Bleak：`python3 -m pip install bleak`。

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
