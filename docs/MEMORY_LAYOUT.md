# IDF 固件整体内存布局

> 本文描述当前 ESP32-S3 工业网关固件的 Flash、内部 SRAM、PSRAM、任务栈、运行时
> 快照和协议地址空间。地址和容量以仓库中的 `partitions.csv`、`sdkconfig.defaults`、
> `src/config.rs` 及相关 Rust 常量为准。修改这些定义时，必须同步更新本文并重新执行
> 构建检查。

## 1. 物理资源与边界

| 资源 | 容量 | 用途与限制 |
| --- | ---: | --- |
| ESP32-S3R2 内部 SRAM | 512 KiB | CPU/中断/DMA/网络控制块/任务栈/小对象；这是物理总量，不等于启动后的可用 heap |
| Quad PSRAM | 2 MiB | 大对象、协议/文本工作区、RCU 大数组和网络动态对象；80 MHz、malloc 优先使用 |
| SPI Flash | 8 MiB | bootloader、分区表、NVS、holding、三套应用槽、coredump、BLE Mesh NVS、FAT storage |
| Flash 擦除块 | 通常 4 KiB | 所有持久化写入必须遵守分区边界和相应的掉电保护策略 |

### 1.1 内存分配策略

`sdkconfig.defaults` 固定了以下策略：

- `CONFIG_SPIRAM_SIZE=2097152`，项目构建约束要求板卡使用 2 MiB PSRAM；ESP-IDF 5.5
  若提示该符号为 unknown，不能把 defaults 文件当成运行时探测结果，必须以启动时
  `esp_psram_get_size()` 和 `heap_caps_get_free_size(MALLOC_CAP_SPIRAM)` 为准。
- `CONFIG_SPIRAM_USE_MALLOC=y`，普通大块 malloc 可使用 PSRAM。
- `CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=4096`：小于 4 KiB 的分配优先放内部 SRAM；
  大于该阈值的对象优先放 PSRAM。
- `CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=65536`：始终为 DMA、中断和关键内部对象
  保留 64 KiB 内部 SRAM。
- pthread 栈显式使用 `MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT`，不会因为启用 PSRAM
  自动迁移到片外内存。
- W5500 DMA 缓冲、串口 DMA 和中断相关对象必须留在内部 SRAM；协议大缓冲不得放入
  这些实时路径的栈。

内部 SRAM 的实际可用量会被 ESP-IDF 系统任务、静态 `.data/.bss`、heap 元数据、
DMA 保留区和碎片消耗。生产判断必须使用运行时 `esp_get_minimum_free_heap_size()`、
`heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL)` 和任务 high-water mark，不能
用 `512 KiB - 静态数据` 直接推导。

### 1.2 CPU 可寻址内存窗口（ESP-IDF 5.5.4 生成链接脚本）

下面是当前 `target/*/build/esp-idf/esp_system/ld/memory.ld` 的实际窗口。它们是
CPU 的虚拟/映射地址，不等同于 Flash 物理偏移，也不能把同一物理 SRAM 的 IRAM 和
DRAM 两个别名相加：

| 窗口 | 起始 | 结束（不含） | 大小 | 可用方式与限制 |
| --- | ---: | ---: | ---: | --- |
| 内部 IRAM0 / DIRAM 指令别名 | `0x40378000` | `0x403CB700` | `0x53700`（333.75 KiB） | 中断安全代码、热点函数；与 DRAM0 共享同一片物理 SRAM，静态代码会减少可用数据/堆 |
| 内部 DRAM0 / DIRAM 数据别名 | `0x3FC88000` | `0x3FCDB700` | `0x53700`（333.75 KiB） | `.data/.bss`、内部 heap、任务栈、DMA；与上行 IRAM 窗口物理重叠，不能重复计量 |
| Flash IROM 映射窗口 | `0x42000020` | `0x42800000` | 约 8 MiB 映射窗口 | `.flash.text` 执行区；实际占用由 app 分区镜像大小决定，不能写入 |
| Flash/PSRAM DROM 映射窗口 | `0x3C000020` | `0x3E000000` | 约 32 MiB 映射窗口 | `.flash.rodata` 和外部 RAM 共用地址窗口；由 MMU/缓存管理，不能按整个窗口当作物理 RAM |
| RTC fast IRAM/DRAM | `0x600FE000` | `0x600FFFE8` | `0x1FE8`（8168 B） | 深睡眠保持、RTC 代码/数据；末尾 24 B 为 `rtc_reserved`，不得使用 |
| RTC slow memory | `0x50000000` | `0x50002000` | 8 KiB | 深睡眠保持数据/ULP；当前未作为业务 heap 使用，启用 ULP 时必须扣除 ULP 保留区 |
| 外部 PSRAM 物理容量 | 由 `0x3C000020` 窗口映射 | — | 2 MiB | `MALLOC_CAP_SPIRAM`/普通大块 malloc；实际可用量需扣除启动测试、分配器元数据和碎片 |

当前配置使用 32 KiB ICache、32 KiB DCache。启动代码还会占用 ROM/二级 bootloader
保留区，`0x403B9000..0x403E0000` 不能作为普通静态 IRAM；这些保留区已经从生成的
链接脚本长度中扣除。`0x60000000` 外设寄存器、`0x40000000` 以上的 CPU/缓存控制区、
ROM 映射区和未列出的 MMIO 地址均不是通用可分配内存。

### 1.3 物理容量、链接容量和运行时容量的区别

必须按三个层次记录内存，避免把“芯片标称容量”误当成“业务可用容量”：

1. **物理容量**：片上 SRAM 512 KiB、PSRAM 2 MiB、Flash 8 MiB；这是芯片/板卡规格。
2. **链接容量**：当前生成脚本为共享 DIRAM 提供 333.75 KiB 地址空间；IRAM 与 DRAM
   是同一物理池的两个视图。Flash IROM/DROM 是映射窗口，不是额外的 SRAM。
3. **运行时容量**：启动后扣除静态段、系统任务、heap 元数据、DMA 连续块、PSRAM
   初始化和碎片；只能由 `heap_caps_get_*` 和任务 high-water mark 确认。

最近一次 Release ELF（以 `xtensa-esp32s3-elf-size -A` 输出为基线）静态段示例为：

| 段 | 大小 | 说明 |
| --- | ---: | --- |
| `.iram0.vectors + .iram0.text` | 约 108 KiB | 占用共享 DIRAM 的 IRAM 侧；`.dram0.dummy` 是链接器跳过的重叠镜像，不应再次相加 |
| `.dram0.data` | 约 27 KiB | 已初始化内部数据 |
| `.dram0.bss` | 约 26 KiB | 零初始化内部数据 |
| `.flash.text` | 约 1.1 MiB | 存放于 app 分区并由 IROM 执行 |
| `.flash.rodata` | 约 354 KiB | 存放于 app 分区并由 DROM 读取 |

上述数值随 feature、链接器版本和业务代码变化，只用于发现趋势；发布验收必须保存
本次 ELF 的 `size -A`、分区镜像大小和设备启动后的最小 heap 水位。

### 1.4 ESP-IDF capability 与可分配边界

业务代码只能通过 capability allocator 判断“这块内存能否用于某类硬件”，不能仅凭
指针地址猜测区域：

| Capability | 对应区域 | 可放置对象 | 禁止/注意事项 |
| --- | --- | --- | --- |
| `MALLOC_CAP_INTERNAL` | 片上 DRAM/IRAM 可作为数据的部分 | 任务栈、控制块、低延迟对象、DMA | 资源最紧张；受 64 KiB `MALLOC_RESERVE_INTERNAL` 约束 |
| `MALLOC_CAP_DMA` | 当前实际使用为片上内部 DRAM | W5500、SPI、UART、I2C 等 DMA 缓冲 | 必须连续、对齐；不能把普通 PSRAM 指针直接传给未声明支持 PSRAM 的驱动 |
| `MALLOC_CAP_SPIRAM` | 2 MiB Quad PSRAM | 协议/文本 blob、RCU 大数组、非实时网络工作区 | 访问经 cache/MMU；ISR、启动早期和硬实时路径不得依赖它 |
| `MALLOC_CAP_8BIT` | 可按字节访问的内部/外部 RAM | 字节缓冲、序列化区、网络 payload | 仍需与 `INTERNAL`/`SPIRAM` 组合约束位置 |
| `MALLOC_CAP_EXEC` | IRAM | 从 RAM 执行的代码 | 仅用于 flash cache 关闭时必须运行的函数；会挤占 DIRAM 数据容量 |
| `MALLOC_CAP_RTCRAM` | RTC fast/slow | 深睡眠保持的少量状态 | 不作为普通业务 heap；需明确掉电/深睡眠生命周期 |

当前 `CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=4096` 只影响**普通 malloc 的放置优先级**，
不改变 capability 硬约束；`Box<[u8]>` 超过 4 KiB 只是“优先 PSRAM”，不是绝对保证。需要
绝对位置时必须调用 `heap_caps_malloc/calloc` 并检查返回值，释放必须使用对应的
`heap_caps_free`。任何分配失败都必须走可恢复错误路径，不得在现场任务中 `unwrap` 触发复位。

生产启动日志至少应记录以下七项，并同时记录单位为字节的数值：

```text
esp_get_free_heap_size()
esp_get_minimum_free_heap_size()
heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT)
heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT)
heap_caps_get_free_size(MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT)
heap_caps_get_minimum_free_size(MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT)
esp_psram_get_size()
```

其中 minimum 值是启动以来的低水位，必须作为发布验收指标；单次 free 值不能证明
长期运行安全。若目标配置启用 RTC fast memory 作为 heap，还要单独记录
`MALLOC_CAP_RTCRAM`，并从深睡眠保持预算中扣除。

## 2. SPI Flash 分区布局

所有偏移均为 Flash 物理地址，区间采用 `[start, end)` 表示。分区表位于 `0x8000`，
旧量产设备的系统 NVS 固定从 `0x9000` 开始，因此不得随意移动。

| 区域 | 起始 | 大小 | 结束(不含) | 内容、所有权和注意事项 |
| --- | ---: | ---: | ---: | --- |
| bootloader | `0x000000` | 最大 `0x8000`（32 KiB） | `0x008000` | 一级启动、分区表加载、OTA 回滚选择；实际镜像必须小于分区表起点，不可由应用覆盖 |
| 分区表 | `0x008000` | ESP-IDF 默认 4 KiB | `0x009000` | `partitions.csv` 的编译产物 |
| `nvs` | `0x009000` | `0x6000` (24 KiB) | `0x00F000` | namespace `gateway`、系统配置、协议 A/B、文本、DO/复位/BLE Mesh 索引等；生产兼容地址固定 |
| `phy_init` | `0x00F000` | `0x1000` (4 KiB) | `0x010000` | ESP-IDF PHY 校准数据 |
| `otadata` | `0x010000` | `0x2000` (8 KiB) | `0x012000` | OTA 当前槽位和回滚状态；完整刷写时可按发布流程清除 |
| 保留空洞 | `0x012000` | `0x5000` (20 KiB) | `0x017000` | 为兼容旧量产分区和 `nvs_keys` 对齐保留；不得写入业务数据 |
| `nvs_keys` | `0x017000` | `0x1000` (4 KiB) | `0x018000` | NVS 加密密钥，分区标志为 `encrypted`；`0x012000..0x017000` 为保留空洞 |
| `holding` | `0x018000` | `0x8000` (32 KiB) | `0x020000` | 原始 PRegBuf/监控字/控制字/扩展线圈的掉电安全 A/B raw store |
| `factory` | `0x020000` | `0x240000` (2.25 MiB) | `0x260000` | 出厂应用镜像；可启动、可作为 OTA 回退目标 |
| `ota_0` | `0x260000` | `0x240000` (2.25 MiB) | `0x4A0000` | OTA 应用槽 0 |
| `ota_1` | `0x4A0000` | `0x240000` (2.25 MiB) | `0x6E0000` | OTA 应用槽 1 |
| `coredump` | `0x6E0000` | `0x10000` (64 KiB) | `0x6F0000` | ELF 格式崩溃转储；满时按 ESP-IDF coredump 策略处理 |
| `ble_mesh` | `0x6F0000` | `0x10000` (64 KiB) | `0x700000` | BLE Mesh 独立 NVS，和系统 `nvs` 隔离 |
| `storage` | `0x700000` | `0x100000` (1 MiB) | `0x800000` | FAT 文件系统，供日志/备份等大文件使用 |

Flash 末地址为 `0x800000`，与 8 MiB 容量一致。应用镜像不得超过 `0x240000`；
`build.rs` 会检查三个应用槽的偏移和大小，偏离即中止构建。

## 3. 运行时 SRAM/PSRAM 布局

ESP-IDF 的链接脚本会根据目标和组件版本决定具体 CPU 虚拟地址。下表是逻辑所有权
和已验证的容量，不应在业务代码中硬编码链接地址。

### 3.1 链接和启动基线

- 当前构建目标：ESP32-S3R2，双核 Xtensa LX7，240 MHz。
- 当前 Release ELF 的链接基线：`.dram0.data + .dram0.bss` 约 54 KiB；生成脚本中的
  `.dram0.heap_start` 到 `.dram0` 末端约 109 KiB 是链接器可交给内部 heap 的候选区，
  该值会随 IRAM 代码、feature、cache 和 ESP-IDF 版本变化，绝不是启动后的 free heap。
- Release 构建应保持 `-O3`、fat LTO、单 codegen unit、`panic=abort` 和 strip；
  不得通过关闭边界检查、放宽栈保护或把 DMA 对象放 PSRAM 来换取尺寸。
- 主任务栈配置为 24 KiB（`CONFIG_ESP_MAIN_TASK_STACK_SIZE=24576`），其余业务栈
  统一引用 `src/safety/stack_budget.rs`。

### 3.2 用户任务栈预算

| 任务 | 栈大小 | 默认是否启用 | 说明 |
| --- | ---: | --- | --- |
| main loop | 24 KiB | 是 | 5 ms 网络调度，并分频执行 DI/DO、AI/AO、BLE 和健康检查 |
| DeviceActor | 12 KiB | 是 | NVS/raw store 持久化；大 blob 工作区移出栈 |
| Modbus RTU port 0 | 8 KiB | 是 | RS485 主站 |
| Modbus RTU port 1 | 8 KiB | 是 | RS485 从站 |
| UDP multicast | 4 KiB | 是 | 组播接收和状态发布 |
| NFC | 6 KiB | 是 | NFC 轮询、备份和恢复 |
| HTTP server | 10 KiB | 是 | Web 配置和流式 OTA |
| Modbus RTU port 2 | 8 KiB | 可选 | UART0 复用调试串口，默认关闭 |
| Wi-Fi heartbeat | 6 KiB | 可选 | Wi-Fi 心跳线程 |

默认用户栈合计 `72 KiB`；全部可选任务打开为 `86 KiB`。编译期断言要求用户栈总量
不超过 `128 KiB`。已知系统任务预算（BTC、BTU、LwIP、系统事件、FreeRTOS timer、
W5500 RX）合计约 `36 KiB`，实际还会包含 ESP-IDF 组件内部任务和控制块。

每个常驻任务必须注册 `TaskHb`，健康监控读取 high-water mark：剩余小于 1024 B 或
使用率达到 90% 时记录诊断。栈预算是上限，不是可随意复用的共享内存。

### 3.3 主要固定缓冲和快照

以下大小是有效载荷或工作区大小，尚未计入 allocator、`Arc`、Mutex 和对象对齐开销：

| 所属模块 | 缓冲/对象 | 大小 | 位置和生命周期 |
| --- | --- | ---: | --- |
| `holding_store` | 单个 raw slot 的编码内容 | 6504 B | `16 B` 头 + 4096 B holding + 256 B monitor + 600 B control + 1536 B legacy coils；写入 `holding` 分区的 8 KiB 槽，双槽轮换 |
| `holding_store` | `HOLDING_BUFFER` | 6504 B | 长期复用的 boxed 工作区，超过 4 KiB，优先 PSRAM |
| `device` | 协议 blob 工作区 | 4097 B | `proto_a/proto_b` 序列化、CRC 和回读校验，优先 PSRAM |
| `device` | 文本 blob 工作区 | 4097 B | 设备文本序列化，优先 PSRAM |
| `device` | 文本回读校验区 | 4097 B | 与写缓冲分离，避免借用切片越过锁作用域 |
| `StorageSnapshot` | protocol data | 3000 B | 1500 个 `u16`，`Arc<[u16]>`，RCU/COW |
| `StorageSnapshot` | device text | 4000 B | 2000 个 `u16`，对应 Modbus `5000..6999` |
| `StorageSnapshot` | holding buffer | 4096 B | 2048 个 `u16`，对应通用 P 区 |
| `StorageSnapshot` | monitor/control/legacy coils | 256 B / 600 B / 1536 B | 旧 MCA 兼容窗口，均为 `Arc` 快照字段 |
| BLE | RX 字符缓冲 | 512 B | GATT 文本/控制输入 |
| BLE | 二进制 RX 槽 | 4 x 512 B | 固定所有权槽，回调只传槽索引 |
| BLE | TX 帧环 | 8 x 272 B | 保持完整业务帧边界，分片只推进 offset |
| UDP multicast | 接收缓冲 | 32 B | 对齐旧 MCA `recvBuffer[32]` |
| ring log | 日志环 | 100 条 `LogEntry` | 固定容量，满后覆盖最旧条目 |

`StorageSnapshot` 使用 RCU：读取只增加 `Arc` 引用，写入使用 COW 后一次发布完整快照。
写入高峰可能短暂同时存在旧、新两代数组，因此必须以运行时最小 heap 为准；不能把
表中的载荷简单相加当作峰值 heap 上限。

## 4. Flash 持久化对象布局

### 4.1 系统 NVS (`nvs`, 24 KiB)

系统 namespace 由 `src/device/mod.rs` 和 `src/device/system_config.rs` 管理。主要对象：

| NVS key/组 | 有效大小 | 语义 |
| --- | ---: | --- |
| `sys_cfg` | 152 B | 固定布局系统配置：SN、位置、硬件/固件字段、网络、BLE、3 路 RS485、4 路 TCP 端口 |
| `proto_a` / `proto_b` | 每个 3010 B | 协议数据 A/B blob：10 B 头（magic/version/length/CRC）+ 1500 `u16` |
| `proto_act` | 1 B | 当前有效协议槽 |
| `dev_text` | 4000 B | 设备文本区，2000 `u16`；另有 magic key |
| `dev_cfg` | 最多 1024 B | 最多 32 个设备功能条目及参数 |
| `do_bits`、复位计数、BLE Mesh 索引等 | 小对象 | 通过独立 magic 或 key 做有效性判断 |

NVS 的实际占用包含页、条目、磨损均衡和（生产启用时）加密开销，不能只按有效
payload 求和判断是否有空间。新增 key 前必须重新核算 24 KiB 分区余量。

协议和文本使用 A/B 或校验后写入策略；应用不得直接擦除整个 `nvs`，否则会同时
丢失网络、端口和 BLE 配置。

### 4.2 Holding raw 分区 (`holding`, 32 KiB)

分区固定为两个 `0x2000` (8 KiB) 槽：

```text
holding + 0x0000 .. 0x1FFF  slot A
holding + 0x2000 .. 0x3FFF  slot B
```

每个槽的当前 raw record：

```text
offset 0x0000  header 16 B (magic/version/generation/length/CRC)
offset 0x0010  holding_buf   2048 x u16 = 4096 B
             monitor_words  128  x u16 =  256 B
             control_words  300  x u16 =  600 B
             legacy_coils   1536      = 1536 B
```

记录实际使用 6504 B，槽内剩余空间保留给版本扩展和对齐。保存顺序是“写 inactive
槽 -> 回读并校验 CRC -> 发布 generation/active slot”；掉电时至少保留上一份有效
记录。旧版本 NVS 的 `hld_buf_a/hld_buf_b` 只作为一次兼容读取来源，不再作为权威写入目标。

## 5. Modbus 逻辑地址空间与内存映射

下表是协议地址，不是 Flash 地址。FC=01/02/03/04/06/15/16 的边界由
`src/modbus/shared.rs` 执行：单次寄存器读取最多 125，单次寄存器写入最多 123，
位读写最多 2000。

| 地址范围 | 类型 | 内存来源 | 说明 |
| --- | --- | --- | --- |
| `0..` | FC=02 离散输入 | IO 实时状态 | DI，实际通道数由 F3/F4 硬件配置决定，未安装点返回 0 |
| `0x0080..` | FC=04 输入寄存器 | AI/状态快照 | AI 数值和状态 |
| `0x0090..0x0100` | FC=04 | UDP multicast 缓冲 | 组播状态窗口 |
| `0x0800..0x0805` | FC=04 | PC 兼容元数据 | 固定窗口，供配置工具读取 |
| `0x087C..0x087F` | FC=04 | 系统只读信息 | 点数、固件版本、日期 |
| `0x0880..0x08A6` | FC=04/03 | 故障、环日志和兼容状态 | 只读诊断窗口 |
| `0x08C7..0x08E5` | FC=04 | IP、MAC、BLE 名称 | 手持机兼容窗口；BLE 名称为 `0x08E2..0x08E5` |
| `0x0200..0x07FF` | FC=01/05/15 | DO/legacy coil 快照 | DO 及旧 MCA 扩展线圈 |
| `0x0880..0x107F` (2176..4223) | FC=03/06/16 | `StorageSnapshot.holding_buf` + `CONFIG` | 通用 PRegBuf；系统字段优先由 `SystemConfig` 解释，未映射字段落 raw holding |
| `2196..2278` | FC=03/06/16 | `SystemConfig` 与兼容 P 区 | SN、位置、串口、TCP、IP、MAC、BLE 节点 ID（D98..D101 别名） |
| `2300..` | FC=03/06/16 | 设备功能表/holding | 设备功能条目及参数 |
| `4000..4223` | FC=03/06/16 | holding raw | 用户逻辑区及保护字 `4222` |
| `0x4000..0x45DB` | FC=03/06/16 | `StorageSnapshot.proto` | 1500 个协议字；末尾为 commit/reload/version/length/status/magic 控制字段 |
| `5000..6999` | FC=03/06/16 | `StorageSnapshot.device_text` | 2000 个文本字；元数据从 `5000` 起按协议解释 |

读请求只获取一代 RCU 快照，因此单个 FC03/FC04 响应不会混合不同配置版本。写请求
在完整批次上执行边界检查，写入 holding/protocol/text 后由 dirty 标志交给
DeviceActor 持久化。

## 6. 设备配置字段完整契约

本节是设备配置的逐字段说明。这里的地址是 Modbus/手持机协议地址；NVS offset 是
`sys_cfg` blob 内的字节偏移，两者不是同一套地址。PC 配置工具从 `2196` 开始读取
83 个保持寄存器（最后地址 `2278`），手持机则主要使用 `0x08A5..0x08E5` 的 BLE
兼容输入窗口。

### 6.1 设备基本信息

| 字段 | Rust 字段 | 保持寄存器 | 字数/字节数 | 编码与边界 | NVS `sys_cfg` |
| --- | --- | --- | ---: | --- | ---: |
| 序列号/SN | `SystemConfig.sn` | `2196..2204` (`HOLD_SN_BASE`, 9 regs) | 9 / 最多 18 个 ASCII 字节对外可见；内部数组 32 B | 每个寄存器 1 个 ASCII 高字节 + 1 个 ASCII 低字节，大端；不足补 `0x00`，超出对外窗口的字节不会通过 PC 设备块传输 | offset `0`, 32 B |
| 设备位置/名称 | `SystemConfig.name` | `2205..2212` (`HOLD_PLACE_BASE`, 8 regs) | 8 / 16 B | 与 SN 相同的大端 ASCII 字节序；支持中文时必须确认客户端字节限制，固件不会把寄存器解释为 UTF-16 | offset `32`, 16 B |
| 硬件型号 | `SystemConfig.hw_version` | `2213` (`HOLD_HW_VER`)；FC=04 兼容镜像 `0x08A5` | 1 / 2 B | F3/F4 型号码由 `hw_version::MODEL_CODE` 定义；当前 F3=`0x00F3`、F4=`0x00F4`。该字段是设备型号，不是软件版本 | offset `48`, 2 B |
| 固件版本 | `SystemConfig.fw_version` | FC=04 `0x087E`；内部别名 `CFG_FW_VER` | 1 / 2 B | Android 版本字段使用数值编码，例如 `221` 表示 `2.2.1`；只读诊断字段 | offset `50`, 2 B |
| 固件日期 | `SystemConfig.fw_date` | FC=04 `0x087F` | 1 / 2 B | 版本日期码，按手持机协议解释；不应当当作硬件型号 | 未单独写入旧 152 B PC blob，按兼容镜像提供 |
| 配置版本 | `SystemConfig.cfg_version` | 内部 `0xFF00` (`CFG_CFG_VER`) | 1 / 2 B | 配置应用时递增；不是硬件型号或固件版本 | offset `52`, 2 B |

SN/位置/型号属于 PC 和手持机共同依赖的基础字段。写入 SN 或位置后返回
`Persist`，不要求额外写 `CFG_APPLY`；写入型号同样持久化。读取时所有字符串在第一个
NUL 截断，避免把填充字节显示为乱码。

### 6.2 网络与物理 MAC

| 字段 | Rust 字段 | 地址 | 字数 | 每寄存器编码 | 写入行为 | NVS |
| --- | --- | --- | ---: | --- | --- | ---: |
| DHCP 开关 | `SystemConfig.dhcp` | 内部 `0xFF03` (`CFG_DHCP`) | 1 | `0`=静态，非 0=DHCP | `Apply`，网络模块监听配置世代变化 | offset `60`, 1 B |
| IPv4 地址 | `SystemConfig.ip` | `2247..2250` (`HOLD_IP_BASE`)；FC=04 镜像 `0x08C7..0x08CA` | 4 | 每个寄存器低 8 位为一个 octet，例如 `192`，高 8 位忽略 | `Apply` | offset `61..64`, 4 B |
| 子网掩码 | `SystemConfig.mask` | `2251..2254`；FC=04 `0x08CB..0x08CE` | 4 | 同 IPv4 | `Apply` | offset `65..68`, 4 B |
| 网关 | `SystemConfig.gateway` | `2255..2258`；FC=04 `0x08CF..0x08D2` | 4 | 同 IPv4 | `Apply` | offset `69..72`, 4 B |
| DNS | `SystemConfig.dns` | `2259..2262`；FC=04 `0x08D3..0x08D6` | 4 | 同 IPv4 | `Apply` | offset `73..76`, 4 B |
| 以太网 MAC | `SystemConfig.eth_mac` | `2263..2268` (`HOLD_MAC_BASE`)；FC=04 `0x08D7..0x08DC` | 6 | 每寄存器低 8 位为一个 MAC octet，显示时使用 `AA:BB:CC:DD:EE:FF` | `Apply`；全 0 启动时回读 ESP-IDF 硬件 MAC | offset `54..59`, 6 B |

IP/MAC 地址不是把两个字节打包在一个寄存器中；这是与 MCA/手持机协议保持一致的
“一个寄存器一个字节”布局。任何写入值大于 `0xFF` 时，固件只取低 8 位，因此上位机
应在发送前严格校验范围 `0..255`。网络字段写入后可能导致监听器重绑或设备重启，
上位机不能在收到写响应后立即假定 TCP 连接仍然有效。

### 6.3 蓝牙字段：名称、节点地址和硬件 MAC 的区别

蓝牙相关字段必须按下表区分，不能把“蓝牙地址”这个 UI 文案直接等同于硬件 MAC：

| 概念 | Rust 字段/来源 | 地址 | 长度/编码 | 是否可写 | 说明 |
| --- | --- | --- | ---: | --- | --- |
| BLE 硬件 MAC | `SystemConfig.ble_mac`，默认从 `esp_read_mac(ESP_MAC_BT)` 读取 | 运行时/内部字段 `CFG_BLE_MAC_BASE=0`；不占用 PC 的 `2274..2277` | 6 B，显示 `AA:BB:CC:DD:EE:FF` | 不通过普通 `2274` 区写入；生产设备应以芯片硬件 MAC 为准 | 这是无线控制器身份地址，不是 BLE 广播名称 |
| BLE 名称/节点 ID | `SystemConfig.ble_name`（唯一权威数据源） | FC=04 输入寄存器 `0x08E2..0x08E5` (`INREG_BLE_ID_BASE`)；FC=03 保持寄存器 `2274..2277` (`D98..D101`, `HOLD_BLE_ADDR_BASE`) | 两个协议窗口均为 4 regs = 8 B，大端 ASCII，最多 8 字节；当前默认 `Mesh` | 两个窗口写入都更新同一 `ble_name` 并 `Persist`；完成配置应用/持久化后由 BLE 模块重建广播数据 | tauri-app 可优先一次读取 FC=03 D20..D102；旧 MCA 客户端仍可读写 D98..D101；221 实测值为 `Mesh001` |

BLE 名称的两个地址窗口共享同一份 `SystemConfig.ble_name`，不存在第二份 raw 缓存，
因此 FC=03 与 FC=04 不会出现读值不一致或重启后回退。BLE 名称写入和硬件 MAC 写入仍是
两条完全不同的持久化/运行时路径，禁止在协议适配层混淆，否则会出现“界面显示为空”、
“重启后名称丢失”或错误修改硬件地址的问题。

> **实现边界（必须注意）**：`0x08E2..0x08E5` 是 FC=04 输入寄存器读取窗口，
> `2274..2277` 是 FC=03 保持寄存器窗口；两个窗口虽然属于不同功能码，但都由
> `SystemConfig.ble_name` 提供数据。旧 MCA 的 `0xCB` 自定义命令写入 D98..D101，
> 普通 FC=06/16 写入 D98..D101，以及 BLE 配置流程都进入同一持久化路径；写入完成后
> 只触发一次 BLE 广播名称重建。生产联调应按“写入 → 读取 FC=03 D98..D101 和
> FC=04 `0x08E2..0x08E5` → 断电重启 → 再读”做闭环校验。

### 6.4 RS485 三个物理端口

三个端口分别对应 `SystemConfig.rs485[0..2]`，每个端口占 5 个保持寄存器，起点为
`2214 + port_index * 5`：

| 相对字 | 字段 | 编码/范围 |
| ---: | --- | --- |
| 0 | 波特率、校验、停止位、数据位、主从模式 | `bits 15..12` 波特率索引：`1=1200, 2=2400, 3=4800, 4=9600, 5=14400, 6=19200, 7=38400, 8=57600, 9=115200, 10=128000, 11=153600, 12=230400, 13=256000, 14=460800, 15=921600`；`bits 11..10` 校验；`bit 9` 停止位（0=1 位，1=2 位）；`bit 8` 数据位（0=8 位，1=7 位）；低字节 `0x01`=主站，其他值=从站 |
| 1 | 从站地址 | 低 8 位有效，通常 `1..247` |
| 2 | 重试次数 | `u16`，由 `retry_count` 保存 |
| 3 | 响应超时 | `u16` 毫秒，由 `timeout_ms` 保存 |
| 4 | 轮询间隔 | `u16` 毫秒，由 `interval_ms` 保存 |

RS485 配置写入后立即返回 `Persist`，运行中的串口任务在配置变化后重新应用。PC 工具
的 BT/NET 兼容块位于相邻的保留区，不得把它们误写成 RS485-1，否则会破坏现场串口
配置。

### 6.5 TCP 监听端口和兼容保留字段

| 字段 | 地址 | 默认值 | 说明 |
| --- | --- | --- | --- |
| TCP COM1..COM4 | `2243..2246` (`HOLD_TCP_COM_BASE`) | `502, 503, 504, 5002` | 每个地址一个 `u16` 端口；写 0 被拒绝；写入后持久化并由 TCP 服务重绑 |
| PC local_port1..4 保留字段 | `2239..2242` (`HOLD_UNKNOWN_BASE`) | `5500, 5501, 5502, 5503` | 旧 PC/MCA 的兼容 raw PRegBuf，当前不解释为 TCP 监听端口；读写原样保持 |
| 主站 COM 数量 | `2269` | 由协议/设备配置决定 | 旧 MCA 主站兼容字段，未映射内容保留在 raw holding |
| 主站 IP | `2270..2273` | 由工程配置决定 | 旧 MCA 兼容字段，按 raw holding 原样保存 |

### 6.6 传感器校准字段

| 字段 | 地址 | 范围 | 说明 |
| --- | --- | --- | --- |
| 传感器最小值 | `2280..2287` (`HOLD_SENSOR_MIN_BASE`) | 8 个 `u16` | 对应 8 路标定下限 |
| 传感器最大值 | `2288..2295` (`HOLD_SENSOR_MAX_BASE`) | 8 个 `u16` | 对应 8 路标定上限；必须大于最小值，采样转换时执行边界钳位 |

这些字段属于通用 holding/raw 配置，具体校准业务由 AI 模块消费；修改后由 holding
dirty 路径异步写入 `holding` 分区，不写入 `sys_cfg` 的 152 B 系统配置 blob。

### 6.7 设备功能配置表（2300+）

`2300` 起不是固定结构体，而是可扩展的变长设备条目表，最多 32 条。布局如下：

| 相对字 | 内容 |
| ---: | --- |
| `2300` | 已存条目数 `stored_count`；实际设备数最大 32 |
| 每条第 0 字 | 低字节 `DeviceType`，高字节 RS485 端口 |
| 每条第 1 字 | 低字节从站地址，高字节 Modbus 功能码 |
| 每条第 2 字 | 从站寄存器起始地址 |
| 每条第 3 字 | 寄存器数量 |
| 每条第 4 字 | 参数数量 `N`，最大 32 |
| 每条后续 `N` 字 | 设备类型专用原始参数，`u16` 原样保存 |

每条占 `5 + N` 个寄存器。设备类型通过 `DeviceType::from_u8` 解码，轮询引擎只消费
端口、从站、功能码、地址和数量，具体设备控制逻辑按类型扩展。未知类型和未知参数
不会使整张表解析失败；原始参数仍可读回并持久化到 NVS key `dev_cfg`（最多 1024 B）。

### 6.8 `sys_cfg` 固定 blob 的完整字节布局

`SystemConfig` 的 NVS key 为 `sys_cfg`，当前 blob 固定 152 B；前 144 B 兼容旧版本。
多字节整数在 NVS 中使用小端序，字符串和地址数组按字节原样保存：

| 字节偏移 | 长度 | 对应字段 |
| ---: | ---: | --- |
| `0..31` | 32 B | `sn` |
| `32..47` | 16 B | `name`（设备位置） |
| `48..49` | 2 B | `hw_version`（设备型号） |
| `50..51` | 2 B | `fw_version` |
| `52..53` | 2 B | `cfg_version` |
| `54..59` | 6 B | `eth_mac` |
| `60` | 1 B | `dhcp` |
| `61..64` | 4 B | `ip` |
| `65..68` | 4 B | `mask` |
| `69..72` | 4 B | `gateway` |
| `73..76` | 4 B | `dns` |
| `77..82` | 6 B | `ble_mac`（硬件蓝牙 MAC） |
| `83..90` | 8 B | `ble_name`（BLE 广播名称/节点 ID） |
| `92..106` | 15 B | `rs485[0]` |
| `107..121` | 15 B | `rs485[1]` |
| `122..136` | 15 B | `rs485[2]` |
| `137..144` | 8 B | `tcp_ports[0..3]`，每项 2 B |
| `145..151` | 7 B | 预留，必须保留为 0/兼容扩展空间 |

`fw_date` 当前用于输入寄存器/手持机兼容读取，但不在上述固定 blob 中单独占用字节；
修改其持久化布局必须增加版本号和兼容解码，不能直接挤占旧字段。

### 6.9 配置写入结果与持久化路径

| 结果 | 含义 | 典型字段 |
| --- | --- | --- |
| `Ok` | 写入当前配置但不触发 NVS | 只读/诊断或内部版本字段 |
| `Persist` | 写入配置并持久化，不改变配置版本 | SN、位置、型号、RS485、BLE 名称、TCP 端口 |
| `Apply` | 写入、持久化并递增配置版本；网络/BLE 等模块重新应用 | DHCP、IP、掩码、网关、DNS、以太网 MAC |
| `Reset` | 恢复 `SystemConfig::defaults()` 并持久化 | 写 `CFG_RESET_DEFAULT=0xD5D5` |
| `NotFound` | 不属于 `SystemConfig`，交给 raw holding/PRegBuf | 2239 保留区、用户逻辑区 |

配置写入的最终顺序是：校验地址和值 → 发布 CONFIG/STORAGE 快照 → 设置 dirty 标志
或发送 DeviceActor 持久化消息 → 回读校验 → 对网络/BLE 配置执行异步应用。任何客户
端都不应只依据 Modbus 写响应判断“已经写入 Flash”，应在重连或重启后再次读取验证。

## 7. 监控、诊断和发布检查

### 7.1 编译期检查

`build.rs` 会拒绝以下漂移：

- ESP32-S3 目标、2 MiB PSRAM、64 KiB internal reserve、20 个 socket。
- DIO、40 MHz、8 MiB Flash。
- `nvs`、`holding`、`factory`、`ota_0`、`ota_1` 的固定偏移和大小。
- Cargo 包版本与 `CONFIG_APP_PROJECT_VER` 不一致。
- 用户任务栈总预算超过 128 KiB，或 F3/F4 同时启用。

### 7.2 运行时检查

生产启动和压力测试至少记录：

```text
esp_get_free_heap_size()
esp_get_minimum_free_heap_size()
heap_caps_get_free_size(MALLOC_CAP_INTERNAL)
heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL)
每个 TaskHb 的 uxTaskGetStackHighWaterMark2()
```

同时验证：8 个 TCP 长连接、BLE 分片收发、三路 RTU（如启用）、Web/OTA、NFC 和
组播并发时没有内部 heap 低水位告警、栈余量低于 1 KiB、DMA 分配失败或 WDT 超时。

推荐命令：

```bash
cargo check --all-features
cargo test --all
cargo build --release
```

发布刷写必须同时携带 bootloader、分区表、目标 app，并保留 `nvs`、`holding` 和
`otadata` 的兼容布局；只刷 app 会破坏本文件所述的分区契约。

## 8. 变更规则

1. 移动 Flash 分区、改变 NVS/raw record 或扩大协议/文本数组，必须先评估掉电恢复、
   OTA 回滚和旧设备兼容性。
2. 新增长期缓冲优先放 PSRAM；DMA、ISR、任务栈对象必须显式保留在内部 SRAM。
3. 不得在 Modbus/BLE 热路径创建无界 `Vec`、复制整块协议/文本或增加按连接 pthread。
4. 任何容量调整都要同时修改源码常量、`build.rs` 硬门槛、测试和本文档。
5. 以运行时最小水位和真实硬件浸泡测试作为最终内存容量依据，编译成功不等于内存
   余量满足工业现场长期运行要求。
