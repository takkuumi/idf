# Android 设备信息读取修复 (DI 别名)

## 问题
用户报告: 蓝牙连上后,设备型号、固件版本、SN号、位置编号、MAC地址、IP地址、子网掩码、网关地址、蓝牙ID、通讯配置、I/O信息、测控执行组态读不出来。

## 根本原因
通过深入分析 Android 手持机 (`metuory-wireless-management-app-1.0.78`) 源码,发现:

**Android 端 sendReadComInputIOStatusCMD 使用 FC=01 (读线圈) 读取 DI**,但 DI 在 Modbus 规范里应该用 FC=02 (读离散输入)。Android 的实现是:
```java
public static byte[] readComInputIOStatusCMD(short transmissionId, byte unitId, short count) {
    CMDFunctionCodeEnum functionCode = CMDFunctionCodeEnum.READ01;  // FC=01, 不是 FC=02!
    byte[] data = buildReadData(CMDTransmissionTypeEnum.READ_COM_INPUT_IO_STATUS, count);
    // 地址 = 0x0000
}
```

而我们设备的 `read_coil` 只处理 0x0200-0x02FF (DO) 范围的线圈:
```rust
pub fn read_coil(&self, addr: u16) -> Option<bool> {
    if addr >= regs::COIL_DO_BASE && addr < regs::COIL_DO_END {
        // ...
    } else {
        None  // 返回 None, Modbus 异常
    }
}
```

所以 Android 读取 DI 时,我们的设备返回异常 0x02 (ILLEGAL_DATA_ADDRESS)。

## 修复
参考 MCA 源码 `MODS_01H` 函数,它的 FC=01 (读线圈) 处理器在地址 < 0x0200 时实际读取 DI:
```cpp
for (i = 0; i < num; i++) {
    if(reg - REG_T01 + i <= REG_TMAX) {  // REG_T01=0x0000, REG_TMAX=0x000F
        // 读输入状态 (DI)
    }
    else if(reg - REG_T01 + i >= REG_D01 && ...) {  // REG_D01=0x0200
        // 读线圈状态 (DO)
    }
}
```

在 `src/bus.rs` 中添加 DI 别名:

```rust
/// 读取线圈 (FC=0x01).
///
/// 地址分配 (与 MCA 一致, 兼容 Android 手持机):
/// - 0x0000-0x001F: 别名读取 DI (DI 离散输入)
/// - 0x0200-0x02FF: 读取 DO (线圈)
pub fn read_coil(&self, addr: u16) -> Option<bool> {
    // DO 范围 (0x0200+)
    if addr >= regs::COIL_DO_BASE && addr < regs::COIL_DO_END {
        let ch = (addr - regs::COIL_DO_BASE) as usize;
        return Some(self.do_.bits & (1u64 << ch) != 0);
    }
    // 别名: FC=01 读取 0x0000-0x001F 范围时, 实际读取 DI
    if addr < regs::DISC_DI_COUNT as u16 {
        return Some(self.di.bits & (1u64 << addr) != 0);
    }
    None
}
```

## 验证结果

所有 16 个 Android 设备信息命令通过 E2E 验证:

| Android 命令 | 寄存器 | FC | 结果 |
|--------------|--------|-----|------|
| 设备型号 (Device Product) | 0x08A5 | 0x03 | 0x0100 ✓ |
| 固件版本 (FW Version) | 0x087E | 0x04 | 0x0100, Date 0x0615 ✓ |
| SN 号 | 0x0894 | 0x03 | 'ESP32S3-UNKNOWN-00' ✓ |
| 位置编号 (Location) | 0x089D | 0x03 | 'GW-ESP32S3' ✓ |
| MAC 地址 | 0x08D7 | 0x03 | 80:B5:4E:5B:24:E7 ✓ |
| IP 地址 | 0x08C7 | 0x03 | 192.168.51.221 ✓ |
| 子网掩码 (Mask) | 0x08CB | 0x03 | 255.255.255.0 ✓ |
| 网关地址 (Gateway) | 0x08CF | 0x03 | 192.168.51.1 ✓ |
| 蓝牙 ID (BT ID) | 0x08E2 | 0x03 | 'Mesh' ✓ |
| 通讯配置 (RS485-1/2/3) | 0x08A6/AB/B0 | 0x03 | 5 words each ✓ |
| I/O 信息 (DI 离散输入) | 0x0000 | **0x01** | bits ✓ (修复后) |
| I/O 信息 (DO 线圈输出) | 0x0200 | 0x01 | bits ✓ |
| I/O 信息 (AI 模拟输入) | 0x0080 | 0x04 | raw values ✓ |
| 测控执行组态 (Device Func) | 0x08FC | 0x03 | function config ✓ |

## 修改文件
- `src/bus.rs`: 修改 `read_coil` 函数, 添加 DI 别名
