# SDKCONFIG 与持久化性能核查（2026-10-31）

## 范围与结论

基线为 Git `7d6b580`、ESP-IDF v5.5.4、esp-idf-svc 0.53.0。
核对 `sdkconfig.defaults`、DEFAULT/F3/F4 已生成的 `out/sdkconfig`，并阅读本机
ESP-IDF NVS/Flash 实现和当前 Rust 持久化路径，未将注释视为实现证据。

三种型号的相关设置一致：240MHz CPU、C 性能优化、DIO/40MHz/8MB Flash、
1000Hz FreeRTOS tick、内部 RAM NVS cache、IRAM Flash 操作函数、8192 字节写块、
擦除 20ms 阈值/1 tick 让出 CPU。目前没有发现能靠一个配置开关显著缩短实际
保存耗时、又已有该硬件验证依据的优化。未测得吞吐或延迟提升比例。

本次不改变版本、分区布局、Flash 频率、自动保存周期、A/B 或 CRC 校验，
不刷设备、不覆盖已有发布包。已有发布包仍是本次修改前的配置。

## 修改

| 配置 | 原有效值 → 本次值 | 原因 |
| --- | --- | --- |
| `SPI_FLASH_USE_4BLOCK_ERASE` | 无效旧键 → 删除 | v5.5.4 无此 Kconfig，不能认为已启用快速擦除 |
| `NVS_ASSERT_ERROR_CHECK` | y → n | 将相应内部检查失败返回错误码，供现有失败/重试路径处理，而非直接 assert 重启；这不是写入加速 |
| `NVS_ALLOCATE_CACHE_IN_SPIRAM` | n → 显式 n | 固定内部 RAM cache；IDF 明确说明 PSRAM cache 会降低 NVS 操作速度 |
| `SPI_FLASH_PLACE_FUNCTIONS_IN_IRAM` | y → 显式 y | 保持现有 Flash 函数执行性能 |
| `SPI_FLASH_BYPASS_BLOCK_ERASE` | n → 显式 n | 不强制把大块擦除拆成慢速 sector 擦除；NVS 单页 4KB/holding 单槽 8KB 本来就不满足 64KB 块擦除大小 |
| `SPI_FLASH_YIELD_DURING_ERASE` | y → 显式 y | 继续允许任务在擦除命令之间运行 |
| `SPI_FLASH_ERASE_YIELD_DURATION_MS` | 20 → 显式 20 | 保持原调度边界，不能理解为擦除最多只阻塞 20ms |
| `SPI_FLASH_ERASE_YIELD_TICKS` | 1 → 显式 1 | 本机 1000Hz 下为 1ms，关闭让出 CPU 的收益没有实测依据 |
| `SPI_FLASH_WRITE_CHUNK_SIZE` | 8192 → 显式 8192 | 已是 Kconfig 上限；不是 NOR 芯片物理 page 大小，也不消除内部编程等待 |
| `SPI_FLASH_AUTO_SUSPEND` | n → 显式 n | 需要按实际 Flash 型号验证，不盲目开启 |

同步修正本区域“只预留 16KB internal RAM”“NVS cache 走 PSRAM”和
“生产已启用 NVS 加密”的不实/歧义注释。常规堆栈保护、CRC、业务回读均保留。

## 落盘及时性取决于哪一层

1. `src/actor/mod.rs::spawn` 每批最多处理 8 条消息，随后执行 idle；
   `DeviceActor::idle` 返回 50ms 睡眠。入队等待还包括前序消息和 Flash/锁等待，
   不能把 50ms 当作端到端硬上限。
2. 系统配置、设备文本、Web 元信息和 DO 自动保存有 1 秒调度/节流。
   holding/control/legacy 自动保存还要求最后一次相关修改后静默 2000ms。
   若持久化区被持续写入，静默条件可以一直不满足；这是软件策略，不是 NVS commit 慢。
3. Web 保存调用 `await_pending_persistence`，在 Actor 上完成持久化屏障后才返回成功；
   队列满、并发等待、失败或 8 秒超时有失败返回。显式 holding 保存不受 idle 静默条件限制。
   Modbus 普通写响应只确认 RAM 接收，不能等同于 Flash 落盘确认。
4. `holding_store::save_to_nvs` 名称具有误导性：当前实际写独立 `holding` raw 分区，
   每次擦除 inactive 槽 8192 字节、写入 6504 字节、回读 CRC 后发布新代数。
   `NVS_ALLOCATE_CACHE_IN_SPIRAM` 无法优化这条路径。
5. NVS 的 `nvs_set_*` 调用执行实际存储；本版本 IDF 的 `nvs_commit` 仅检查句柄并返回
   `ESP_OK`，没有后台定时 flush。仍须保留公共 API 的 commit 调用，不能依赖实现细节删掉。
6. esp-idf-svc 0.53.0 的 `EspNvs::set_blob` 会先 `nvs_erase_key`，再 `nvs_set_blob` 和
   `nvs_commit`。重复保存、GC、blob 大小和擦除/编程时间都可能比 commit 本身更重要。
   当前文本路径已做内容去重；其它 blob 路径仍需分别评估。这不是 SDKCONFIG 可以解决的问题，
   本轮未改依赖库或持久化格式，也未将单 key 保存宣称为完整掉电事务。

## 不采用的“加速”

- 不调 QIO/80MHz：本项目此前硬件启动验证失败，DIO/40MHz 为现行生产边界。
- 不缩短 holding 静默周期、不提高自动保存频率：会改变多帧写入语义和擦写寿命。
- 不关闭 CRC/回读、watchdog/栈保护，不强行禁用擦除让出 CPU。
- 不扩大 NVS 分区：需迁移现场布局，且不会直接加快单次物理编程。
- 不将 NVS cache 放 PSRAM，不把 1000Hz tick 改低以牺牲现有毫秒调度精度。

## 验证方法与边界

`tests/host/check_storage_sdkconfig.py` 检查 defaults 和调用者明确提供的已生成
sdkconfig，必须提供 `--generated`；发现键缺失或旧产物仍启用 NVS assert 会失败。
例如：

```bash
python3 tests/host/check_storage_sdkconfig.py --generated \
  target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-*/out/sdkconfig
python3 tests/host/run_production_core.py
```

本轮验证：配置脚本对 defaults 与生成 sdkconfig 均通过（13 项）；F3 `cargo build --locked --bin gateway --features f3` 通过；`cargo fmt --all -- --check` 通过；`tests/host/run_production_core.py` 25/25 通过；`cargo check --locked --bin gateway --features f3` 0 warning。未进行本版本实机写耗时 A/B 测量、强制 NVS 故障注入或物理掉电测试，不能从编译结果推导毫秒级性能提升或全系统量产放行。

## 实现依据

- ESP-IDF：`components/nvs_flash/Kconfig`、`private_include/nvs_internal.h`、
  `src/nvs_api.cpp::nvs_commit`、`src/nvs_handle_simple.cpp::commit`。
- ESP-IDF：`components/spi_flash/Kconfig`、`esp_flash_api.c`、`spi_flash_os_func_app.c`。
- esp-idf-svc 0.53.0：`src/nvs.rs::EspNvs::set_blob`。
- 本项目：`src/device/mod.rs`、`src/device/holding_store.rs`、
  `src/bus/storage_state.rs`、`src/actor/mod.rs`、`src/web/mod.rs`。
