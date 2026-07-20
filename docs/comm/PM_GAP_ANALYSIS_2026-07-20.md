# 产品经理差距分析 (2026-07-20)

> 对照 MCA_F16V2_1_F48_BLE 与 metuory-wireless-management-app-1.0.78,
> 列出本系统(esp32s3-iot-gateway)尚未支持或部分支持的功能.

## 1. Android 1.0.78 业务交互流程

### 1.1 完整 BLE 命令清单 (tx_id 维度)

| tx_id | 命令名 | 寄存器 | FC | 我们状态 | 备注 |
|------:|--------|------:|---:|----------|------|
| 0x00 | HEARTBEAT | - | 0x11 | ✅ | 实现 |
| 0x10 | READ_ADC_VALUE | 0x0080 | 04 | ✅ | 走 Modbus, 已实现 |
| 0x20 | READ_SN | 0x0894 | 03 | ⚠️ 部分 | 读 OK, 无自定义 0xC2 路径 |
| 0x21 | WRITE_SN | 0x0894 | 10 | ⚠️ 部分 | 写入未走 NVS 持久化 |
| 0x30 | READ_LOCATION | 0x089D | 03 | ⚠️ 部分 | 读 OK |
| 0x31 | WRITE_LOCATION | 0x089D | 10 | ⚠️ 部分 | 写入未持久化 |
| 0x40 | READ_MAC | 0x08D7 | 03 | ⚠️ 部分 | 通过 0xC6 走 |
| 0x50 | READ_BLUETOOTH_ID | 0x08E2 | 03 | ⚠️ 部分 | 通过 0xCA 走 |
| 0x51 | WRITE_BLUETOOTH_ID | 0x08E2 | 10 | ❌ | 无 SET 实现 |
| 0x60 | READ_DEVICE_PRODUCT | 0x08A5 | 03 | ⚠️ 部分 | 通过 0xCC 走 |
| 0x70 | READ_IP | 0x08C7 | 03 | ⚠️ 部分 | 通过 0xCE 走 |
| 0x71 | WRITE_IP | 0x08C7 | 10 | ⚠️ 部分 | 通过 0xCD 走, 但需重启生效路径 |
| 0x80 | READ_FW_VERSION | 0x087E | 04 | ⚠️ 部分 | 通过 0xCF 走 |
| 0x81 | READ_HARDWARE_INFO | 0x087C | 04 | ❌ | 未实现 |
| 0x90 | READ_COM_INPUT_IO_STATUS | 0x0000 | 01 | ✅ | 走 Modbus |
| 0x91 | READ_COM_OUTPUT_IO_STATUS | 0x0200 | 01 | ✅ | 走 Modbus |
| 0x92 | WRITE_COM_OUTPUT_IO_STATUS | 0x0000+ | 05 | ✅ | 走 Modbus |
| 0x93 | WRITE_COM_OUTPUT_MULTI_IO_STATUS | 0x0000+ | 0F | ✅ | 走 Modbus |
| 0x94 | REPORT_COM_INPUT_IO_STATUS | 0x0000 | - | ⚠️ 部分 | 设备主动上报未实现 |
| 0xA0 | READ_RS485_1_CONFIG | 0x08A6 | 03 | ⚠️ 部分 | 寄存器映射有, 但需校验读写全链路 |
| 0xA1 | WRITE_RS485_1_CONFIG | 0x08A6 | 10 | ⚠️ 部分 | 同上 |
| 0xA2 | READ_RS485_2_CONFIG | 0x08AB | 03 | ⚠️ 部分 | 同上 |
| 0xA3 | WRITE_RS485_2_CONFIG | 0x08AB | 10 | ⚠️ 部分 | 同上 |
| 0xA4 | READ_RS485_3_CONFIG | 0x08B0 | 03 | ⚠️ 部分 | 同上 |
| 0xA5 | WRITE_RS485_3_CONFIG | 0x08B0 | 10 | ⚠️ 部分 | 同上 |
| 0xB0 | READ_DEVICE_FUNCTION_COUNT | 0x08FC | 03 | ❌ | **0x08FC 寄存器未定义** |
| 0xB1 | WRITE_DEVICE_FUNCTION_COUNT | 0x08FC | 10 | ❌ | 同上 |
| 0xB2 | READ_DEVICE_FUNCTION_CONFIG | 0x08FE+ | 03 | ❌ | **功能配置区未实现** |
| 0xB3 | WRITE_DEVICE_FUNCTION_CONFIG | 0x08FE+ | 10 | ❌ | 同上 |
| 0xB4 | READ_DEVICE_TEXT_COUNT | 0x1388 | 03 | ❌ | **文本区头未解析** |
| 0xB5 | WRITE_DEVICE_TEXT_COUNT | 0x1388 | 10 | ❌ | 同上 |
| 0xB6 | READ_DEVICE_TEXT_DATA | 0x138A | 03 | ⚠️ 部分 | DEVICE_TEXT_BASE=5000=0x1388 ✓, 但读写未实现 |
| 0xB7 | WRITE_DEVICE_TEXT_DATA | 0x138A | 10 | ⚠️ 部分 | 同上 |
| 0xB8 | READ_CONTROL_ADDRESS | 0x0000 | 03 | ✅ | 走 Modbus FC=03 |
| 0xB9 | WRITE_CONTROL_ADDRESS | 0x0000 | 10 | ✅ | 走 Modbus FC=10 |
| 0xC0 | MODBUS_COMMAND | - | - | ✅ | raw modbus 透传 |
| 0xC1 | READ_RS485_EXECUTE_RESULT | 0x0000 | 03 | ❌ | **执行结果缓冲未实现** |
| 0xD0-DF | READ_RS485_INDEX1-16_VALUE | 0x0000 | 03 | ❌ | **RS485 数值表未实现** |
| 0xE0-EF | READ_RS485_CUSTOM_INDEX1-16_VALUE | 0x0000 | 03 | ❌ | **自定义 RS485 数值表未实现** |

### 1.2 P0 - 必须支持 (高优先级)

1. **0x08FC 设备功能计数 / 0x08FE+ 设备功能配置**
   - 缺失整个寄存器区
   - Android 端是核心功能 (FunctionConfig IOControl RS485 Analog Background)
   - 影响: 手持机无法配置设备功能
2. **0x1388 文本计数 / 0x138A 文本数据**
   - 文本区映射已有 (DEVICE_TEXT_BASE=5000=0x1388)
   - 但读写/持久化未实现
   - 影响: 设备名称/位置/桩号等字符串无法持久化到手持机
3. **写入命令的 NVS 持久化 (WRITE_SN / WRITE_LOCATION / WRITE_BLUETOOTH_ID / WRITE_IP / WRITE_RS485_*_CONFIG)**
   - 当前写操作仅改 holding_buf, 不入 NVS
   - 复位后丢失 → 工业不可接受
   - 影响: 工程师无法持久化配置

### 1.3 P1 - 应该支持 (中优先级)

4. **READ_HARDWARE_INFO (0x81)**: 0x087C QI count 已有, 但需要包成 HW info 格式
5. **REPORT_COM_INPUT_IO_STATUS (0x94)**: 设备主动上报 DI 变化
6. **READ_RS485_EXECUTE_RESULT (0xC1)**: RS485 master 执行结果缓冲
7. **READ_RS485_INDEX/CUSTOM_INDEX 1-16 (0xD0-EF)**: RS485 数据表 (从站轮询结果)

### 1.4 P2 - 可选 (低优先级, 延后)

8. **Multicast UDP**: MCA 用了, Android 不依赖
9. **Logic Config (0xD0/0xD1)**: 隧道信号灯/风机联动逻辑, 工业专用
10. **LED Control (0xB0)**: 显示灯光控制

## 2. 与 MCA_F16V2_1_F48_BLE 的内存布局对比

### 2.1 已对齐 ✅

| MCA 地址 | 名称 | 我们地址 | 状态 |
|---------|------|---------|------|
| 0x0080-0x008F | AI 数据 + 状态 | INREG_AI_BASE..+AI_COUNT | ✅ AI_COUNT 现在 F48=8, F16=4 |
| 0x087C | QI count | INREG_QI_COUNT | ✅ |
| 0x087D | ADC 485 | INREG_ADC485 | ✅ |
| 0x087E-0x087F | FW ver/date | INREG_FW_VER/DATE | ✅ |
| 0x0880-0x0883 | 485 ERR | HOLD_485_*_COMERR/APPERR | ✅ RO=0 (本轮修复) |
| 0x0894-0x089C | SN 9 字 | HOLD_SN_BASE..+9 | ✅ |
| 0x089D-0x08A4 | PLACE 8 字 | HOLD_PLACE_BASE..+8 | ✅ |
| 0x08A5 | HW ver | HOLD_HW_VER | ✅ |
| 0x08A6-0x08B4 | RS485 1/2/3 配置 | HOLD_RS485_BASE..+15 | ✅ |
| 0x08C7-0x08D2 | IP/MASK/GW | HOLD_IP/MASK/GW_BASE | ✅ |
| 0x08D7-0x08DC | MAC | HOLD_MAC_BASE..+6 | ✅ |
| 0x08E2-0x08E5 | BT ADDR | HOLD_BT_ADDR_BASE..+4 | ✅ |
| 0x107F | PXX END | HOLD_PXX_END | ✅ |
| 0x1388-0x1B77 | 设备文本 | DEVICE_TEXT_BASE..+2000 | ✅ 映射对齐, **实现缺** |
| 0x4000-0x45DB | 协议存储 | PROTO_BASE..+1500 | ✅ |

### 2.2 未对齐 / 缺 ❌

| MCA 地址 | 名称 | 我们状态 |
|---------|------|---------|
| 0x08FC | 设备功能计数 | ❌ 完全缺失 |
| 0x08FE+ | 设备功能配置 | ❌ 完全缺失 |
| 0x08C3-0x08C6 | TCP_COM | ⚠️ 我们映射有 HOLD_TCP_COM_BASE=2243=0x8C3, 4 regs |

## 3. 优先级建议

| 优先级 | 项 | 影响 | 工作量 |
|------:|----|------|------|
| P0-A | 0x08FC / 0x08FE 设备功能寄存器 | 阻断手持机 1.0.78 业务 | 大 (新加 1000+ reg) |
| P0-B | 写入命令 NVS 持久化 | 工业可靠性 | 中 (5-6 处 write 改 AT 通路) |
| P0-C | 0x1388/0x138A 文本数据 | 阻断名称/位置持久化 | 中 |
| P1-A | READ_HARDWARE_INFO 包成格式 | 兼容性 | 小 |
| P1-B | DI 主动上报 0x94 | 用户体验 | 中 (BLE notify 链路) |
| P1-C | RS485 执行结果 (0xC1) | 调试便利 | 小 |
| P2-A | Multicast UDP | MCA 对齐 | 大 (新线程) |
| P2-B | Logic Config 0xD0/0xD1 | 工业专用 | 巨大 |
| P2-C | LED Control 0xB0 | 显示 | 小 |

## 4. 风险与依赖

- **网络封锁**: 无法升级 esp-idf-hal 至 0.47, 0x08FC+ 实现受限 (PATCH 仍在用)
- **NVS 容量**: 8MB 分区, NVS 默认 ~24KB, 写入新持久化需评估
- **PSRAM 限制**: 设备功能区可能 > 2KB, 走 PSRAM
- **BLE MTU**: 247 字节, 写功能配置需分片
