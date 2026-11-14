# F4 48 点 DI/DO 位序根因与修复

日期：2026-11-14（Asia/Shanghai）

## 根因

此前的主机测试和注释错误地假定 F4 PCA9555 的 P0、P1 都要 bit reverse，并将这项假设写入 `decode_di_bank_f4`。重新核对旧固件 `nca9555.cpp::nca9555_read_all_Gpio_State` 与 `nca9555_Write_all_Q_LEDState` 后，真实规则是每片器件 **P0 直接映射、P1 才反转**：

- DI：P0=`!raw`；P1=`bit_reverse(!raw)`。
- F4 DO：P0 直接写；P1=`bit_reverse(logical)`。
- F3/F16 DO 是另一个旧版路径，P0/P1 均反转，不能将 F4 规则套到 F3。

旧固件地址顺序：DI `0x40/0x44/0x42` 对应三组 16 点；DO `0x48/0x4C/0x4E` 对应三组 16 点。当前 Rust 地址顺序原本与旧固件一致，错误在位变换，而非芯片地址组顺序。

## 修复内容

- F4 DI 恢复 P0 直接位序，仅对 P1 做反转。
- F4 DO 输出写入与读回均改为 P0 直接位序、P1 反转。
- F3/Default 输入映射不变；F3/Default DO 仍保持双端口反转。
- 测试按每片、每个端口、每个引脚验证逻辑编号，覆盖 P0/P1 边界。

## 构建与 F4 实机刷写验收（2026-11-14）

- `cargo check --locked --bin imc --features f4`：通过。
- `cargo check --locked --bin imc --features f3`：通过。
- `python3 tests/host/run_io_transactions.py`：F3/Default 11/11、F4 11/11 通过；覆盖所有 DI/DO 位序、读回和 NACK 路径。
- `cargo build --release --locked --bin imc --features f4`：通过。
- F4 app：1,976,228 字节，占 2,359,296 字节 factory/OTA app 槽 83.76%；SHA-256 `ee415e572573cc1fa9caa22a15705d6492c78309f1cfa0f2e2de25e4f1bfb9ca`。
- 使用项目规定的 `espflash flash --no-skip` 完整写入 bootloader、partition table、otadata 和 factory app，随后设备成功启动并通过网络复核。固件寄存器仍为 3.2.0，型号寄存器 `0x00F4`，当前分区 `factory`。
- Web、Modbus TCP 可达；系统状态为 `Normal`，以太网链路已连接。PCA9555 诊断：`di_valid=true`、`di_failures=0`、`do_readback_valid=true`、`do_failures=0`，输出队列为空。
- 刷写前后持久化 DO 逻辑位均为 Q8、Q24、Q40 开。没有对外部负载执行额外 Q 点切换。
- 当前 I 点读回全为 0；不能据此替代带线端子的逐点功能确认。编译和刷机验证的是映射实现及固件运行，实物 Q/I 的逐点回环仍需在正确端子接线条件下确认。
