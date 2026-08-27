#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ELF="${1:-}"
if [[ -z "$ELF" ]]; then
    echo "错误: Cargo runner 未收到固件 ELF。" >&2
    exit 2
fi
shift

# Cargo test 的目标位于 deps/gateway-<hash>。它是 libtest 运行器，不是设备应用；
# 刷入后会在 app_main 中尝试列出测试并立即 abort。
if [[ "$ELF" == */deps/* || "$(basename "$ELF")" != "gateway" ]]; then
    echo "错误: 拒绝把非应用目标刷入设备: $ELF" >&2
    echo "测试只允许编译检查: cargo test --bin gateway --no-run" >&2
    exit 2
fi
if strings "$ELF" | grep -Fq "io error when listing tests"; then
    echo "错误: ELF 包含 Rust 测试运行器，拒绝刷入设备: $ELF" >&2
    exit 2
fi
if [[ $# -ne 0 ]]; then
    echo "错误: 固件不接受 cargo run 的程序参数。" >&2
    exit 2
fi

PROFILE_DIR="$(cd "$(dirname "$ELF")" && pwd)"
BOOTLOADER="$PROFILE_DIR/bootloader.bin"
PARTITIONS="$ROOT_DIR/partitions.csv"
PORT="${ESPFLASH_PORT:-/dev/cu.usbserial-1430}"

for required in "$ELF" "$BOOTLOADER" "$PARTITIONS"; do
    if [[ ! -f "$required" ]]; then
        echo "错误: 缺少完整烧录所需文件: $required" >&2
        exit 1
    fi
done

COMMAND=(
    espflash flash
    --port "$PORT"
    --before no-reset
    --no-skip
    --monitor
    --bootloader "$BOOTLOADER"
    --partition-table "$PARTITIONS"
    --partition-table-offset 0x8000
    --target-app-partition factory
    --erase-parts otadata
    --flash-mode dio
    --flash-freq 40mhz
    --flash-size 8mb
    "$ELF"
)

if [[ "${GATEWAY_RUNNER_DRY_RUN:-0}" == 1 ]]; then
    printf '%q %q\n' "$ROOT_DIR/scripts/enter-bootloader.sh" "$PORT"
    printf '%q ' "${COMMAND[@]}"
    printf '\n'
    exit 0
fi

cd "$ROOT_DIR"
"$ROOT_DIR/scripts/enter-bootloader.sh" "$PORT"
exec "${COMMAND[@]}"
