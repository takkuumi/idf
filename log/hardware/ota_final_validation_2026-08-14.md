# OTA 最终实机闭环记录

日期：2026-08-14

## 固件与上传

- Git 提交：`c04381f`
- OTA 镜像：由默认全功能 `gateway` ELF 使用 `espflash save-image` 生成
- Flash 参数：ESP32-S3 / DIO / 40 MHz / 8 MB
- 镜像大小：`1,689,664` bytes，未超过 OTA 槽 `0x240000`（2,359,296 bytes）
- SHA-256：`efe6c9b4eb7ced5bfd2f984d1acaedf103825ae8388f53a4acdd56448fe4dc82`

## 结果

1. Web 登录成功，上传 `POST /updateota` 返回 HTTP 200、`code=0`。
2. 设备自动重启并重新获取静态 IP `192.168.51.221`；重启后 `uptime=99s`、
   `recovery_mode=Normal`、`reset_count` 仅增加 1，符合本次 OTA 计划重启。
3. 重启后 `/getsysteminfo`、`/getnetworkconfig`、`/getportconfig`、
   `/getiodata` 全部返回 `code=0`，配置未丢失。
4. TCP 502/503/504/5002 均恢复监听；从 `0x0880` 读取标准最大 125 个保持
   寄存器，四路均返回 259 字节、功能码 `0x03`、字节数 `250`。
5. 四路均接收 260 字节最大 Modbus TCP ADU，并返回完整异常响应，未截断、
   超时或挂起。

本记录验证了当前 A/B OTA 上传、切换、重启、重新联网和业务端口恢复路径。
72 小时连续浸泡、真实 USB-RS485 从站闭环仍属于交付现场的长期验收项目。
