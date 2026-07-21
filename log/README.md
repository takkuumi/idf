# 项目测试日志与文档索引

> 创建: 2026-07-21
> 目的: 记录所有测试用例、运行日志、性能数据, 支持持续验证

## 目录结构

| 子目录 | 内容 | 用途 |
|--------|------|------|
| `sessions/` | 每次烧录/启动的设备日志 | 启动验证、问题回溯 |
| `hardware/` | 引脚核对、PSRAM/Flash 容量 | 硬件层验证 |
| `ble/` | BLE 协议兼容性测试 | Android 手持机兼容 |
| `modbus/` | Modbus TCP/RTU 完整测试 | 工业协议验证 |
| `unit/` | cargo test 单元测试报告 | 代码级验证 |
| `performance/` | 性能测量 (tick/notify/CRC) | 高性能验证 |
| `stress/` | 7×24 长稳测试 | 高可靠验证 |

## 当前测试矩阵

| 编号 | 类型 | 测试项 | 状态 | 文档 |
|------|------|--------|------|------|
| TC001 | 编译 | heapless 0.9.3 升级 | ✅ | `sessions/build_2026-07-21.md` |
| TC002 | 启动 | 设备启动无 panic | ✅ | `sessions/boot_2026-07-21.md` |
| TC003 | 启动 | sys_evt task Stack canary panic 修复 | ✅ | `sessions/boot_2026-07-21.md` |
| TC004 | 网络 | DHCP 获取 IP | ✅ | `sessions/boot_2026-07-21.md` |
| TC005 | BLE | 心跳响应 | ✅ | `ble/heartbeat_2026-07-21.md` |
| TC006 | BLE | IP/MAC/BLE_ID 显示 (Android) | 🆕 实现 | `ble/android_read_2026-07-21.md` |
| TC007 | 编译 | BLE Mesh 清理 | ✅ | `sessions/build_2026-07-21.md` |
| TC008 | 文档 | 引脚核对 | ✅ | `hardware/pinout_audit.md` |
| TC009 | 单元 | 无锁实现测试 | ⏳ 待跑 | `unit/lockfree_tests.md` |
| TC010 | Modbus | Modbus TCP 完整测试 | ⏳ 待跑 | `modbus/tcp_test.md` |
| TC011 | 性能 | Tick 抖动测量 | ⏳ 待跑 | `performance/tick_jitter.md` |
| TC012 | 长稳 | 7×24 持续运行 | ⏳ 待跑 | `stress/long_run.md` |

## 测试方法

### 单元测试 (host target)

```bash
cargo test --lib
cargo test --lib backends  # 特定模块
```

### 设备测试

```bash
# 1. 编译
cargo build

# 2. 烧录
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway

# 3. 监控
espflash monitor --port /dev/cu.usbserial-1430 --monitor-baud 115200
```

### Modbus TCP 测试

```bash
# 使用 pymodbus
python3 -m pymodbus.server --host 0.0.0.0 --port 502
# 或用 mbpoll
mbpoll -m tcp -p 502 -a 1 -t 4 -r 1 -c 10 192.168.51.140
```

## 当前未解决问题

- ⚠️ `[mb-rtu-master] poll slave=1 fc=03 failed` - Modbus RTU 总线 slave 1 无响应 (物理设备未接)
- ⚠️ `[main] stalled tasks: eth-heartbeat` - 以太网心跳任务卡死 (待查)
- ⚠️ Modbus TCP 服务器未启用 (需要 ethernet IP 起来后启动)
