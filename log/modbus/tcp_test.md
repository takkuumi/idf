# Modbus TCP 测试报告 (2026-07-21)

## TC010: Modbus TCP 完整测试 ✅ (部分通过)

### 测试环境

| 项 | 值 |
|----|-----|
| 测试机 | 192.168.51.201 (本机) |
| 设备 IP | 192.168.51.140 (DHCP) |
| 串口 | /dev/cu.usbserial-1430 |
| 端口 | TCP 502 (Modbus 标准) |
| 测试时间 | 2026-07-21 23:17-23:25 |

### 测试结果矩阵

| TC | 命令 | 寄存器 | 结果 | 备注 |
|----|------|--------|------|------|
| TCP-1 | READ_IP | FC=03 addr=0x08C7 cnt=12 | ❌ Connection reset | 大响应(27B), W5500 buffer 问题 |
| TCP-2 | READ_MAC | FC=03 addr=0x08D7 cnt=6 | ❌ Connection reset | 大响应(15B) |
| TCP-3 | READ_FW_VERSION | FC=04 addr=0x087E cnt=2 | ✅ 成功 | `recv 13 bytes: ...0001 0267` |
| TCP-4 | READ_HW_INFO | FC=04 addr=0x087C cnt=2 | ✅ 成功 | DO=16, DI=16, ADC=4, RS485=2 |
| TCP-5 | READ_HW_VER | FC=03 addr=0x08A5 cnt=1 | ❌ TIMEOUT | |
| TCP-6 | READ_BLE_ID | FC=03 addr=0x08E2 cnt=4 | ⏳ 未测 | |

### 通过的测试详细

#### TCP-3: READ_FW_VERSION
```
请求: 0001 0000 0006 01 04 087E 0002
响应: 0001 0000 0007 01 04 04 0001 0267 [crc_lo] [crc_hi]
      └──MBAP────┘ │ │  └─PDU──────┘
                   │ └── byte_count=4
                   └─ func=0x04
```
- FW_VER = 0x0001
- FW_DATE = 0x0267

#### TCP-4: READ_HW_INFO
```
请求: 0001 0000 0006 01 04 087C 0002
响应: 0001 0000 0007 01 04 04 1010 0402 [crc_lo] [crc_hi]
```
- regs[0] = 0x1010 → DO=0x10=16, DI=0x10=16 ✓
- regs[1] = 0x0402 → ADC=0x04=4, RS485=0x02=2 ✓

> 注: 这里 ADC=4 不是预期 6 (hw_version AI_COUNT=6), 需查 INREG_ADC485 的实现

### 失败的测试分析

#### TCP-1: READ_IP 失败
- 现象: `[Errno 54] Connection reset by peer`
- 响应大小: MBAP(7) + PDU(2 + 12 regs×2) = 31 字节
- 推测原因: W5500 socket buffer (默认 2KB) 应该够, 但 esp_eth driver 可能有默认 MTU/socket 配置

#### TCP-5: READ_HW_VER 失败
- 现象: TIMEOUT
- 响应大小: 11 字节 (较小)
- 推测原因: 寄存器 0x08A5 在 cfg_handlers 中是 HOLD_HW_VER = 2213，但 cfg_handlers::handle_cfginfo 显示 cfg.hw_version, 而 read_hold_reg 可能没覆盖到此地址

### 问题根因 & 修复建议

#### 问题 1: 大响应 TCP reset
W5500 默认 socket buffer 配置可能不足以处理并发请求。
**建议**: 在 sdkconfig.defaults 中:
```
CONFIG_ETH_SPI_ETHERNET_W5500=y
# 增大 socket buffer (默认 2KB → 8KB)
CONFIG_ETH_W5500_BUF_SIZE=8192
```

#### 问题 2: READ_HW_VER 返回超时
HOLD_HW_VER = 0x08A5 (2213) 不在 0x0880~0x107F 范围内 (HOLD_CFG_BASE 0x0880, END 0x107F)。
**建议**: 在 `read_hold_reg` 中扩展 cfg 读取范围, 或在 cfg_handlers 添加 fc=03 读取的兼容路径

### 后续测试建议

1. 增加 esp_eth socket buffer 配置
2. 用 pymodbus 替代 raw socket 测试 (自动重试 + 大响应处理)
3. 写串口并发测试脚本 (短间隔 100ms 持续请求 1 小时)
4. 多连接并发测试 (2-4 个客户端同时连接)
