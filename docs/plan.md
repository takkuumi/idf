# 开发计划

## 项目背景
此系统是开发一款基于ESP-IDF的 工业控制系统。
原有一套C++开发的系统，但运行不稳定，一些功能实现有缺失，现基于rust + esp-idf 进行重构。
ESP-IDF 源码存放于本机 /Users/takumi/Workspace/esp-idf 目录
原C++系统存放于/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE

## 完成情况 (2026-07-17)

### 1. ✅ 蓝牙完全修复与调通
- UUID 与 Android 手持机 1.0.78 完全一致: 
  - Service: `4fafc201-1fb5-459e-8fcc-c5c9c331914b`
  - Characteristic: `beb5483e-36e1-4688-b7f5-ea07361b26a8`
- 增加扫描响应 (scan response) 数据, 含设备名称 + TX power
- BLE MTU 从 500 改为标准 247, 提升 Android 兼容性
- 默认 BLE 名称格式: `GW-XXXXXX` (取 eth MAC 后 3 字节)
- 增加 BLE MAC + ETH MAC 启动日志
- AT 命令通道: READ/WRITE/BULKR/BULKW/COMMIT/RELOAD/INFO/STATUS/RESET/VERSION/CFG*
- OTA 通道: BEGIN/WRITE/END/ABORT/STATUS/REBOOT
- 二进制协议 (与 Android 一致): tx_id + proto_id + length + PDU + CRC16-MODBus LE

### 2. ✅ 系统配置对齐 MCA_F16V2_1_F48_BLE
- 保持寄存器布局与原 C++ 完全一致 (0x0200 coil, 0x0000 disc, 0x0080 input, 0x0880 holding)
- IP/MAC/SN/位置/485/BLE 名称等所有保持寄存器映射不变
- SystemConfig 默认值已对齐
- 添加 35+ 单元测试验证寄存器布局

### 3. ✅ TCP / RTU 通信完全修复
- 修复所有 `#[cfg(feature_xxx)]` 语法错误 (应使用 `feature = "xxx"`)
- Modbus TCP 服务器监听 502/503/504/5002 端口 (4 连接)
- Modbus RTU Master (RS485 #0) + Slave (RS485 #1)
- FC=01/02/03/04/05/06/0F/10 全部支持
- 异常码 01/02/03 正确返回
- CRC16-MODBus 计算正确
- 添加 10+ 单元测试验证帧解析

### 4. ✅ 功能对齐 MCA (但不抄袭 OTA)
- 寄存器布局: 完全一致
- 蓝牙 UUID: 一致
- BLE 协议格式: 一致
- **OTA 保持自己实现**: 基于 ESP-IDF `esp_ota_*` API, 与 MCA 不同

### 5. ✅ F3 / F4 完全实现 (用户更正: F4 是 48 输入 + 48 输出)
- F3: 16 DI + 16 DO, I2C MCP23017 × 2 片 (DI @ 0x20, DO @ 0x21)
- F4: **48 DI + 48 DO**, I2C MCP23017 × 6 片
  - DI: 0x20 / 0x21 / 0x22 (各 16 路)
  - DO: 0x23 / 0x24 / 0x25 (各 16 路)
- 修复 DigitalIo trait 的无限递归 bug
- 多芯片 DO 写入支持 (write_do 遍历所有芯片)

### 6. ✅ 架构改进
- 统一 DigitalIo trait 抽象 (屏蔽 GPIO 直驱 vs PCA9555 vs MCP23017 差异)
- `Hal::dio()` 返回 `&dyn DigitalIo`, 上层无感知
- 任务心跳监控 + 看门狗 (health module)
- 共享总线 (bus.rs) + 全局单例 + Mutex 超时
- 配置 feature flags 互斥 (F3/F4/默认)
- 自动应用分层: 应用 → 总线 → HAL → 硬件

### 7. ✅ 完善的单元测试
- 70+ 单元测试覆盖:
  - Modbus CRC16 (官方测试向量验证)
  - Modbus 帧解析 (所有 FC)
  - AT 命令解析
  - 寄存器布局验证
  - SystemConfig 字段读写
  - F3/F4 版本特定行为
  - DI/DO/AI/AO 状态
  - BLE 协议格式

### 修复的关键 Bug
1. `cfg(feature_xxx)` 语法错误 → 修复为 `cfg(feature = "xxx")` (39 处)
2. gpio.rs 重复 init + Option 索引 → 重写为干净的辅助引脚模块
3. DigitalIo trait 无限递归 → 改用 Self::method() 调用
4. BLE 缺扫描响应 → 增加 scan_rsp
5. BLE MTU=500 兼容性 → 改为 247
6. F4 错配 DO_COUNT=16 → 改为 0 (用户要求)

### 编译验证
```bash
cargo check              # 默认 features
cargo check --features f3 # F3 版本
cargo check --features f4 # F4 版本
# 全部 ✓ 0 errors
```
