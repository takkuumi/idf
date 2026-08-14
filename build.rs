// 构建脚本：调用 embuild 编排 ESP-IDF
fn main() {
    embuild::espidf::sysenv::output();
    validate_production_envelope();
    emit_build_identity();

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

/// ESP-IDF 的 CMake 子构建可能复用旧 `PROJECT_VER` 缓存。Rust 主程序额外嵌入
/// 当前仓库提交号，确保现场日志能够准确追溯实际业务固件。
fn emit_build_identity() {
    use std::process::Command;

    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD")
        && let Some(reference) = head.trim().strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed=.git/{reference}");
    }

    let revision = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .is_some_and(|output| output.status.success() && !output.stdout.is_empty());
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=GATEWAY_BUILD_ID={revision}{suffix}");
}

/// Reject builds whose generated firmware would violate the verified hardware
/// and resource envelope. These are product invariants, not optional tuning.
fn validate_production_envelope() {
    const SDKCONFIG: &str = "sdkconfig.defaults";
    const PARTITIONS: &str = "partitions.csv";
    println!("cargo:rerun-if-changed={SDKCONFIG}");
    println!("cargo:rerun-if-changed={PARTITIONS}");

    let sdk = std::fs::read_to_string(SDKCONFIG)
        .unwrap_or_else(|e| panic!("cannot read {SDKCONFIG}: {e}"));
    require_config(&sdk, "CONFIG_ESP_MAIN_TASK_STACK_SIZE", "32768");
    require_config(&sdk, "CONFIG_SPIRAM_SIZE", "2097152");
    require_config(&sdk, "CONFIG_SPIRAM_TRY_ALLOCATE_WIFI_LWIP", "y");
    require_config(&sdk, "CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL", "65536");
    require_config(&sdk, "CONFIG_LWIP_MAX_SOCKETS", "20");
    require_config(&sdk, "CONFIG_ESPTOOLPY_FLASHMODE_DIO", "y");
    require_config(&sdk, "CONFIG_ESPTOOLPY_FLASHFREQ_40M", "y");
    require_config(&sdk, "CONFIG_ESPTOOLPY_FLASHSIZE_8MB", "y");
    require_config(&sdk, "CONFIG_APP_PROJECT_VER_FROM_CONFIG", "y");
    require_config(&sdk, "CONFIG_APP_PROJECT_VER", "\"2.2.1\"");

    let partitions = std::fs::read_to_string(PARTITIONS)
        .unwrap_or_else(|e| panic!("cannot read {PARTITIONS}: {e}"));
    require_partition(&partitions, "nvs", 0x9000, 0x6000);
    require_partition(&partitions, "factory", 0x20000, 0x240000);
    require_partition(&partitions, "ota_0", 0x260000, 0x240000);
    require_partition(&partitions, "ota_1", 0x4A0000, 0x240000);
}

fn require_config(config: &str, key: &str, expected: &str) {
    let actual = config.lines().find_map(|line| {
        let line = line.trim();
        (!line.starts_with('#'))
            .then(|| line.split_once('='))
            .flatten()
            .filter(|(name, _)| *name == key)
            .map(|(_, value)| value.trim())
    });
    assert_eq!(
        actual,
        Some(expected),
        "production envelope violation: {key} must be {expected}"
    );
}

fn require_partition(csv: &str, name: &str, expected_offset: u32, expected_size: u32) {
    let row = csv.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let mut fields = line.split(',').map(str::trim);
        (fields.next()? == name).then(|| {
            let _partition_type = fields.next()?;
            let _subtype = fields.next()?;
            let offset = parse_hex(fields.next()?)?;
            let size = parse_hex(fields.next()?)?;
            Some((offset, size))
        })?
    });
    assert_eq!(
        row,
        Some((expected_offset, expected_size)),
        "production partition violation: {name} must be at {expected_offset:#X}, size {expected_size:#X}"
    );
}

fn parse_hex(value: &str) -> Option<u32> {
    u32::from_str_radix(value.strip_prefix("0x")?, 16).ok()
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
    let _has_network = has_eth || has_wifi;

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
