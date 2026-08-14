# Web UI 重组与保存隔离实机验证

日期: 2026-08-14

## 验证范围

- Web 导航由 6 个平级 Tab 重组为 4 个工作区，原有功能与接口全部保留。
- 可编辑输入框与固件版本、MAC、DHCP、运行状态、NFC 状态等只读值使用不同组件和样式。
- 网络参数与设备基本信息分别保存，旧客户端完整参数提交保持兼容。

## 构建与静态检查

- `cargo check`: 通过，0 warning。
- `cargo check --features f3`: 通过，0 warning。
- `cargo check --features f4`: 通过，0 warning。
- `cargo test --bin gateway --no-run`: 通过。
- JavaScript 语法检查: 通过。
- HTML 静态 ID: 54 个，无重复。
- 390px 浏览器设备仿真: 页面宽度 390px，无横向溢出；Tab 为 2x2 排列。

## 实机结果

- 完整烧录 factory 分区成功，应用大小 `1,688,640 / 2,359,296` bytes (71.57%)。
- Web 登录成功，设备实际页面包含 4 个工作区、只读值组件及两个独立保存动作。
- 仅提交设备基本信息: 返回 `code=0`，IP、掩码、网关和 DNS 未改变。
- 仅提交网络参数: 返回 `code=0`，设备基本信息未改变。
- 提交旧客户端完整参数: 返回 `code=0`，兼容路径正常。
- 验证前后配置保持一致:
  - IP: `192.168.51.140`
  - mask: `255.255.255.0`
  - gateway/DNS: `192.168.51.1`
  - SN: `ESP32-001`
  - addressinfo: `GW-ESP32`
  - BLE name: `Mesh001`
- 验证结束时 recovery mode 为 `Normal`，严重故障计数为 0，Web 与 TCP 端口 502/503/504/5002 均可连接。

## 说明

网络参数和 RS485 端口参数仍按固件现有机制在重启后应用，Web 成功提示已明确说明该生效时机。

## 保存并重启增量验证

- 网络参数和 RS485 端口配置均增加“保存并重启”操作，普通保存操作保留。
- 重启请求由 DeviceActor 在 `SystemConfig` 成功写入 NVS 后发出；写入失败时取消重启并保留重试标志。
- `cargo check`、`cargo test --bin gateway --no-run`、JavaScript 语法和 `git diff --check` 通过。
- 使用 `--no-skip` 完整烧录 factory 分区，应用大小 `1,690,544 / 2,359,296` bytes (71.65%)。
- 使用原网络参数执行“保存网络参数并重启”：串口日志顺序为 NVS 落盘成功、请求复位、`RTC_SW_CPU_RST`、从 NVS 恢复相同参数。
- 使用原 RS485 参数执行“保存端口配置并重启”：3 个端口均返回 `code=0`，复位后读回值逐项一致。
- 两次复位后 Web 恢复正常，TCP `502/503/504/5002` 均可连接，recovery mode 为 `Normal`，严重故障计数为 0。
