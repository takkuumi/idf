# NVS 落盘性能优化核查（2026-10-31）

## 结论

本轮对当前 ESP-IDF v5.5.4、esp-idf-svc 0.53.0 的真实调用链做了整体检查。
运行时采样数据和 RS485 读取结果不写 NVS；holding 仍使用独立 raw A/B 槽，
因此优化重点是系统配置、协议 blob、设备文本、Web 元信息、DO 位图和设备配置表
进入 NVS 的公共写入路径。

发现 `esp-idf-svc 0.53.0::EspNvs::set_blob` 会先调用 `nvs_erase_key`，再调用
`nvs_set_blob` 和 `nvs_commit`。ESP-IDF v5.5.4 的 `nvs_set_blob` 本身已经执行
同 key 的版本替换、旧值失效和相同内容去重；预先删除会增加一次不必要的 NVS
删除操作和掉电中间窗口。新增 `src/device/nvs_store.rs`，在保留现有 NVS 互斥锁、
错误返回和逐 key commit 边界的前提下，统一调用 IDF 原生 `nvs_set_blob`。

## 影响范围

- SystemConfig blob：不再删除 `sys_cfg` 后重写。
- 协议 A/B blob：不再删除 inactive blob 后重写，仍先写 inactive，再切换 active。
- 设备文本 blob：保留写前内容比较、写后读回校验和 magic。
- Web system info blob：保留写后读回校验。
- DO 位图 blob：保留 magic 和失败重试。
- `device_config` 及其它统一走 `device::nvs_write` 的 blob：自动使用同一优化层。

没有改变：NVS 分区布局、key 名、blob 格式、A/B 选择、CRC、回读、dirty 语义、
保存节流、holding raw A/B 写入、重启前持久化屏障、NVS 失败后的重试策略。

## 为什么不做更激进的改动

- `nvs_commit` 在本地 IDF v5.5.4 的 simple handle 实现中当前是 no-op，但公共 API
  要求仍保留 commit；删除 commit 会把代码绑定到实现细节，升级 SDK 后可能造成数据未落盘。
- 不把多个业务 key 合并成一个新 blob：会改变现场格式、迁移策略和掉电恢复边界。
- 不缩短自动保存静默窗口，不提高 DO/holding 写频率；这些会扩大 Flash 磨损并改变组态多帧写语义。
- 不关闭 holding CRC/回读，不改变 raw A/B 槽的擦除顺序。

## 验证

- 新增 `tests/host/run_nvs_store.py`：用带调用追踪的 host harness 验证真实适配层，
  包括 set→commit 顺序、空/NUL/超长 key 拒绝、set 失败不 commit、commit 失败保留 dirty、
  重试和并发新 dirty 保留；共 10 项测试全部通过（含 5 项适配层场景及 key/pending_save 单测）。
- 新增 `tests/host/run_nvs_engine.py`：编译并运行当前 ESP-IDF NVS page/blob/GC 实现，
  使用 24KB（与生产 NVS 相同大小）的内存 NOR 仿真。50 轮（每轮 2 个 blob，
  共 100 次相同值保存）中，旧 delete-before-set 路径产生 1,084 次分区写调用、
  6 次 sector 擦除；原生替换路径为 0 次写、0 次擦除。200 轮混合 7 类记录后重载通过，
  对 7 类记录枚举的 113 个实际写/擦除操作中断点全部恢复为完整旧值或新值，且其它记录
  不变；填满分区后更新失败仍保留旧值，重新初始化后复读通过。该仿真不覆盖芯片物理时序、
  单次 program 内部断电或真实硬件电源故障。
- 新增的现场基线脚本在优化前设备上完成 12 次相同值保存和 6 次变化值保存：相同值中位数
  106.6ms、最大 152.7ms；变化值中位数 102.4ms、最大 108.8ms。测试结束后原始系统信息
  已恢复并复读，期间 reset_count 保持 76、Recovery 为 Normal。此数据包含 HTTP/Actor
  调度，不能直接当作物理 Flash 编程时间。
- `tests/host/run_production_core.py`：25/25 通过。
- F3 `cargo clippy --locked --bin gateway --features f3 -- -D warnings`：通过。
- 默认/F4 `cargo clippy --locked --bin gateway [--features f4] -- -D warnings`：通过。
- `cargo test --locked --bin gateway --no-run`：通过。
- `cargo fmt --all -- --check`、`git diff --check`：通过。

优化后的 F3 Release 已完成构建；设备刷入和现场重复基准应单独记录。上述 NVS engine
结果是当前 IDF 实现的可重复仿真证据，现场脚本测量的是端到端 Web 确认时延。任何对比
都必须保持同一设备、字段、网络负载和恢复校验条件。

## 现场结果与边界

- 已全量刷入当前 F3 设备（串口 `/dev/cu.usbserial-1430`），保留 `nvs`/`holding`。
  固件版本仍为 2.3.0；这是工作区优化验证镜像，原 `release/v2.3.0` 发布包未覆盖。
  app 为 1,694,624 字节，SHA-256：
  `0fbf37f268ff3e86349bd9828753b8bc11f4f723a26bed84b11ca343e32fcc90`。
  UART 的 ELF SHA 前缀 `f19e8ee47` 与此镜像描述匹配，运行 factory `0x20000`。
- 升级后网络、串口、系统信息、传感器、BLE 配置与刷前相同；已有组态前 32 字复读一致。
- 同一脚本在新镜像上的 12 次相同值保存：中位数 95.35ms，最大 103.83ms；
  6 次变化值：中位数 99.94ms，最大 158.56ms。相同值基线中位数为 106.60ms；
  样本较少且包含调度/网络，变化值尾延迟没有证明改善，不能宣称所有场景都加速。
- 原有参数均在 finally 路径恢复并逐项复读；两次计划 Web 重启分别确认测试显示名和
  恢复后的原参数能持久化。复位计数 77→78→79，UART 均为 `planned reset requested by web`。
- 45 秒独立验收：四端口 Modbus 共 1,640/1,640 请求成功，Web 9 次、0 错误；
  Recovery Normal，故障计数全零，堆低水位 2,086,264 字节。
- 后续 120 秒高频测试因用户同时操作而中止，不能列为通过。该窗口出现 Web JSON 解析错误；
  当前 Web 为单管理员会话，测试与人工登录会相互替换 token，缺少逐个失败 HTTP 响应时
  不能将所有错误归于同一根因。已停止所有设备测试、登录和重启。
- 用户报告 SOFTWARE 后核对仍为 reset_count=79；当前记录对应上述验收主动重启，
  未确认 Q 点操作引发新的整机复位。事件证据见
  `log/hardware/nvs-optimization-incident-2026-10-31.md`。
- RS485-1 从站 5 无响应为刷前已有观察项。本轮未完成真实断电、全端口外设或 72h 验收。

## 重放命令

```bash
python3 tests/host/run_nvs_store.py
python3 tests/host/run_nvs_engine.py --idf-path /Users/takumi/Workspace/esp-idf
python3 tests/host/run_production_core.py
```

现场脚本见 `tests/manual/nvs_save_benchmark.py`。必须独占台架，脚本登录会影响人工会话；
`--reboot-check` 会主动进行两次整机重启，不得与现场 Q 点操作并发。
