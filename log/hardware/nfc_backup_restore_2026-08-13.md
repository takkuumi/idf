# NFC 备份恢复与系统稳定性实机回归 (2026-08-13)

## 硬件

- ST25DV16KC: `IC_REF=0x26`, `MEM_SIZE=0x01FF`, `BLOCK=0x03`
- 原始快照范围: `0x0120..0x07FF`, 1760 bytes / 880 words

## Web 维护接口

- 未认证访问 `/getnfcstatus` 返回登录重定向。
- `GET /nfcbackup` 返回 HTTP 405；备份/恢复只允许认证 POST。
- `POST /nfcbackup` 后日志: `backup complete: 1760 bytes, CRC=0xE8DFA6C2`，状态保持 `BackedUp`。
- `POST /nfcrestore` 后日志: `restore complete: 880 words written to holding_buf + NVS queued`，状态保持 `Restored`。
- 工具复位后不重新备份，直接恢复仍成功，证明恢复不依赖 RAM CRC。

## OTA

- 最终应用镜像 1,666,720 bytes，占 2,359,296-byte OTA 槽 70.64%。
- Web 上传返回 `code=0`，启动分区为 `ota_0@0x260000`。
- 全部服务运行 30 秒后日志确认: `firmware confirmed valid`。
- 再次工具复位仍从 `ota_0@0x260000` 启动，未回滚。
- OTA 后 Web、四路 Modbus TCP、BLE 18 项读取再次通过。

## 栈与堆

60/120 秒真实水位样本:

| Task | Stack | Peak used |
|---|---:|---:|
| device-store | 16384 B | 14% |
| udp-mcast | 6144 B | 30% |
| nfc-st25 | 8192 B | 40% |
| http-srv | 12288 B | 23% |
| mb-rtu-master | 8192 B | 39% |
| mb-rtu-slave | 8192 B | 29% |
| main | 32768 B | 15% |

`free_heap` 约 2.0 MiB，`recovery_mode=Normal`；未出现 pthread 创建失败、stack canary、panic 或非计划重启。

## 限制

这是启动、维护操作和短期运行回归，不等同于 72 小时浸泡测试。UART1 未接真实 RTU 从站，周期超时日志符合当前接线条件。
