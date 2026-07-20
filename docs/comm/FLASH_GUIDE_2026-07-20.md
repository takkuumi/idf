# ESP32-S3 烧录指南 (2026-07-20)

> 完整烧录流程 + 常见问题解决. 适用于 esp32s3-iot-gateway 项目.

## 1. 一键烧录 + 监控（推荐流程）

```bash
cd /Users/takumi/Workspace/idf
cargo build && \
espflash flash --port /dev/cu.usbserial-1430 target/xtensa-esp32s3-espidf/debug/gateway && \
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

**关键步骤**:
1. `cargo build` — 编译 release/debug 固件 (生成 ELF)
2. `espflash flash` — 通过 `/dev/cu.usbserial-1430` 串口烧录. 自动处理:
   - 识别芯片 (esp32s3 v0.2)
   - 检测 Flash 大小 (8MB)
   - 烧 bootloader + partition table + app
3. `espflash monitor` — 进入串口监控模式
4. **进入 monitor 后按 `Ctrl+R` 重启设备**, 即可看到启动日志

## 2. 串口选择

| 路径 | 用途 |
|------|------|
| `/dev/cu.usbserial-1430` | **主用** (callout, macOS 默认) |
| `/dev/tty.usbserial-1430` | 备选 (dial-in, 某些工具需要) |

可用 `espflash list-ports` 查看当前可用端口.

## 3. 烧录输出解读

成功烧录的典型输出:
```
[2026-07-20T14:51:27Z WARN ] Enable feature socks-proxy to use proxy
                                    configured via environment variables
Chip type:         esp32s3 (revision v0.2)
Crystal frequency: 40 MHz
Flash size:        8MB
Features:          WiFi, BLE, Embedded Flash
MAC address:       80:b5:4e:5b:24:e4
App/part. size:    1,401,248/8,323,072 bytes, 16.84%
```

含义:
- **Chip type**: esp32s3 v0.2
- **Flash size**: 8MB Quad SPI
- **App/part. size**: 1.4MB / 8.3MB (16.84% 使用率)

## 4. 常见问题与解决

### 4.1 `Error: Error while connecting to device`

**症状**: 烧录立即失败, 显示 "Failed to connect to device"

**原因**:
- 设备已上电运行, 但自动重置 (DTR/RTS) 失败
- USB-Serial 适配器线序不对

**解决**:
1. **方法 A (推荐)**: 手动重启设备 (拔插 USB 或按 RST 按钮), 然后**立即**重试烧录
   ```bash
   espflash flash --port /dev/cu.usbserial-1430 target/...
   ```
2. **方法 B**: 使用 `--before no-reset` (假设设备已在 bootloader 状态)
3. **方法 C**: 重置 ESP32-S3 GPIO0 引脚 (低电平进入 bootloader)

### 4.2 `Failed to open serial port ... Operation not permitted`

**原因**: macOS 串口权限问题. 串口设备 `/dev/cu.usbserial-1430` 是 root:wheel 拥有

**解决**:
- 在沙箱环境外运行 (`require_escalated` 权限)
- 或修改 udev 规则 (Linux)
- 或加入 dialout 组 (Linux)

### 4.3 `Operation not permitted` (ps/lsof/stty)

**症状**: 监控 / 调试命令无法执行

**原因**: macOS sandbox 限制 `ps`, `lsof`, `stty` 等命令

**解决**: 用 `require_escalated` 权限运行

### 4.4 `Timeout while running ReadReg command`

**症状**: 烧录慢速启动, 然后超时

**原因**: 波特率太高 (>115200) 或硬件问题

**解决**:
```bash
espflash flash --port /dev/cu.usbserial-1430 --baud 115200 target/...
```

## 5. 监控命令详解

### 5.1 基本监控
```bash
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

### 5.2 监控时键盘快捷键
- `Ctrl+R` — **重启设备** (关键!)
- `Ctrl+C` — 退出监控
- `Ctrl+D` — 关闭串口 (但保留连接)

### 5.3 不重置监控 (看现有运行状态)
```bash
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200 --no-reset
```

## 6. 完整工作流示例

```bash
# 1. 编辑代码
vim src/main.rs

# 2. 编译 (debug, 带调试信息)
cargo build

# 3. 烧录
espflash flash --port /dev/cu.usbserial-1430 \
    target/xtensa-esp32s3-espidf/debug/gateway

# 4. 监控 + 重启
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
# (进入后按 Ctrl+R 重启设备, 观察启动日志)

# 5. 验证关键日志:
#    [main] starting ethernet (W5500)...
#    [main] entering main loop (period=100ms)
#    [main] uptime=1s tick=10
```

## 7. 不同 feature flag 烧录

### 7.1 默认 (F16, 16 DI + 16 DO 直驱)
```bash
cargo build
```

### 7.2 F3 (16 DI + 16 DO I2C 扩展)
```bash
cargo build --features f3
espflash flash --port /dev/cu.usbserial-1430 \
    target/xtensa-esp32s3-espidf/debug/gateway
```

### 7.3 F4 (48 DI + 48 DO I2C 扩展)
```bash
cargo build --features f4
espflash flash --port /dev/cu.usbserial-1430 \
    target/xtensa-esp32s3-espidf/debug/gateway
```

## 8. 故障排查清单

- [ ] 串口路径正确 (`ls /dev/cu.*`)
- [ ] 设备已上电 (LED 亮)
- [ ] 串口权限 OK (`crw-rw-rw-`)
- [ ] Flash 大小匹配 (8MB)
- [ ] 烧录完成后 **按 Ctrl+R 重启**
- [ ] 监控波特率匹配 (`CONFIG_ESP_CONSOLE_UART_BAUDRATE=115200`)

## 9. 完整的环境变量

```bash
# 关闭 socks 代理警告 (本机有 proxy 设置)
ESPFLASH_SKIP_UPDATE_CHECK=1

# 或直接清空代理 (烧录不需外网)
unset all_proxy http_proxy https_proxy
```

## 10. 与 cargo-espflash 集成

也可使用 cargo 子命令:
```bash
cargo espflash flash --port /dev/cu.usbserial-1430
cargo espflash monitor --port /dev/cu.usbserial-1430
```

(需要 `cargo install cargo-espflash`, 已安装在 ~/.cargo/bin)
