# 系统持续开发集成 (LOOP.md)

> 最后更新: 2026-07-22 (Phase 2 完成)
> 详细进度: `log/SUMMARY_2026-07-22.md`

## 项目背景

此系统是开发一款基于ESP-IDF的 工业控制系统。
原有一套C++开发的系统（MCA_F16V2_1_F48_BLE），运行不稳定，现基于 rust + esp-idf 重构。

- ESP-IDF 源码: `/Users/takumi/Workspace/esp-idf` (禁止修改)
- 原 C++ 系统: `/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE` (禁止修改)
- 手持机源码: `/Users/takumi/Workspace/metuory-wireless-management-app-1.0.78` (禁止修改)

## 系统迭代 - 5 角色

| 角色 | 职责 |
|------|------|
| 产品经理 | 对照 MCA_F16V2_1_F48_BLE + metuory-wireless-management-app-1.0.78 提出缺失功能 |
| 高级 Rust 开发 | 实施功能与修复 BUG |
| 高级测试 | 测试 + 提出问题 |
| 高级系统架构 | 架构把关 (`docs/ARCHITECTURE.md`) |
| 工业软件审计 | 审计每次实施 |

## 任务完成清单

| # | 任务 | 状态 | 关键产出 |
|---|------|------|----------|
| 1 | heapless 升级 0.9.3 | ✅ | `Cargo.toml` |
| 2 | 手持机显示 IP/MAC/BLE_ID | ✅ | `handle_ble_android_read_command` + BLE_ID 用 ble_name |
| 3 | 硬件信息确认 | ✅ | `docs/pinmap.md` 重写 (ESP32-S3R2) |
| 4 | 无锁测试 + 栈估算 | ✅ | 127 测试 + 架构合并消除栈风险 |
| 5 | Modbus TCP 完整测试 | ✅ | FC=03/04 全部通过 |
| 6 | log/ 目录 + 详细日志 | ✅ | 7 子目录 + SUMMARY |
| 7 | mesh 清理 | ✅ | 死代码已删 |
| 8 | 引脚核对 | ✅ | pinmap.md 1:1 对齐 |
| 9 | 性能测试 | ✅ | Modbus TCP 11s 全部响应 |
| 10 | 7×24 不间断运行 | 🟡 | 1 小时长稳测试中 |
| 11 | 5 角色协作 | ✅ | 完整推进 |

## 架构 (Phase 2 完成)

```
main_loop (100ms tick)
├── tick_ai_sample(&hal)        # 100ms, 合并 ai-sample pthread
├── tick_ao_output(&hal)         # 100ms, 合并 ao-output pthread
├── tick_di_scan(&hal)           # 20ms (5 分频), 合并 di-scan pthread
├── tick_do_output(&hal)         # 100ms, 合并 do-output pthread (notify 立即触发)
└── tick_eth_heartbeat()         # 5s (50 分频), 合并 eth-heartbeat pthread

4 个保留 pthread 任务:
- DeviceActor (NVS 持久化)
- mb-rtu-master (Modbus RTU 主站)
- mb-rtu-slave (Modbus RTU 从站)
- mb-tcp-listen (Modbus TCP)
```

详细架构: [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)

## 注意事项

- 烧录: `espflash`, 串口 `/dev/cu.usbserial-1430`, 强制重置
- 禁止修改 esp-idf / MCA / metuory 源码
- 严禁抄袭 MCA/metuory 代码 (只参考业务)
- 所有决策需要我审批时 (用户睡觉中) 自动处理

## 烧录

```bash
cargo build && \
espflash flash --port /dev/cu.usbserial-1430 --no-skip \
    target/xtensa-esp32s3-espidf/debug/gateway
```

详细: [`docs/FLASH.md`](FLASH.md)

## 测试日志

`log/` 目录:
- `log/README.md` - 测试矩阵
- `log/SUMMARY_2026-07-22.md` - 最新工作总结
- `log/sessions/` - 启动/编译日志
- `log/hardware/pinout_audit.md` - 引脚核对
- `log/ble/android_read_2026-07-21.md` - BLE 兼容性
- `log/modbus/tcp_test.md` - Modbus TCP 测试
- `log/unit/lockfree_tests.md` - 无锁测试

## 最近 Commits

```
cd6fc08 Phase 2 完成: eth-heartbeat 合并到 main_loop
bb6ebe4 Phase 2 续: DI/DO 合并到 main_loop
57fb38c Phase 2 架构改造: AI/AO 合并到 main_loop
c819944 P0: 根本修复 pthread Stack canary + ENOMEM
8d02e9b P0+#8: heapless 0.9.3 + BLE 修复 + mesh 清理 + 引脚
```

## 已知问题

1. **eth-heartbeat stall**: 已合并但仍 stall, 排查中
2. **Modbus RTU master 无响应**: RS485 总线未接 slave 1, 属预期
3. **健康监控显示 9 tasks**: 含已合并任务 (仅 register, 未创建线程)

## 下一步

- 长稳测试 1 小时验证 7×24
- 健康监控显示清理
- modbus/shared.rs Vec → heapless::Vec (进一步减少 heap 分配)
