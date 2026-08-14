# AI 首轮采样 BREAK 修复真机记录

日期：2026-08-14

## 故障

完整或裁剪固件进入主循环后，在静态 IP 事件之后稳定触发：

```text
Guru Meditation Error: Core 0 panic'ed (Unhandled debug exception)
Debug exception reason: BREAK instr
```

core dump 中 `tick=20`，对应首次 100 ms AI 采样。`map_range` 按参考固件
使用反向区间，却把 `4095, 0` 直接作为 `i64::clamp` 下界和上界，违反 Rust
API 契约。

## 修复

- 保持 `map(avg, cal_max, cal_min, 4095, 0)` 业务方向。
- 使用 `min(out_min, out_max)` 和 `max(out_min, out_max)` 做最终夹紧。
- 回归覆盖反向输入、反向输出、上下越界和除零。
- 删除硬编码地址诊断和 W5500 自定义 DMA 不可达路径。

## 完整固件实测

```text
[adc] Continuous+DMA mode started: 6 ch, 1kHz total, Type2 12-bit
[ble_at] BLE advertising active
[mb-tcp] 4 ports, max 8 clients, main-loop polling
[main] uptime=180s heap=2053KB min=2047KB internal=43KB internal_min=41KB
[stack] tasks=7 low=0 min_free=4308B max_used=56% max_task=http-srv
```

在线协议结果：

- TCP 502/503/504/5002 FC03 均成功。
- FC03 125 寄存器返回 259 B。
- 260 B 最大 ADU 请求返回标准异常响应，未截断或挂起。
- Web 登录和系统、网络、端口、IO、BLE、传感器、NFC 状态接口成功。

短时结果不能替代 72 小时浸泡和真实外设闭环。
