# 当前生产待办

更新：2026-10-30。问题与证据见 [本轮生产核查](testing/production-audit-2026-10-30.md)，
执行步骤见 [生产验收清单](verification.md)。

- [ ] 同一最终 SHA 连续 72 小时浸泡；被暂停或中途刷机的记录不算通过。
- [ ] 真实手持机 BLE 网络/组态写入、重连及断电复读。
- [ ] 物理断电和 Flash 写入中断恢复；软件重启不能替代。
- [ ] RS485 额外端口、慢帧、错 CRC、离线恢复和总线冲突。
- [ ] AI 电流源、AO 测量、DI/DO 实际负载；按默认/F3/F4 实物分别验收。
- [ ] NFC 标签备份、恢复、掉电与冲突。
- [ ] 双光口独立通路、双口转发和规定环网模式下的故障切换。
- [ ] OTA 正向升级、失败回滚、传输和供电中断。
- [ ] 固定版本、硬件变体和干净提交后生成正式生产清单及 SHA。

## 当前不承诺的能力

LoRa 芯片/LoRaWAN、RS485 Gateway/透明转发端点、Wi-Fi 自动以太网故障切换、
BLE Mesh 业务及 MCU 双光口冗余协议，不属于当前默认固件已完成能力。
RS485 mode 2/3 走从站兼容路径，第三 UART 默认保留控制台。

参考 [光纤调查](hardware/fiber-analysis.md)、[LoRa 调查](hardware/lora-analysis.md)。
旧 TODO 已完整归档到 [历史记录](archive/reports/todo-history-pre-production-2026-10-30.md)，
其中旧 API、地址和硬件描述不作为当前产品说明。
