# 生产验收清单

更新：2026-10-30。实际结果见 [本轮生产核查](testing/production-audit-2026-10-30.md)。
清单不是通过证明；设备测试的 `--no-run` 仅编译，短时联网不替代连续长测。

## 构建与产物

- [ ] fmt、diff 检查通过，Clippy `-D warnings` 通过。
- [ ] `python3 tests/host/run_production_core.py` 实际执行核心主机测试。
- [ ] `cargo test --locked --bin gateway --no-run` 编译通过；禁止烧入测试 ELF。
- [ ] 默认、F3、F4、TCP-only 分别编译，按实物选择变体。
- [ ] Release 构建成功，记录 ELF/app SHA-256；app 小于 `0x240000`。
- [ ] 正式发布固定版本、干净提交和发布清单；候选产物不冒充正式发布。

## 启动与数据语义

- [ ] 带 `--no-skip`、显式 bootloader/partitions/factory 完整烧录，保留 NVS/holding。
- [ ] 核对启动、配置 A/B 加载、BLE 广播、HTTP/Modbus 监听、reset reason/count。
- [ ] 保留 W5500 PHY/IP 事件时间，另测业务可达时间；不把 PHY 速率解释为光口速率。

| 数据 | 实际协议地址 | 注意事项 |
| --- | --- | --- |
| 版本/日期 | FC04 `0x087E/0x087F` | 应与候选镜像一致 |
| IP/掩码/网关 | FC03 `2247..2258` | 12 octet，IP → 掩码 → 网关 |
| AI | FC04 `0x0080` 起 | 默认/F3 报告 4 路；数值不是 mA 工程量 |
| AI 状态 | FC04 `0x0088` 起 | 状态位，不能作 AI 数值 |
| 组态 | FC03 `2300+` | 按实际变长表及提交闸门语义操作 |
| RS485 结果 | FC04，组态指定的目标地址 | RAM 实时值；FC03 低地址是控制字 |
| UDP 状态 | FC04 `0x0090..0x009F` | LE 镜像；读取历史镜像须结合接收新鲜度 |

不得凭通用 3xxxx/4xxxx 惯例自行改偏移；本工程有实际数值为 30001/40001 的兼容别名。
旧清单中 `0x0103=重启`、`FC03 0=AO`、8DI/8DO 和 BLE Mesh 等描述已过期。

## 参数保存

- [ ] Web 保存成功有 DeviceActor 完成确认；忙、队列满、失败、超时返回失败。
- [ ] 缺失/非法参数不能误改 DO0、AI0 或清空 BLE 名称。
- [ ] 网络、端口、BLE 名称、AI 校准、系统信息、holding、设备文本分别复读。
- [ ] 保存后立即软件重启再复读；再独立做真实断电试验。
- [ ] 保存失败及并发新写保留 dirty；运行时 AI/RS485 采样不写 Flash。
- [ ] 测试参数有备份并最终恢复，不能把测试标记带入交付。

台架测试会改少量参数及原始字，禁止直接用于已接生产控制回路的设备：

```bash
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/configuration_acceptance.py \
  192.168.51.226 --apply --backup log/hardware/config-original.json
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/test_udp_reconfiguration.py \
  192.168.51.226 --apply
```

## 通信与物理功能

- [ ] 四端口 FC03/04、125-word 响应、分片、流水、非法 PDU、Web 参数和认证检查通过。
- [ ] 真实 RS485 数据持续写入配置目标；慢帧、错 CRC、离线、多端口分别验证。
- [ ] UDP 被过滤流量持续到达时仍可热重绑，停包后新鲜度失效。
- [ ] 真实手持机 BLE 读写、重连、组态和网络断电恢复通过。
- [ ] NFC 实际标签、DI/DO 负载、AI 标准源/AO 测量、双光口和环网分别验收。
- [ ] HTTP 慢滴送、TCP 半帧/背压、断线恢复不饿死其它任务。

```bash
GW_STABILITY_WEB_PASSWORD='<当前密码>' python3 tests/manual/production_acceptance.py \
  192.168.51.226 --duration 600 --expected-version 0x00E4
```

## 7×24 放行门槛

- [ ] 同一 SHA 连续 72 小时 Web/Modbus/真实负载和串口记录；刷机或复位后重新计时。
- [ ] 无非计划重启、panic、WDT、低栈、堆损坏或持续内存下降。
- [ ] 记录请求量、错误、P95/P99/最长延迟、内部 SRAM、任务最小栈和主循环 miss。
- [ ] 完成物理断电、Flash 写入中断、从站离线、OTA 升级/回滚和传输中断测试。
- [ ] 每项承诺交付功能都有实测证据；缺少硬件的测试不得标为通过。

放行结论必须限定镜像、硬件、配置、负载和测试时长，不承诺“100% 永不故障”。
