# 架构阶段方案 (2026-07-20)

> 由高级系统架构师出具, 基于 PM_GAP_ANALYSIS_2026-07-20.md 的差距清单.
> 目标: 在不破坏已有稳定基线 (无锁 + Actor + RCU + P0/P1/P3) 的前提下,
> 按 4 个阶段补齐 Android 1.0.78 业务流.

## 1. 阶段划分原则

| 阶段 | 范围 | 风险 | 验收 |
|------|------|------|------|
| **阶段 1**: 持久化基线 | P0-B 写入 NVS, P1-A HW info 打包, P1-C RS485 执行结果 | 低 | host test + 实机复位 |
| **阶段 2**: 文本与功能配置 | P0-C 文本数据, P0-A 功能计数+配置 (精简版) | 中 | host test + 实机握手 |
| **阶段 3**: 主动上报 | P1-B DI 主动 notify (REPORT_COM_INPUT_IO_STATUS) | 中 | BLE notify 链路 |
| **阶段 4**: 工业高级 | P2-A multicast, P2-B logic config, P2-C LED | 高 | 专项设计 |

## 2. 阶段 1: 持久化基线 (P0-B + P1-A + P1-C)

### 2.1 范围

1. **写入命令 NVS 持久化** (P0-B):
   - WRITE_SN (0x21 / reg 0x0894): 写后异步提交至 NVS
   - WRITE_LOCATION (0x31 / reg 0x089D)
   - WRITE_BLUETOOTH_ID (0x51 / reg 0x08E2)
   - WRITE_IP (0x71 / reg 0x08C7)
   - WRITE_RS485_*_CONFIG (0xA1/A3/A5 / regs 0x08A6/0x08AB/0x08B0)
   - WRITE_DEVICE_TEXT_DATA (0xB7 / reg 0x138A)

2. **READ_HARDWARE_INFO (0x81)**: 复用 INREG_QI_COUNT + INREG_ADC485 + INREG_AI_COUNT 组合
   - 格式: [Q_count_high, I_count_low, ADC_count, RS485_count]
   - 与 Android CMDResHardwareInfoReadModel 对齐

3. **READ_RS485_EXECUTE_RESULT (0xC1)**: 维护一个 16 字缓冲
   - 写入 RS485 master 命令后填结果
   - RS485 master 周期填最近 16 次执行结果 (成功/失败/响应时间)

### 2.2 关键设计决策

**D-1.1 持久化策略**:
- 写入 holding_buf 后, 立刻触发 `device::schedule_commit()`
- 与 AT 通路 `CFG_COMMIT` 复用同一 commit 路径
- 不引入新的同步阻塞, 全部走 Actor mailbox

**D-1.2 写权限边界**:
- 0x0880-0x0883 (485 ERR): RO, 已在 P1 实现
- 0x4000+ (PROTO): RW, 已支持
- 0xFF00-0xFF11 (CFG_*): 已有 AT 通路
- 0x107F+ (TEXT): 新加 RW 写入

**D-1.3 文本数据布局**:
- 0x1388: count (1 reg = text count + total len, 2 字节对齐)
- 0x138A+: 文本数据 (UTF-16LE, 1 字符 1 reg, 最高 1998 字符)
- 已存在的 `device_text: Box<[u16]>` 即为此区, 写入走同路径

### 2.3 不引入

- 不改 Modbus FC 处理 (已经通用)
- 不改 BLE 二进制协议 (1.0.78 兼容)
- 不改 RCU/Actor/Spin 基础设施

## 3. 阶段 2: 文本与功能配置 (P0-A + P0-C)

### 3.1 范围

1. **0x1388 文本计数** (P0-C):
   - 0x1388: text_count (1 reg)
   - 0x1389: total_bytes (1 reg)
   - 0x138A+: text_data (UTF-16LE, N regs)

2. **0x08FC 设备功能计数** (P0-A 简化版):
   - 0x08FC: function_count (1 reg)
   - 0x08FE+: function_config (复杂 TLV, 每条 ≥10 regs)

3. **功能配置 TLV 简化**:
   - 类型 (1 reg): IO_CONTROL / RS485 / ANALOG / BACKGROUND
   - 长度 (1 reg): 后续 reg 数
   - payload (N regs): 类型相关
   - 与 Android `CMDDeviceFunction*Item` 对齐

### 3.2 风险

- TLV 解析复杂, 但 Android 端有完整模型, 我们只需实现 ON-WIRE 字节序
- 0x08FE+ 大小未知, 上限 500 regs 足够 (Android 实测平均 30-50 regs/func)

## 4. 阶段 3: 主动上报 (P1-B)

### 4.1 范围

- REPORT_COM_INPUT_IO_STATUS (0x94): DI 边沿变化触发 BLE notify
- 需 DI 边沿检测 (上升沿/下降沿) + BLE notify 路径
- 与现有 BLE notify 链路复用 (BINARY_TX)

### 4.2 风险

- BLE notify 不能在 GATT 回调中调用 → 走 BINARY_TX 队列 (已有)
- 边沿检测去抖: 5ms 内多次触发合并

## 5. 阶段 4: 工业高级 (P2) - 延后设计

- multicast UDP: 与 TCP server 资源共享需评估
- Logic Config: 隧道联动逻辑, 单开 task
- LED Control: LEDC PWM, 已有 AO 通路可复用

## 6. 验收矩阵

| 阶段 | cargo check | host test | 实机烧录 | 业务测试 |
|------|-------------|-----------|----------|---------|
| 1 | 0 errors | 36+ 新增 | espflash + monitor | Android 写 SN/IP/RS485 后复位保留 |
| 2 | 0 errors | 20+ 新增 | espflash | Android 读写 0x08FC/0x08FE/0x1388/0x138A |
| 3 | 0 errors | 5+ 新增 | espflash | Android 触发 DI 变化接收 notify |
| 4 | 0 errors | 10+ 新增 | espflash | multicast 探测 / logic 联动 |

## 7. 顺序

1. 测试工程师先补 host test 用例 (现有 register layout test)
2. 架构师出 TLV 编码草案 (阶段 2 用)
3. 开发工程师按阶段 1 → 2 → 3 实施, 每阶段出审计报告
4. 实机验证每阶段独立烧录一次
