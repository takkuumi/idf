# 架构优先级改进与验收（2026-11-01）

版本保持 2.4.0，DEFAULT=MCP23017、F3/F4=PCA9555，寄存器地址和持久化格式不变。

## 已实施

### 高优先级：输入时效与输出确认

- PCA9555 DI 以采样序号、时间戳和失败计数发布；最近一次总线读取失败或超过 250ms 时返回错误；未就绪/短暂发布争用返回待采样，不伪报硬件故障。主循环保持最后状态，但不推进去抖；相同采样序号不重复计数。Web 保留旧数值并明确显示质量。
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

实机结果见下节。主机 NACK 测试只证明对应事务 helper 的恢复行为，不代表真机拔板验收。

## F3 实机验收（最终源码 928fe474ff29）

- 使用完整 espflash --no-skip 命令刷入 factory，保留业务 NVS/holding；随后单连接 esptool verify_flash 核对 bootloader、partition table、已擦除 otadata 与 app，四段 digest 全部匹配。
- App 1,772,608 bytes，占 2,359,296-byte OTA 槽约 75.13%，余量 586,688 bytes；候选产物位于 release/candidates/v2.4.0-priority-20261101-928fe47。此前生产发布目录未覆盖。
- 主循环启动由上一镜像约 10.011s 提前到 1.931s，Web 监听约 1.800s，四个 TCP listener 1.865..1.901s 就绪。物理网络 IP 事件仍约 3.404s，未宣称 PHY 协商变快；事件分发延迟约 1ms。
- 首次主机连接启动探测报 Host is down，保留失败日志；网络恢复后才重新开始稳定窗口计时，未把该失败删掉或混作通过。
- 稳定窗口 180.021s：33,797 条 Modbus 请求全部成功，约 187.74 req/s；34 次认证 Web 检查无错误、会话替换 0。四端口各 9 项协议边界/分片/流水检查、12 项 Web 非法参数拒绝通过。
- 端口 502：9,673 请求，P95 26.29ms，P99 31.14ms，max 157.50ms。
- 端口 503：7,180 请求，P95 31.53ms，P99 33.89ms，max 169.45ms。
- 端口 504：7,124 请求，P95 31.74ms，P99 34.46ms，max 165.24ms。
- 端口 5002：9,820 请求，P95 25.94ms，P99 30.61ms，max 148.85ms。
- 同步认证 Web 样本中 DI 有效、DO 读回有效，读失败/队列满/LED 错误均为 0；确认代号与目标代号一致，读回 0xD011。网络、三个串口、校准端点和当前输出与刷前快照逐项一致；未主动写 Q 点。
- 复位计数在稳定窗口保持 141。RCU 观察到最大回收等待 config=10us、storage=16us；这次只读回归不是并发配置写入极限测试。
- 满负载主循环工作 P95/P99 桶上界达到 7.5ms；分钟日志中有 deadline miss 及约 35.8ms 最大工作耗时，不承诺 5ms 硬实时或所有网络响应低于 50ms。
- 启动后 603s 的生产栈检查：8 个注册任务，low=0；HTTP 剩余栈最小 1,188B、最大使用率 88%。这是本次路径的观测水位，不能代表所有异常路径的余量。后续轻负载窗口主循环工作 P95/P99 桶上界为 2ms/4ms，最大偶发迭代 43.017ms。
- 后续独立诊断窗口 300.005s，60 个样本零错误、零会话替换，复位计数保持 141；DI 龄期最大 11ms、DO 寄存器读回龄期最大 106ms。
- 用户随后要求真实 DO 切换及连续压测，首轮发现队列争用误进 100ms 硬件退避，继续修复并验收，详见 `do-response-stress-2026-11-01.md`。本节数据属于该修复之前的候选，不混作新镜像成绩。
- 最终修复候选 `a988c46ae8f7` 的独占 DO 连续窗口已完成：600.025s、4,160 笔确认、92,878 次后台并发读均无错误，45 次争用重试成功；输出按当时重新备份的 `0xFE07` 恢复。该窗口分位数和 66.738ms 主循环工作峰值见 DO 专项报告，不沿用旧候选 35.8ms/43.017ms 数据作为最新峰值。

### 保留的中间问题

- 首个候选 781b11e 把短暂 DI 发布争用当故障，实机出现误报警；最终版改为待采样返回，真实读失败才报错。
- 首个候选 120 秒 Modbus 22,481/22,481 通过，但 Web 出现 19 次 JSON 解析错误；期间输出目标发生变化，新登录后读取正常。缺少原错误响应类型，不能把全部错误绝对归因于现场登录。最终工具增加响应类型记录及显式会话替换选项，最终复测两类错误均为 0。
- 构建中遇到 Xtensa fetch_max 代码生成缺陷，已改为现有单写者/串行写者保护下的 atomic load/max/store；未修改外部 SDK、旧固件或手持机项目。

## 测试证据与边界

- 专项 host 实际执行 28 项通过（包含配置布局回归），原有核心 host 25 项通过；实际 F3/F4 事务 helper 故障测试共 4 项通过；限定抢占 Loom 模型通过。
- DEFAULT/F3/F4 Clippy 零 warning，F3 设备测试目标编译及 Release 构建通过，JavaScriptCore 语法检查通过。设备测试目标未在 ESP32 上执行，不冒称全部设备单测运行。
- UART：log/hardware/architecture-priority-final-uart-2026-11-01.log。
- 压力/诊断：architecture-priority-final-acceptance-rerun-180s-2026-11-01.jsonl、architecture-priority-diagnostics-60s-2026-11-01.jsonl、architecture-priority-final-diagnostics-300s-2026-11-01.jsonl。
- 四段校验：log/hardware/architecture-priority-final-verify-2026-11-01.log；汇总：architecture-priority-final-summary-2026-11-01.json。
- 物理拔板/NACK 注入、触点负载反馈、精确脉宽、F4 真机、多协议持续写入与写 Flash 中掉电未完成；主机模拟不替代这些验收。
- 这次镜像未完成 8h/72h 长测；先前 6h7m 记录属于旧镜像，不能合并作为本次成绩。
