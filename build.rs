// 构建脚本：调用 embuild 编排 ESP-IDF
fn main() {
    embuild::espidf::sysenv::output();

    // 设置编译期常量供代码使用
    println!("cargo:rustc-cfg=esp32s3");

    // ---- feature 互斥校验 ----
    // 这些组合不允许, 编译时直接报错 (避免运行时诡异行为)
    check_feature_compatibility();

    // ---- 通过 env 让源码感知 feature ----
    if std::env::var("CARGO_FEATURE_BLE_MESH").is_ok() {
        println!("cargo:rustc-cfg=feature_ble_mesh");
    }
    if std::env::var("CARGO_FEATURE_ETHERNET_W5500").is_ok() {
        println!("cargo:rustc-cfg=feature_ethernet");
    }
    if std::env::var("CARGO_FEATURE_MODBUS_RTU").is_ok() {
        println!("cargo:rustc-cfg=feature_modbus_rtu");
    }
    if std::env::var("CARGO_FEATURE_MODBUS_TCP").is_ok() {
        println!("cargo:rustc-cfg=feature_modbus_tcp");
    }
    // ADC Continuous/DMA 模式 (ESP32-S3 硬件加速)
    if std::env::var("CARGO_FEATURE_ADC_CONTINUOUS").is_ok() {
        println!("cargo:rustc-cfg=feature_adc_continuous");
    }
    // Wi-Fi 支持 (ESP32-S3 内置)
    if std::env::var("CARGO_FEATURE_WIFI").is_ok() {
        println!("cargo:rustc-cfg=feature_wifi");
    }
    // GPIO 直驱 DI/DO (默认版本). 实际硬件 DI/DO 走 PCA9555, 需禁用此 feature
    if std::env::var("CARGO_FEATURE_IO_DI_DO").is_ok() {
        println!("cargo:rustc-cfg=feature_io_di_do");
    }
    // 硬件版本 F3/F4 (I2C MCP23017 扩展)
    if std::env::var("CARGO_FEATURE_F3").is_ok() {
        println!("cargo:rustc-cfg=feature_f3");
    }
    if std::env::var("CARGO_FEATURE_F4").is_ok() {
        println!("cargo:rustc-cfg=feature_f4");
    }
}

/// 校验 feature 组合是否合法, 不合法则 panic 中断编译
fn check_feature_compatibility() {
    // 1. modbus-rtu 与 modbus-tcp 不能同时关闭 (至少一个通信)
    let has_rtu = std::env::var("CARGO_FEATURE_MODBUS_RTU").is_ok();
    let has_tcp = std::env::var("CARGO_FEATURE_MODBUS_TCP").is_ok();
    if !has_rtu && !has_tcp {
        panic!(
            "feature error: 至少启用一个 Modbus 通信通道 (modbus-rtu 或 modbus-tcp). \
             建议 `--features modbus-rtu,modbus-tcp` 或 `--all-features`"
        );
    }

    // 2. ethernet-w5500 与 wifi 同时关闭 (允许, 但至少一个网络接口)
    let has_eth = std::env::var("CARGO_FEATURE_ETHERNET_W5500").is_ok();
    let has_wifi = std::env::var("CARGO_FEATURE_WIFI").is_ok();
    if !has_eth && !has_wifi {
        println!(
            "cargo:warning=warning: 未启用任何网络接口 (ethernet-w5500 和 wifi 均关闭), \
             仅 BLE 通信可用"
        );
    }

    // 3. wifi 与 ble-mesh 共存需要 ESP32-S3 内置共存 (sdkconfig 已配 COEX)
    if has_wifi && std::env::var("CARGO_FEATURE_BLE_MESH").is_ok() {
        println!(
            "cargo:warning=info: Wi-Fi 与 BLE Mesh 共存 (依赖 CONFIG_ESP_COEX_SW_COEXIST_ENABLE)"
        );
    }

    // 4. 硬件版本 F3/F4 互斥
    let has_f3 = std::env::var("CARGO_FEATURE_F3").is_ok();
    let has_f4 = std::env::var("CARGO_FEATURE_F4").is_ok();
    if has_f3 && has_f4 {
        panic!(
            "feature error: F3 与 F4 互斥, 不能同时启用. \
             建议 `--features f3` 或 `--features f4` (单选)"
        );
    }
    // 具体硬件版本已由 feature_f3/feature_f4 cfg 表达，不用 cargo:warning 输出普通信息。
}
