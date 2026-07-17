# 验证清单

> 首次烧录后建议按顺序逐项验证

## 启动验证

- [ ] 串口能看到 ESP32-S3R8 启动 banner
- [ ] 串口能看到 `esp32s3-iot-gateway v0.1.0` 日志
- [ ] 串口能看到 `[main] starting ethernet/ble mesh/io scan/ai-ao/modbus rtu/tcp`
- [ ] 串口能看到 `[main] entering main loop`
- [ ] `[main] uptime=1s tick=10` 每秒打印一次
- [ ] 无连续 panic / backtrace

## 以太网验证

- [ ] W5500 复位正常（GPIO15 输出低脉冲 50ms，由 `hal.gpio.eth_reset()` 控制）
- [ ] 网线插入后 DHCP 获取 IP（log 输出 `[eth] got IP: x.x.x.x / x.x.x.x gw x.x.x.x`）
- [ ] 拔插网线能重新获取 IP
- [ ] `ping <设备IP>` 能通
- [ ] 心跳任务每 5s 执行一次（debug 级日志）
- [ ] 拔网线 15s 后设备自动复位（连续 3 次心跳失败）

## BLE Mesh 验证

使用 ESP-BLE-Mesh App (iOS/Android) 或 [nRF Mesh](https://www.nordicsemi.com/Products/Development-tools/nrf-mesh) 测试：

- [ ] 扫描能发现设备 `ESP32S3-GW`
- [ ] 能成功配网 (Provisioning)
- [ ] 配网后设备加入 mesh 网络
- [ ] Generic OnOff Client 能控制设备 DO0
- [ ] 设备能作为 Proxy 节点（手机远离 mesh 时通过设备转发）
- [ ] 心跳上报每 60s 一次

## Modbus TCP Server 验证

用 [Modbus Poll](https://www.modbustools.com/modbus_poll.html) 或 [pymodbus](https://pypi.org/project/pymodbus/) 测试：

```bash
pip install pymodbus
python -c "
from pymodbus.client import ModbusTcpClient
c = ModbusTcpClient('192.168.1.100', port=502)
print(c.connect())
# 读固件版本 (0x0100)
r = c.read_holding_registers(address=0x0100, count=1)
print(hex(r.registers[0]))  # 应输出 0x100 (v1.00)
# 读运行时长
r = c.read_holding_registers(address=0x0101, count=1)
print('uptime:', r.registers[0], 's')
"
```

- [ ] TCP 能连接到设备 502 端口
- [ ] 读保持寄存器 0x0100 返回固件版本 (0x0100)
- [ ] 读保持寄存器 0x0101 返回 uptime（每秒自增）
- [ ] 写线圈 0x0000 (DO0) → DO0 GPIO 输出变化
- [ ] 读离散输入 0x0000 (DI0) → 反映 DI0 实际电平
- [ ] 读输入寄存器 0x0000 (AI0 raw) → 返回 ADC 原始值
- [ ] 读输入寄存器 0x0010 (AI0 scaled) → 返回工程量
- [ ] 写保持寄存器 0x0000 (AO0) → AO0 PWM 输出变化
- [ ] 写保持寄存器 0x0103 = 0xA5A5 → 设备复位
- [ ] 多连接（同时 4 个客户端）正常
- [ ] 第 5 个客户端连接被拒绝

## Modbus RTU Slave 验证

用 [Modbus Poll](https://www.modbustools.com/modbus_poll.html) 或 USB-RS485 转换器测试：

- [ ] 主站设备能访问从站地址 1
- [ ] 各功能码 (01/02/03/04/05/06/0F/10) 正常响应
- [ ] 非法地址返回异常码 0x02
- [ ] 非法数据值返回异常码 0x03
- [ ] 广播地址 0 不返回响应但执行操作
- [ ] CRC 错误的帧被丢弃（无响应）

## Modbus RTU Master 验证

- [ ] 能成功轮询从站 1 (FC=03, addr=0, count=8)
- [ ] 从站不存在时超时返回 (TIMEOUT_MS=500ms)
- [ ] CRC 校验失败的响应被丢弃
- [ ] 异常响应被正确处理并打日志

## IO 验证

### DI 数字输入
- [ ] DI0~DI7 电平变化能在 1ms+去抖后反映到总线
- [ ] 上升沿/下降沿日志正常
- [ ] 通过 Modbus 读离散输入能获取最新状态

### DO 数字输出
- [ ] DO0~DO7 通过 Modbus 写线圈能切换
- [ ] BLE Mesh OnOff Set 能控制 DO0
- [ ] DO 输出电流符合硬件设计（开漏 100mA）

### 联动测试
- [ ] DO0 输出 → DI0 检测（硬件回环）状态一致
- [ ] DO0 通过 Modbus 写 → DI0 通过 Modbus 读，状态一致
- [ ] DO0 通过 BLE Mesh 写 → DI0 通过 Modbus 读，状态一致

## AI/AO 验证

### AI 模拟输入
- [ ] ADC 0-3.3V 输入对应 raw 0-4095
- [ ] 滑动平均生效（连续 8 个采样后才稳定）
- [ ] 4-20mA 标定正确（4mA → scaled=4000, 20mA → scaled=20000）

### AO 模拟输出
- [ ] 写保持寄存器 0x0000 = 0 → PWM duty = 0
- [ ] 写保持寄存器 0x0000 = 10000 → PWM duty = 4095
- [ ] AO 经 RC 滤波后电压与 scaled 线性相关

### 联动测试
- [ ] AO0 输出 PWM → 用万用表测量电压
- [ ] AO0 输出 → 经外部电路 → AI0 采集，闭环测试

## 稳定性测试

- [ ] 24 小时连续运行无 panic
- [ ] 24 小时 Modbus TCP 持续轮询无丢包
- [ ] 网线插拔 100 次能正常恢复
- [ ] BLE Mesh 配网/断网 10 次能正常工作
- [ ] 内存占用稳定（不持续增长）
- [ ] 任务堆栈无溢出（`CONFIG_FREERTOS_WATCHPOINT_END_OF_STACK=y`）

## 性能基准

- [ ] Modbus TCP 响应延迟 < 50ms
- [ ] Modbus RTU 响应延迟 < 100ms (9600bps)
- [ ] DI 变化到总线更新延迟 < 5ms (1ms 扫描 + 3 次去抖)
- [ ] DO 写到 GPIO 输出延迟 < 20ms (10ms 周期)
- [ ] AI 采样到总线更新延迟 < 100ms
- [ ] BLE Mesh OnOff Set 到 DO 输出延迟 < 200ms (心跳周期)

## BLE 验证 (新)

### 启动日志检查
- [ ] `[ble_at] service starting (uuid=4fafc201-...)` 应在初始化时打印
- [ ] `[ble_at] controller config magic=0x...` 显示 magic 正确
- [ ] `[ble_at] Classic BT memory release returned 0x...` (S3 上是 NOT_FOUND, 正常)
- [ ] `[ble_at] BLE device name set to 'GW-XXXXXX'` 显示 BLE 名称
- [ ] `[ble_at] BLE MAC = XX:XX:XX:XX:XX:XX` 显示蓝牙 MAC
- [ ] `[ble_at] ETH MAC = XX:XX:XX:XX:XX:XX` 显示以太网 MAC
- [ ] `[ble_at] local MTU set to 247` MTU 设置成功
- [ ] `[ble_at] GATT app registered` GATT 应用注册
- [ ] `[ble_at] GOT CREAT_ATTR_TAB_EVT` 属性表创建事件
- [ ] `[ble_at] GATT service started` 服务启动
- [ ] `[ble_at] ADV_DATA configured` 广播数据配置
- [ ] `[ble_at] SCAN_RSP configured` 扫描响应配置 (含 name + txpower)
- [ ] `[ble_at] SCAN_RSP configured; starting advertising...` 启动广播
- [ ] `[ble_at] BLE advertising active (svc=4fafc201-...)` 广播已激活

### 移动端扫描测试
- [ ] 打开 nRF Connect 或 手持机 APP
- [ ] 扫描 BLE 设备, 应该看到 "GW-XXXXXX" (后 3 字节是 MAC 地址)
- [ ] 点击连接, 应能成功连接
- [ ] MTU 协商 = 247 (查看 nRF Connect 详情)
- [ ] 服务列表中应显示 4fafc201-1fb5-459e-8fcc-c5c9c331914b
- [ ] 特征 b/eb5483e-36e1-4688-b7f5-ea07361b26a8 应显示 Read/Write/Notify 属性
- [ ] Subscribe Notify 应能成功
- [ ] 写入 "AT\r\n" 到 characteristic, 设备应通过 Notify 返回 "OK\r\n"

## 单元测试运行

```bash
# 这些测试是 #[cfg(test)] mod tests, 嵌入在源码中
# 由于项目链接 esp-idf-sys, 标准的 cargo test 无法运行
# 但可通过文档测试或 device 上运行的集成测试验证

# 验证代码结构正确:
cargo check              # 默认 features
cargo check --features f3 # F3 版本 (16 DI + 16 DO)
cargo check --features f4 # F4 版本 (48 DI, 无 DO)
```

## F3/F4 版本特定测试

### F3 (16 DI + 16 DO)
- [ ] 编译时启用: `cargo build --features f3`
- [ ] `cfg::hw_version::NAME` == "F3"
- [ ] `cfg::hw_version::DI_COUNT` == 16
- [ ] `cfg::hw_version::DO_COUNT` == 16
- [ ] 3 片 MCP23017 在 I2C 总线 (DI@0x20, DO@0x21)
- [ ] 读 DI 0..15 正确
- [ ] 写 DO 0..15 正确

### F4 (48 DI + 48 DO)
- [ ] 编译时启用: `cargo build --features f4`
- [ ] `cfg::hw_version::NAME` == "F4"
- [ ] `cfg::hw_version::DI_COUNT` == 48
- [ ] `cfg::hw_version::DO_COUNT` == 48
- [ ] `cfg::hw_version::DO_EXT_CHIPS` == 3
- [ ] 6 片 MCP23017 在 I2C 总线
  - DI: 0x20 / 0x21 / 0x22 (各 16 路)
  - DO: 0x23 / 0x24 / 0x25 (各 16 路)
- [ ] 读 DI 0..47 正确
- [ ] 写 DO 0..47 正确 (一次 I2C 写 3 片, 约 600μs)
- [ ] 读 DO 缓存正确
