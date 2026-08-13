# ESP32-S3 工业网关 — 跨平台烧录 Justfile
# 用法: just <recipe>  或  just --list
# 依赖: just (brew install just), espflash (cargo install espflash --locked), ldproxy, cargo
# 工具链: rustup + espup (Xtensa Rust toolchain)

# ───── 通用变量 ─────
export PORT      := env_var_or_default("ESPFLASH_PORT", "/dev/cu.usbserial-1430")
export SERIAL    := PORT
export ELF_DEBUG   := "target/xtensa-esp32s3-espidf/debug/gateway"
export ELF_RELEASE := "target/xtensa-esp32s3-espidf/release/gateway"
export BOOTLOADER_DEBUG := "target/xtensa-esp32s3-espidf/debug/bootloader.bin"
export BOOTLOADER_RELEASE := "target/xtensa-esp32s3-espidf/release/bootloader.bin"
export PARTITIONS := "partitions.csv"
export MONITOR_BAUD := "115200"
export TOOLCHAIN_PREFIX := "/Users/takumi/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20260121/xtensa-esp-elf/bin"
export XTENSA_GCC  := TOOLCHAIN_PREFIX + "/xtensa-esp-elf-gcc"

# ───── 默认入口: 显示帮助 ─────
default:
    @just --list

# ───── 0. 环境激活提示 ─────
env:
    #!/usr/bin/env bash
    echo "─── 激活 ESP-IDF/Rust 环境 ───"
    if [ -f "$HOME/export-esp.sh" ]; then
        echo "source $HOME/export-esp.sh     # Xtensa Rust toolchain"
    else
        echo "⚠ $HOME/export-esp.sh 不存在, 请先: cargo install espup && espup install --targets esp32s3 --toolchain-version 1.90.0.0"
    fi
    if [ -d "{{TOOLCHAIN_PREFIX}}" ]; then
        echo "export PATH={{TOOLCHAIN_PREFIX}}:\$PATH   # Xtensa GCC"
    else
        echo "⚠ Xtensa GCC 不在 {{TOOLCHAIN_PREFIX}}, 请安装 ESP-IDF: ./install.sh esp32s3"
    fi
    echo ""
    echo "─── 验证 ───"
    which ldproxy || cargo install ldproxy
    which espflash || cargo install espflash --locked

# ───── 1. Debug 构建 ─────
build:
    cargo build --bin gateway

# ───── 2. Release 构建 ─────
build-release:
    cargo build --bin gateway --release

# ───── 3. 列出可用串口 ─────
ports:
    espflash list-ports

# ───── 4. 只擦 flash (手动按 BOOT + RST 进下载模式后) ─────
erase:
    @echo "⚠ 如自动复位失败, 先按住 BOOT, 短按 RST, 松开 BOOT 再回车"
    @read _
    espflash erase-flash --port {{PORT}}

# ───── 5. 标准烧录 (调试版) — 需手动进下载模式 ─────
flash: build
    @echo "⚠ 手动进下载模式: 按住 BOOT, 短按 RST, 松开 BOOT"
    @echo "  等 1 秒, 看到 'ESP-ROM:' 提示后再烧录"
    @echo ""
    @read _
    espflash flash --port {{PORT}} --no-skip \
        --bootloader {{BOOTLOADER_DEBUG}} \
        --partition-table {{PARTITIONS}} --partition-table-offset 0x8000 \
        --target-app-partition factory --erase-parts otadata \
        --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
        {{ELF_DEBUG}}

# ───── 6. 标准烧录 (Release 版) ─────
flash-release: build-release
    @echo "⚠ 手动进下载模式: 按住 BOOT, 短按 RST, 松开 BOOT"
    @read _
    espflash flash --port {{PORT}} --no-skip \
        --bootloader {{BOOTLOADER_RELEASE}} \
        --partition-table {{PARTITIONS}} --partition-table-offset 0x8000 \
        --target-app-partition factory --erase-parts otadata \
        --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
        {{ELF_RELEASE}}

# ───── 7. 不擦直接重烧 (最快, 跳过已识别区块) ─────
flash-skip:
    @echo "⚠ 手动进下载模式: 按住 BOOT, 短按 RST, 松开 BOOT"
    @read _
    espflash flash --port {{PORT}} \
        --bootloader {{BOOTLOADER_DEBUG}} \
        --partition-table {{PARTITIONS}} --partition-table-offset 0x8000 \
        --target-app-partition factory --erase-parts otadata \
        --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
        {{ELF_DEBUG}}

# ───── 8. 烧录 + 立即监视 ─────
flash-monitor: build
    @echo "⚠ 手动进下载模式: 按住 BOOT, 短按 RST, 松开 BOOT"
    @read _
    espflash flash --port {{PORT}} --no-skip --monitor \
        --bootloader {{BOOTLOADER_DEBUG}} \
        --partition-table {{PARTITIONS}} --partition-table-offset 0x8000 \
        --target-app-partition factory --erase-parts otadata \
        --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
        {{ELF_DEBUG}}

# ───── 9. 只监视 (烧完后再开 monitor) ─────
monitor:
    espflash monitor --port {{PORT}} --monitor-baud {{MONITOR_BAUD}}

# ───── 10. 软复位 (不重新烧录) ─────
reset:
    espflash reset --port {{PORT}}

# ───── 11. 硬复位 (DTR 拉低 500ms 让 EN 断电) ─────
hard-reset:
    #!/usr/bin/env bash
    python3 -c "
    import serial, time
    ser = serial.Serial('{{PORT}}', 115200, timeout=1)
    ser.dtr = True; time.sleep(0.5); ser.dtr = False
    time.sleep(2)
    ser.close()
    print('✓ DTR 硬复位完成 (EN 引脚已断电 500ms)')
    "

# ───── 12. 一键完整烧录 (erase + flash + hard-reset + 找 IP) — 跨平台 ─────
full-flash: build
    #!/usr/bin/env bash
    set -e
    echo "═══ 步骤 1/5: 全擦 flash ═══"
    echo "  ⚠ 先手动进入下载模式: 按住 BOOT, 短按 RST, 松开 BOOT"
    echo ""
    read -p "按 Enter 继续 (或 Ctrl+C 取消) "
    espflash erase-flash --port {{PORT}}

    echo ""
    echo "═══ 步骤 2/5: 全擦完成, 重新进下载模式 ═══"
    echo "  ⚠ 再次按住 BOOT, 短按 RST, 松开 BOOT"
    echo ""
    read -p "按 Enter 继续 (或 Ctrl+C 取消) "
    espflash flash --port {{PORT}} --no-skip \
        --bootloader {{BOOTLOADER_DEBUG}} \
        --partition-table {{PARTITIONS}} --partition-table-offset 0x8000 \
        --target-app-partition factory \
        --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
        {{ELF_DEBUG}}

    echo ""
    echo "═══ 步骤 3/5: 硬复位 (DTR 拉低 500ms) ═══"
    python3 -c "
    import serial, time
    ser = serial.Serial('{{PORT}}', 115200, timeout=1)
    ser.dtr = True; time.sleep(0.5); ser.dtr = False
    time.sleep(15)
    ser.close()
    print('✓ DTR 复位完成, 设备冷启动 + DHCP 等待 15s')
    "

    echo ""
    echo "═══ 步骤 4/5: 读取 IP ═══"
    python3 -c "
    import serial, time, re
    ser = serial.Serial('{{PORT}}', 115200, timeout=1)
    end = time.time() + 10; buf = b''
    while time.time() < end:
        try: d = ser.read(4096); buf += d if d else b''
        except: pass
    ser.close()
    m = re.search(rb'ip: ([\d.]+)', buf)
    print(f'设备 IP: {m.group(1).decode() if m else \"NO IP (查看 monitor 确认启动状态)\"}')
    "

# ───── 13. 编译 + 强制重新链接 esp-idf-sys (烧录后 App version 还是旧) ─────
rebuild-sys:
    touch build.rs
    rm -rf target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-*
    cargo build --bin gateway

# ───── 14. 仅编译测试 binary ─────
test-compile:
    cargo test --bin gateway --no-run

# ───── 15. 显示烧录环境变量与工具版本 ─────
diag:
    #!/usr/bin/env bash
    echo "═══ 工具版本 ═══"
    cargo --version
    espflash --version
    ldproxy --version 2>&1 || echo "ldproxy 未安装 (运行: cargo install ldproxy)"
    which xtensa-esp32s3-elf-gcc || echo "Xtensa GCC 未在 PATH"
    xtensa-esp32s3-elf-gcc --version 2>&1 | head -1 || true
    rustc --version
    echo ""
    echo "═══ 串口 ═══"
    espflash list-ports
    echo ""
    echo "═══ 环境变量 ═══"
    echo "ESPFLASH_PORT = $ESPFLASH_PORT"
    echo "IDF_PATH      = ${IDF_PATH:-未设置}"
    echo "PATH 含 Xtensa GCC? "
    echo "$PATH" | tr ':' '\n' | grep -c esp-elf | xargs echo "  匹配数:"


# just flash — 烧 Debug 固件（按提示手动 BOOT+RST）
# just flash-monitor — 烧完直接看日志（Ctrl+R 复位，Ctrl+C 退出）
# just full-flash — 一键全擦+烧+硬复位+找 IP
# just diag — 排查环境问题
