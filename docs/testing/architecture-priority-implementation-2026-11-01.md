# 架构优先级改进与验收（2026-11-01）

版本保持 2.4.0，DEFAULT=MCP23017、F3/F4=PCA9555，寄存器地址和持久化格式不变。

## 已实施

### 高优先级：输入时效与输出确认

- PCA9555 DI 以采样序号、时间戳和失败计数发布；未初始化、最近一次总线读取失败、超过 250ms 或短暂发布争用时返回错误。主循环保持最后状态，但不推进去抖；相同采样序号不重复计数。Web 保留旧数值并明确显示质量。
- DI 读取发布与指示灯镜像分离，灯 NACK 不阻止有效输入发布。
- DO 使用 32 项固定 FIFO，保留主循环已提交的边沿；满/争用时显式拒绝，主循环按原 100ms 重试。提交成功不代表执行成功。
- worker 对实际 OUTPUT 寄存器写后读回匹配，随后更新确认代号；空闲时每 100ms 复读并在不一致时重写核验。寄存器确认不等于继电器触点/负载反馈。
- 故障期间取消被新目标取代的旧边沿，仅恢复最新目标，避免恢复后重放过期脉冲；取消数量可查询。正常状态 FIFO 每次循环消费一个边沿，但不是硬实时脉冲发生器；短于主循环提交周期或 worker 周期的脉宽不作保证。
- Web 显示采样失效、输出待执行、寄存器读回失效/已确认；控制 toast 明确表示指令接收。

### 高优先级：并发原语

- RCU 用 SeqCst 指针/代号/读者登记建立统一顺序，写者串行发布；双代读者计数避免新读者拖延旧代回收。登记仅覆盖 Arc 获取，长业务闭包或外部 Arc 不占回收等待。
- 回收时协作休眠并记录最大等待；暂停的旧代读者仍可延迟写者，不承诺有限最坏等待。
- AtomicBits64 序号扩大为 u32，原子字段使用共同 SeqCst 顺序；新增有界 try_load_bits，用于 IO 缓存读取，不能再把它标为 wait-free。
- 修复新增诊断使用 fetch_max 时触发 Xtensa LLVM 1.90 undefined branch label 的构建问题：统计已有单写者/写者互斥，改用原子 load/max/store，不修改外部 SDK。

### 中优先级：启动校准及遥测

- 8 秒校准改为主循环状态机，每次至多一个到期样本；启动网络服务不等待时窗结束。
- 现场校准值在时窗中被改写时不覆盖，保留既有端点有效范围和 holding 保存规则。
- 生产构建也采集十个模块及主循环固定容量耗时直方图，60 秒 tick 窗口发布；P95/P99 是桶上界，非精确分位数。主循环耗时包含低频日志开销。
- GET /getiodata 的 data.diagnostics 返回 IO、calibration_status、performance、rcu_reclaim_max_us；沿用认证，不新增匿名信息接口。performance.phases 顺序为 tcp、ai_ao、di、do、control、eth、ble、events、housekeeping、calibration。null 表示尚无可用诊断快照。
- calibration_status：0 未启动、1 采样中、2 已完成、3 无需校准或没有有效端点、4 现场参数修改取消。

## 主机和构建验证

- tests/host/run_architecture_priority.py：实际生产模块/片段测试，覆盖队列满/边沿/故障恢复、时效回卷、暂停写者、32 位代号、RCU 多写者及旧快照寿命、非阻塞时窗、分位桶。
- tests/host/run_io_transactions.py：实际 F3/F4 IO transaction helper，在可注入 NACK/寄存器复位的主机总线下执行读回和灯故障隔离。
- tests/host/run_rcu_model.py：Loom 0.7.2，读者获取与两次发布的限定抢占模型（preemption_bound=2）；只替换同步原语/const 构造/等待适配。不是无限状态或所有弱内存模型的形式证明。
- 原有 tests/host/run_production_core.py 回归。

实机数据在完成后补充；没有物理故障注入时不得把主机 NACK 测试标成真机拔板验收。
