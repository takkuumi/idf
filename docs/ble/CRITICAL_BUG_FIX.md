# 蓝牙修复总结 - 根本原因

## 关键Bug: 在GATT回调中直接调用 send_indicate 导致死锁

ESP-IDF Bluedroid API 明确规定:
**`esp_ble_gatts_send_indicate` 不能在 `esp_gatts_cb_t` 回调中直接调用!**

因为 `send_indicate` 会触发 GATT 事件（例如 `ESP_GATTS_CONF_EVT`），
如果在回调中调用该函数，会导致**回调重入**（callback reentrancy），
具体表现为:
- 系统死锁（Bluedroid内部mutex死锁）
- 通知永远不会发送出去
- Android端收不到任何响应
- 蓝牙看起来连接成功，但实际数据交互完全阻塞

## 之前的所有修复都没解决根本问题

之前一直在纠结心跳格式、MTU大小、写响应顺序等，但这些都不是根本原因。
即使心跳格式完全正确，发送通知的操作在GATT回调中也会死锁，
Android端永远收不到任何数据。

## 修复方法

将 `send_ble_rsp` 中的 `send_indicate` 调用放到独立的 `std::thread::spawn` 线程中执行:

```rust
fn send_ble_rsp(data: &[u8], conn_id: u16) {
    // 在 GATT 回调中不能直接调 send_indicate!
    // 改为在新线程中调用
    std::thread::spawn(move || {
        unsafe {
            esp_ble_gatts_send_indicate(gatts_if, conn_id, handle, ...);
        }
    });
}
```

## 验证方法
1. 烧录固件
2. 用nRF Connect或手持机连接设备
3. 写入AT命令（如 "AT+VERSION\r\n"）
4. 应该能立即收到 "OK esp32s3-iot-gateway v0.1.0 (HW: F16)\r\n" 响应
5. 如果之前连接无响应是正确路径，现在应该能收到响应了

## 修改文件
- `src/ble_at/mod.rs`: `send_ble_rsp` 改为在独立线程中调用 `send_indicate`
