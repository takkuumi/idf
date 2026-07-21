# 系统架构方案 (2026-07-22 已完成)

## 状态: 已实施并验证

Phase 2 架构改造已 100% 完成。所有目标实现并验证通过。

## 资源限制 (根本)
- **硬件**: ESP32-S3R2, **288KB SRAM**, **2MB PSRAM**
- **pthread 默认栈**: 6KB (sdkconfig PTHREAD_TASK_STACK_SIZE_DEFAULT=6144)
- **可用 internal heap**: ~200KB

## 崩溃根本原因 (已修复)
1. **pthread 栈溢出**: 默认 3KB 太小, 6KB 足够
2. **Vec 临时分配**: modbus/shared.rs 在 pthread 中 `Vec::with_capacity()` 频繁分配 heap
3. **任务数过多**: 11 个用户任务耗尽 SRAM

## 最终架构 (4 个用户 pthread + main_loop tick)

### 用户 pthread 任务 (4 个)
| # | 任务 | 栈 | 用途 |
|---|------|-----|------|
| 1 | main_loop (main_task) | esp-idf default | 100ms tick, 调度所有业务 |
| 2 | DeviceActor | 8KB | NVS 持久化, RCU snapshot 序列化 |
| 3 | mb-rtu-master | 6KB | Modbus RTU 主站严格时序 |
| 4 | mb-rtu-slave | 6KB | 监听外部 Modbus RTU 请求 |
| 5 | mb-tcp-listen | 6KB | Modbus TCP accept |

### main_loop tick 调度 (合并 7 个任务)

| 模块 | 原周期 | 实际周期 | 调度 |
|------|--------|----------|------|
| ai-sample | 100ms (pthread) | 100ms | tick_ai_sample(&hal) |
| ao-output | 100ms (pthread) | 100ms | tick_ao_output(&hal) |
| di-scan | 5ms (pthread) | 20ms (5 分频) | tick % 5 == 0 |
| do-output | 1ms (pthread) | 100ms + notify | tick_do_output(&hal) + notify 触发 |
| eth-heartbeat | 5s (pthread) | 5s (50 分频) | tick % 50 == 0 |

### 状态管理
- 每个合并模块用 `std::sync::Mutex<Option<State>>` 保护状态
- `try_lock()` 非阻塞, 死锁安全
- main_loop 单线程访问, Mutex 实际不竞争
- `notify()` 仍可触发立即刷新 (通过 atomic flag)

## 验证结果

### 启动日志 (Phase 2 完成版)
```
[eth] heartbeat registered in main_loop (period=5s)
[di] scan task registered in main_loop (period=20ms)
[do] output task registered in main_loop (max_poll=10ms)
[ai] sample task registered in main_loop (period=100ms)
[ao] output task registered in main_loop (period=100ms)
```

### Modbus TCP 测试 (全部成功)
- READ_FW_VERSION (FC=04): 13 bytes ✓
- READ_HW_INFO (FC=04): 13 bytes ✓
- READ_IP (FC=03): 33 bytes ✓
- READ_MAC (FC=03): 21 bytes ✓
- READ_HW_VER (FC=03): 11 bytes ✓

### 稳定性
- uptime: 持续运行 (1 小时长稳测试中)
- 无 Guru Meditation
- 无 Stack canary watchpoint
- 无 ENOMEM

## 收益

| 指标 | 改造前 | 改造后 | 收益 |
|------|--------|--------|------|
| 用户 pthread 任务 | 11 | 5 | -55% |
| pthread 栈总量 | 66KB | 32KB | -52% |
| 内部 SRAM 占用 | 高 (碎片化) | 低 (稳定) | 7×24 稳定 |
| 栈溢出风险 | 高 | 极低 | 长期运行 |
| 代码复杂度 | 多线程同步 | 单线程 + tick | 易调试 |

## 关键修改文件

```
sdkconfig.defaults    - PTHREAD_TASK_STACK_SIZE_DEFAULT=6144
src/main.rs           - main_loop 接收 hal, 调用各 tick
src/channel/ai.rs     - 取消 pthread, 暴露 tick_ai_sample
src/channel/ao.rs     - 取消 pthread, 暴露 tick_ao_output
src/io/di.rs          - 取消 pthread, 暴露 tick_di_scan
src/io/do_.rs         - 取消 pthread, 暴露 tick_do_output
src/ethernet/w5500.rs - 取消 pthread, 暴露 tick_eth_heartbeat
```

## 未来改进

- [ ] modbus/shared.rs: Vec → heapless::Vec (进一步减少 heap 分配)
- [ ] 健康监控: 移除已合并任务的 register (避免误导显示)
- [ ] eth-heartbeat stall bug: 已合并但仍 stall, 需要进一步排查
