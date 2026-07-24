# 烧录指南 (ESP32-S3 工业网关)

> 最后更新: 2026-07-24 (LOOP7 BLE 名字同步修复后)
> 工具: `espflash 4.5.0` (cargo install espflash)
> 串口: `/dev/cu.usbserial-1430`
> 芯片: ESP32-S3 (Xtensa LX7 双核, 8MB Flash, 2MB PSRAM)

---

## 一、前置条件

```bash
# 1. 安装 espflash (一次性)
cargo install espflash --locked

# 2. 安装 ldproxy (rust esp-idf 依赖)
cargo install ldproxy

# 3. 确认工具
cargo --version
espflash --version    # espflash 4.5+

# 4. 确认串口权限
ls -la /dev/cu.usbserial-1430
# 如无权限: sudo chmod 666 /dev/cu.usbserial-1430
```

---

## 二、烧录命令

### 2.1 标准完整烧录 (推荐)

```bash
cd /Users/takumi/Workspace/idf
cargo build && \
espflash flash --port /dev/cu.usbserial-1430 \
    --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
```

| 参数 | 作用 |
|---|---|
| `--port /dev/cu.usbserial-1430` | 串口路径 (macOS) |
| `--no-skip` | **强制重写所有 flash 块** (即使 checksum 相同). 若不加, 重复烧同一版本会快速跳过 (4秒就退) |
| `target/.../gateway` | ELF 镜像, espflash 自动提取 bootloader / partition-table / app 三段 |

### 2.2 强制全擦后烧录 (如烧录失败/固件异常)

```bash
espflash erase-flash --port /dev/cu.usbserial-1430
espflash flash --port /dev/cu.usbserial-1430 --no-skip target/.../gateway
```

### 2.3 烧录后立即监控

```bash
espflash flash --port /dev/cu.usbserial-1430 --no-skip target/.../gateway && \
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

进入 monitor 后:
- `Ctrl+R` — 重置芯片 (看 `rst:` 重启日志)
- `Ctrl+C` — 退出 monitor

---

## 三、可能遇到的问题

### 3.1 `Failed to connect to the device`

**原因**: 芯片残留 ROM 监控或 DTR/RTS 没复位.

**解决**:

```bash
pkill -9 -f espflash
pkill -9 -f expect
sleep 2
espflash flash --port /dev/cu.usbserial-1430 --no-skip target/.../gateway
```

如仍失败:
- 按住 BOOT 按钮, 然后插拔 USB
- 或按住 BOOT 后按一下 EN/RST 按钮

### 3.2 烧录到 17.45% 后无输出

**原因**: 上一次烧入的固件正在运行, 干扰握手.

**解决**: 加 `--no-skip` + 确保 monitor 已完全退出.

### 3.3 `Stack canary watchpoint triggered (sys_evt)` → 设备重启

**触发场景**: `IP_EVENT_ETH_GOT_IP` 回调执行了过多元代码.

**已修复**: commit `8e148ef` 把 `ip_event_cb` 改为极简版 (只 post 事件, 不做 RCU 写), 重活在 main_loop.

**自检方法**:
```bash
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
# 看到 [eth] main loop: IP assigned xxx.xxx.xxx.xxx 即为正常
# 看到 Guru Meditation Error 即复发
```

### 3.4 烧录后看不到日志 (115200 串口)

```bash
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
# 必须显式 --monitor-baud 115200, 否则 espflash 默认用 chip 实际波特率
```

---

## 四、烧录产物清单

`cargo build` 产物在 `target/xtensa-esp32s3-espidf/debug/`:

| 文件 | 大小 | 烧录位置 | 说明 |
|---|---|---|---|
| `gateway` | ~22 MB ELF | 0x10000 (app) | 应用镜像 |
| `bootloader.bin` | ~22 KB | 0x0000 | 2nd stage bootloader |
| `partition-table.bin` | ~3 KB | 0x8000 | 分区表 (含 nvs, phy_init, factory) |

当前分区布局:

```
Label          Type   Offset    Size
nvs            0x01   0x9000    24 KB   (WiFi/BLE 配置存储)
phy_init       0x01   0xf000     4 KB   (RF 校准)
factory        0x00   0x10000    8 MB   (应用)
```

---

## 五、完整工作流 (一键脚本)

```bash
cd /Users/takumi/Workspace/idf
cargo build 2>&1 | tail -5
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway 2>&1 | tail -10
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

后台记录日志:

```bash
expect -c '
  set timeout 60
  log_file -a /tmp/gateway.log
  spawn /Users/takumi/.cargo/bin/espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
  expect "Commands:" { send "\x12" }
  interact
'
```

---

## 六、最近一次成功烧录记录

| 项 | 值 |
|---|---|
| 时间 | 2026-07-21 22:48 |
| Binary | `target/xtensa-esp32s3-espidf/debug/gateway` 21,958,808 bytes |
| Commit | `8e148ef` fix(eth): sys_evt Stack canary panic |
| ESP-IDF | v5.5.4 |
| Flash size | 8 MB |
| 烧录耗时 | ~30 秒 (--no-skip 强制重写) |
| 验证 | 设备启动 24 秒+ 无 panic, 主循环 tick 正常 |
| 已分配 IP | 192.168.51.140 |

---

## 七、下次烧录命令 (拷贝即用)

```bash
cargo build && espflash flash --port /dev/cu.usbserial-1430 --no-skip target/xtensa-esp32s3-espidf/debug/gateway
```

---

## 八、LOOP7 后烧录流程更新

### 8.1 完整烧录流程 (强制刷入完整固件)

```bash
# 1. 完整擦除 flash
espflash erase-flash --port /dev/cu.usbserial-1430

# 2. 烧录 bootloader + 分区表 + app
espflash flash --port /dev/cu.usbserial-1430 --no-skip     target/xtensa-esp32s3-espidf/debug/gateway

# 3. 硬复位 (DTR 拉低 500ms 让 EN 断电, 见 §8.2)
python3 -c "
import serial, time
ser = serial.Serial('/dev/cu.usbserial-1430', 115200, timeout=1)
ser.dtr = True; time.sleep(0.5); ser.dtr = False
time.sleep(15)  # 等设备冷启动 + DHCP
ser.close()
"

# 4. 找 IP
python3 -c "
import serial, time, re
ser = serial.Serial('/dev/cu.usbserial-1430', 115200, timeout=1)
end = time.time() + 10; buf = b''
while time.time() < end:
    try: d = ser.read(4096); buf += d if d else b''
    except: pass
ser.close()
m = re.search(rb'ip: ([\d.]+)', buf)
print(m.group(1).decode() if m else 'NO IP')
"
```

### 8.2 DTR/RTS 硬复位详解 (LOOP7 实测)

| 状态 | 操作 | 时序 |
|------|------|------|
| 软复位 (DTR pulse 100ms) | `ser.dtr = True; sleep(0.1); ser.dtr = False` | 通常不够, esp32s3 + CH340 在反复擦除后失灵 |
| 半硬复位 (RTS 高电平) | `ser.rts = True; sleep(0.5); ser.rts = False` | 触发 BOOT 引脚高, 可能进入下载模式 |
| **完全断电 (推荐)** | `ser.dtr = True; sleep(0.5); ser.dtr = False` | DTR 接 EN 引脚, 拉低断电, **必触发 POR** |
| 完全拔 USB | 物理操作 | 等 5 秒, 100% 可靠 |

### 8.3 LOOP7 验证烧录

```bash
# 烧录后验证 BLE 名字写入路径
# 1. 读默认
python3 -c "
import socket, struct
def mb(pdu, ip='192.168.51.140'):
    s = socket.socket(); s.settimeout(3)
    try: s.connect((ip, 502)); s.sendall(struct.pack('>HHHB', 1, 0, len(pdu)+1, 1) + pdu); import time; time.sleep(0.3); r = s.recv(256); return r
    finally: s.close()
r = mb(struct.pack('>BHH', 0x03, 0x08E2, 4))
print(r.hex())
# 期望: cfg 默认 'Mesh' (0x4d 0x00 0x65 0x00 0x73 0x00 0x68 0x00)
# MBAP + 0x03 + 长度 4 + Mesh 字节

# 2. 写 'XY' (2 字符)
name = 'XY'.encode('utf-16-be') + b'\x00' * 6
regs = [struct.unpack('>H', name[i:i+2])[0] for i in range(0, 8, 2)]
pdu = struct.pack('>BHHB', 0x10, 0x08E2, 4, 8)
for r in regs: pdu += struct.pack('>H', r)
rsp = mb(pdu)
print('write:', 'OK' if rsp else 'FAIL')

# 3. 读回
r = mb(struct.pack('>BHH', 0x03, 0x08E2, 4))
body = r[7:]
length = body[1]
sn = body[2:2+length]
print(f'cfg.ble_name = {sn.decode("utf-16-be").rstrip(chr(0))!r}')
# 期望: 'XY' (LOOP7 修复前是 '', 修复后是 'XY')
```

### 8.4 烧录失败常见问题

| 症状 | 原因 | 解决 |
|------|------|------|
| 烧录后 App version 还是旧 | esp-idf-sys 没重链接 | `touch build.rs && rm -rf target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-* && cargo build` |
| 烧录后设备没 IP | DTR/RTS 软复位失灵 | 用 §8.2 DTR 500ms 完全断电复位 |
| 烧录后设备不启动 | erase-flash 后 W5500 SPI 卡住 | 完全拔 USB 重插 5 秒 |
| `Failed to open serial port` | 上一个进程占用了 | 关闭 espflash monitor / minicom / putty |
| `Wrong boot mode detected (0x13)!` | ESP32 进入了下载模式 | 短按 RST 重新上电, 不要按 BOOT |

### 8.5 烧录 + 重置一键命令 (LOOP7 验证通过)

```bash
# 一键完整烧录 + 硬复位 + 找 IP
ESPFLASH=espflash && SER=/dev/cu.usbserial-1430 && espflash erase-flash --port $SER && espflash flash --port $SER --no-skip target/xtensa-esp32s3-espidf/debug/gateway && python3 -c "
import serial, time
ser = serial.Serial('$SER', 115200, timeout=1)
ser.dtr = True; time.sleep(0.5); ser.dtr = False
time.sleep(15)
ser.close()
" && python3 -c "
import serial, time, re
ser = serial.Serial('$SER', 115200, timeout=1)
end = time.time() + 10; buf = b''
while time.time() < end:
    try: d = ser.read(4096); buf += d if d else b''
    except: pass
ser.close()
m = re.search(rb'ip: ([\d.]+)', buf)
print(f'设备 IP: {m.group(1).decode() if m else "NO IP"}')"
```
