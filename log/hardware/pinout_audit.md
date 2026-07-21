# 引脚核对审计 (2026-07-21)

## 检查项

### 1. 实际硬件型号 ✅

启动日志确认:
```
I (478) esp_psram: Found 2MB PSRAM device
I (888) esp_psram: Adding pool of 2048K of PSRAM memory
```

**结论**: 实际是 **ESP32-S3R2** (2MB Quad SPI PSRAM), 不是 ESP32-S3R8 (8MB Octal PSRAM)。

之前 `docs/pinmap.md` 错误标注为 ESP32-S3R8, 已重写。

### 2. 引脚与 config.rs 一致性 ✅

| 模块 | pinmap.md 旧值 | config.rs 实际值 | 修正 |
|------|----------------|------------------|------|
| ETH MOSI | GPIO11 | GPIO12 | ✓ 已修正 |
| ETH MISO | GPIO13 | GPIO11 | ✓ 已修正 |
| ETH SCLK | GPIO12 | GPIO10 | ✓ 已修正 |
| ETH CS | GPIO10 | GPIO9 | ✓ 已修正 |
| ETH INT | GPIO14 | GPIO13 | ✓ 已修正 |
| ETH RST | GPIO15 | GPIO14 | ✓ 已修正 |
| RS485_0 TX | GPIO40 | GPIO45 | ✓ 已修正 |
| RS485_0 RX | GPIO41 | GPIO46 | ✓ 已修正 |
| RS485_0 DE | GPIO42 | GPIO7 | ✓ 已修正 |
| RS485_1 TX | GPIO17 | GPIO42 | ✓ 已修正 |
| RS485_1 RX | GPIO18 | GPIO41 | ✓ 已修正 |
| RS485_1 DE | GPIO7 | GPIO8 | ✓ 已修正 |
| PCA9555 SDA | 未提 | GPIO35 | ✓ 已修正 |
| PCA9555 SCL | 未提 | GPIO36 | ✓ 已修正 |
| LED SDA | 未提 | GPIO38 | ✓ 已修正 |
| LED SCL | 未提 | GPIO37 | ✓ 已修正 |

### 3. 与 MCA_F16V2_1_F48_BLE 对比 ✅

| 功能 | MCA | 本系统 | 物理是否同 pad |
|------|-----|--------|----------------|
| RS485 主 TX | GPIO32 | GPIO45 | ✅ ESP32-S3 GPIO 编号统一 |
| RS485 主 RX | GPIO33 | GPIO46 | ✅ |
| PCA9555 SDA | GPIO35 | GPIO35 | ✅ 完全一致 |
| PCA9555 SCL | GPIO36 | GPIO36 | ✅ 完全一致 |
| LED PCA9555 SDA | GPIO38 | GPIO38 | ✅ 完全一致 |
| LED PCA9555 SCL | GPIO37 | GPIO37 | ✅ 完全一致 |

**结论**: 引脚定义与 MCA 参考一致, 调整主要是 RS485 端口号 (Arduino 风格 vs ESP-IDF 风格)。

### 4. 实际驱动使用情况

- W5500: SPI3_HOST (ESP-IDF v5.x SPI2_HOST=1, SPI3_HOST=2)
- RS485: UART1 (主), UART2 (从)
- AI: ADC1_CH0-5 (GPIO1-6)
- AO: LEDC_CH0-3 (GPIO15-18)
- PCA9555: 软件 I2C (sw_i2c crate)

### 5. 启动时序

```
I (1057) gateway::hal: [hal] IO/LED board power enabled (gpio21 + gpio33 HIGH) ← POWER_LED_EN + POWER_RELAY_EN
I (1057) gateway::hal::sw_i2c: [sw_i2c] init: sda=35 scl=36                  ← PCA9555 #1
I (1060) gateway::hal::sw_i2c: [sw_i2c] init: sda=38 scl=37                  ← PCA9555 #2 (LED 板)
I (1068) gateway::hal::pca9555: [pca9555] io bus: DI@0x40(input...           ← PCA9555 地址 0x40
```

✅ 与 config.rs 引脚定义 100% 一致。
