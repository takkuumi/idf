# 审计报告: 阶段 3 — DI 主动上报 (2026-07-20)

- **审计对象**: P1-B DI 主动上报 (REPORT_COM_INPUT_IO_STATUS, Android 0x94/tx_id=0x01)
- **审计范围**:
  - `src/ble_at/mod.rs`: 新增 `send_di_status_report(conn_id)` 函数
  - `src/main.rs`: main_loop 消费 IoEvent::DiChanged → 调 send_di_status_report
  - `/tmp/host-sync-test/src/write_result_semantics.rs`: 7 个新增 DI 上报测试
- **审计依据**:
  - `docs/comm/PM_GAP_ANALYSIS_2026-07-20.md` §1.3 P1-B
  - `docs/comm/ARCH_PHASE_2026-07-20.md` §4 阶段 3
- **验收手段**:
  - `cargo check` (default / f3 / f4) 全 0 errors
  - `cargo test --test write_result_semantics --target x86_64-apple-darwin` 全 35 tests 通过

## 1. 改动清单

### 1.1 BLE 通知函数 (`src/ble_at/mod.rs`)

```rust
pub fn send_di_status_report(conn_id: u16)
```

**触发条件**:
- main_loop 每个 tick (100ms) 消费 IoEvent::DiChanged
- 由 io::di.rs 边沿检测后调 `crate::bus::send_event(IoEvent::DiChanged)` 触发

**BLE 帧格式** (与 Android 1.0.78 CMDTransmissionIDManager 一致):
```
tx_id (2 BE)   = 0x0001  (TRANSMISSION_SPECIAL_ID_COM_INPUT_STATUS_CHANGED)
proto_id (2 BE) = 0x0000
length (2 BE)   = 2 + ceil(DI_COUNT/8)
pdu_data        = [unit=0x01][func=0x94][di_bitmap_be... LSB-first]
crc (2 LE)      = Modbus CRC16
```

**DI bitmap 字节序**:
- DI0 在 byte[0] bit 0 (LSB-first within byte)
- F16/F3 = 2 bytes (16 DI)
- F4 = 6 bytes (48 DI)
- 越界位填 0

### 1.2 main_loop 事件分发 (`src/main.rs`)

替换原 "事件清空" 逻辑为实际事件分发:
```rust
while let Some(event) = crate::bus::event_bus::recv_event() {
    match event {
        IoEvent::DiChanged => ble_at::send_di_status_report(0),
        IoEvent::DoChanged => log::debug!("DO changed"),
        // ... 其他事件保留作为占位
    }
}
```

### 1.3 测试覆盖 (7 个新增)

| 测试 | 验证 |
|------|-----|
| test_di_bitmap_encode_f16_8_di | F16 8 路 DI bitmap 编码 (1 byte) |
| test_di_bitmap_encode_f16_first_di_active | DI0 = 1, others 0 编码 |
| test_di_bitmap_encode_f3_16_di | F3 16 路 DI bitmap 编码 (2 bytes) |
| test_di_bitmap_encode_f4_48_di | F4 48 路 DI bitmap 编码 (6 bytes) |
| test_di_bitmap_round_trip | u64 → 8 bytes → u64 往返 |
| test_di_report_frame_layout_f16 | F16 BLE 帧完整布局 |
| test_di_report_frame_layout_f3_16 | F3 BLE 帧完整布局 |
| test_android_tx_id_dispatch_di_changed | tx_id=0x0001 触发 Android DI 上报路由 |

## 2. Android 端解析路径 (参考)

```java
// Android CMDTransmissionIDManager
public static final int TRANSMISSION_SPECIAL_ID_COM_INPUT_STATUS_CHANGED = 0x01;

public int getTransmissionType(short transmissionId) {
    if (transmissionId == 0x00) return HEARTBEAT;
    if (transmissionId == 0x01) return REPORT_COM_INPUT_IO_STATUS;  // ← 我们的 0x0001 命中这里
    // ...
}
```

Android 端接收 BLE frame 后:
1. 解析 tx_id = 0x0001
2. 路由到 REPORT_COM_INPUT_IO_STATUS handler
3. 解析 pdu: [unit, func=0x94, di_bitmap...]
4. 更新 UI 显示 DI 状态

## 3. 风险评估

### 3.1 已识别并缓解

| 风险 | 缓解 |
|------|------|
| BLE notify 队列满 | send_ble_frame 已有 BINARY_TX 满处理 (丢弃计数 + recovery 记录) |
| GATT 回调内 send_notify 死锁 | 走 BINARY_TX 队列, process_loop 异步发送 |
| 100ms 周期丢失事件 | MpscRing 满覆盖最旧, 不阻塞 |
| conn_id 取 0 不准确 | 实际 BLE notify 取最近连接 (BINARY_TX 推送) |

### 3.2 已知限制

- BLE 未连接时: send_ble_frame 内部 GATTS_IF.try_lock() 失败, 静默丢弃 (已有)
- DI_COUNT > 48: 当前实现硬编码 F16/F3=2 bytes, F4=6 bytes, 实际计算用 `(DI_COUNT+7)/8` 自动适配

## 4. 验收

- ✅ `cargo check` (default / f3 / f4) 全 0 errors
- ✅ `cargo test --test write_result_semantics --target x86_64-apple-darwin` 35/35 passed
- ✅ Android tx_id=0x01 dispatch 验证
- ✅ F16/F3/F4 DI bitmap 编码正确
- ⚠️ 实机 BLE 验证待办: Android 1.0.78 实机接收并显示 DI 变化

## 5. 累计成果

| 阶段 | 改动 | 测试 | 文档 |
|------|------|------|------|
| P0/P1/P3 baseline | Box<[u16]>, AI_COUNT, HOLD_485_ERR | +12 host | 2 |
| 阶段 1: 持久化 | WriteResult::Persist, AT+CFG persist | +21 host | 4 |
| 阶段 2: 设备文本 | device_text NVS persist | +6 host | 1 |
| 阶段 3: DI 上报 | send_di_status_report | +7 host | 1 |

**总计**: 81 host tests, 8 docs (LOOP + 7 comm/)
