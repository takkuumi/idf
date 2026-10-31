# NVS 优化验收期间 SOFTWARE 复位说明（2026-10-31）

## 根因

本轮现场验证脚本 `tests/manual/nvs_save_benchmark.py --reboot-check` 为验证
“保存后重启仍能恢复”和“恢复原值后再次重启仍能恢复”，主动调用了两次 Web `/reboot`。
设备日志对应记录：

```text
[main] planned reset requested by web
[main] reset reason=3 (SOFTWARE)
```

因此 reset count 从 77 增至 78、再增至 79，均对应测试主动发起的计划复位。此窗口
未记录 PANIC、WDT、Stack canary 或 Recovery fault；不能由此推导所有 Q 点场景都无缺陷。

## 时间关联

- `nvs-save-optimized-reboot.jsonl` 修改时间约 15:38:40。
- UART 记录的两次 Web 计划复位位于同一验收窗口，随后设备分别以
  `reset reason=3 (SOFTWARE)` 启动。
- 15:41:02 至 15:41:17 的状态采样仍为 reset_count=79，uptime 持续增长；15:42:57
  再次读取仍为 79、uptime=293，没有确认发生新的整机复位。
- 15:41:22 开始的 `Expecting value` 出现在这两次复位之后，不能归为重启窗口。
  代码另确认 Web 只有一个全局会话，每次新登录会覆盖上一会话；API 未认证时返回登录页，
  前端直接 `response.json()` 会解析失败。自动测试登录会使人工浏览器失效，人工重新登录
  也会使测试会话失效。这提供了页面失联的另一条已确认机制，但未保留每个失败响应，
  不能将全部解析错误断言为同一根因。

## Q 点判断

普通 Q 点路径只调用 `write_coil` 更新 DO 原子位并通知输出任务；重启地址
`COIL_RESTART` / `COIL_LOGIC_RESTART` 才会请求 `ResetSource::MODBUS`。本次 UART
明确记录的是 `ResetSource::WEB`，没有 Q 点触发这两次复位的证据。

验收脚本现已停止；设备当前 `reset_count=79`、`recovery_mode=Normal`、以太网在线、
故障计数为 0。后续不带 `--reboot-check` 的 NVS 基准不会主动重启设备，但登录仍会
覆盖人工浏览器会话；因此此类验收必须在独占台架窗口执行，不与用户 Q 点操作并发。
