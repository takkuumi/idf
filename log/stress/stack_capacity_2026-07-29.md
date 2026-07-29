# 栈容量与长稳验证记录（2026-07-29）

## 静态验证

- 硬件：ESP32-S3R2，512KB 物理 SRAM，2MB Quad PSRAM。
- 生效配置：main 32768B，BTC 8192B，BTU 8192B，pthread default 8192B，
  LwIP 8192B，event 4096B，timer 4096B，internal reserve 65536B。
- linker map：DRAM `0x53700`；data `0x685d`；bss `0x6450`；bss-end 至段末
  `0x2c050`（176.08KB）。
- 默认用户任务固定栈：106KB；所有可选用户任务同时启用：120KB。
- Modbus TCP 连接新增任务栈：0B（8 连接共用一个 16KB 任务）。
- 编译：`cargo build` 通过；`cargo check` 0 warning。

## 真机验收条件

以下项目尚需烧录后的 72 小时连续实测，未执行前不得宣称 7x24 已证明：

1. metuory 1.0.78 持续连接，循环读写 SN/IP/MAC/BLE ID/RS485 配置。
2. 四端口合计 8 个 Modbus TCP 客户端，包含分片、流水请求、慢读和断线重连。
3. RTU master/slave 同时收发，Web 查询与至少一次完整 OTA 流式上传。
4. NFC/UDP/AI/AO/DI/DO 全开；每 60 秒保存 `[stack]` 和 `[mem]` 日志。
5. 每个用户任务 `min_free >= 1024B` 且 `used < 90%`；无 Stack canary、ENOMEM、
   Guru Meditation、非用户触发重启，`min_heap` 不持续单调下降。

## 结果

待真机执行。当前工作只完成静态容量、编译和架构验证，不能替代硬件长稳结论。
