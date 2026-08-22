#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VARIANT="${1:-}"
SKIP_BUILD=0
if [[ "${2:-}" == "--skip-build" ]]; then
    SKIP_BUILD=1
elif [[ $# -gt 1 ]]; then
    echo "用法: $0 <default|f16|f3|f4> [--skip-build]" >&2
    exit 2
fi

case "$VARIANT" in
    f16) VARIANT=default ;;
    default|f3|f4) ;;
    *)
        echo "错误: 必须明确选择硬件型号 default/f16、f3 或 f4，避免刷入错误固件。" >&2
        echo "用法: just release-flash default" >&2
        exit 2
        ;;
esac
VARIANT_UPPER="$(printf '%s' "$VARIANT" | tr '[:lower:]' '[:upper:]')"

for command_name in python3 git; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "错误: 缺少依赖: ${command_name}" >&2
        exit 1
    fi
done

if [[ "$SKIP_BUILD" != 1 ]] && ! command -v cargo >/dev/null 2>&1; then
    echo "错误: 缺少依赖: cargo" >&2
    exit 1
fi

if ! python3 -m esptool version >/dev/null 2>&1; then
    echo "错误: 未安装可用的 esptool，请运行: python3 -m pip install esptool" >&2
    exit 1
fi

PORT="${ESPFLASH_PORT:-/dev/cu.usbserial-1430}"
BAUD="${ESPFLASH_BAUD:-460800}"
# 该设备的 CH340 自动复位可能失败。默认要求手动进入下载模式，
# 可通过 ESPFLASH_BEFORE=default_reset 使用自动复位。
BEFORE="${ESPFLASH_BEFORE:-no_reset}"
AFTER="${ESPFLASH_AFTER:-hard_reset}"
ALLOW_STALE_RELEASE="${ALLOW_STALE_RELEASE:-0}"
ALLOW_DIRTY_RELEASE="${ALLOW_DIRTY_RELEASE:-0}"

if [[ "$SKIP_BUILD" != 1 ]]; then
    echo "=== 编译 ${VARIANT_UPPER} 发布固件 ==="
    # 同一版本反复验证/刷入时，发布目录必须由本次构建原子替换。
    # ALLOW_DIRTY_RELEASE 的安全检查仍由 release.sh 保留。
    OVERWRITE_RELEASE=1 ./scripts/release.sh "$VARIANT"
fi

VERSION="$(python3 - "$ROOT_DIR/Cargo.toml" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
if not match:
    raise SystemExit("无法从 Cargo.toml 读取固件版本")
print(match.group(1))
PY
)"
OUT_DIR="$ROOT_DIR/release/v${VERSION}/${VARIANT}"
PREFIX="gateway-v${VERSION}-${VARIANT}"
CURRENT_COMMIT="$(git rev-parse HEAD)"
CURRENT_DIRTY=false
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
    CURRENT_DIRTY=true
fi

if [[ ! -d "$OUT_DIR" ]]; then
    echo "错误: 未找到发布产物: $OUT_DIR" >&2
    echo "请先运行 just release-flash ${VARIANT}，或确认版本目录未被删除。" >&2
    exit 1
fi

echo "=== 校验 ${VARIANT_UPPER} 发布产物 ==="
python3 - "$OUT_DIR" "$VARIANT" "$VERSION" "$CURRENT_COMMIT" "$CURRENT_DIRTY" "$ALLOW_STALE_RELEASE" "$ALLOW_DIRTY_RELEASE" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])
variant, version, current_commit, current_dirty, allow_stale, allow_dirty = sys.argv[2:8]
manifest_path = out / "manifest.json"
checksums_path = out / "SHA256SUMS"
if not manifest_path.is_file() or not checksums_path.is_file():
    raise SystemExit("错误: 发布产物缺少 manifest.json 或 SHA256SUMS")

manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if manifest.get("firmware_version") != version:
    raise SystemExit("错误: manifest 固件版本与 Cargo.toml 不一致")
if manifest.get("hardware_variant") != variant.upper():
    raise SystemExit("错误: manifest 硬件型号与目标不一致")
git_info = manifest.get("git", {})
if git_info.get("commit") != current_commit and allow_stale != "1":
    raise SystemExit(
        "错误: 发布包不是当前 Git 提交生成，拒绝刷入旧固件。"
        "如确认要刷旧包，请设置 ALLOW_STALE_RELEASE=1。"
    )
if (git_info.get("dirty") is True or current_dirty == "true") and allow_dirty != "1":
    raise SystemExit(
        "错误: 只允许刷入干净工作区生成的发布包。"
        "开发验证请显式设置 ALLOW_DIRTY_RELEASE=1。"
    )
flash = manifest.get("flash", {})
if (flash.get("chip"), flash.get("mode"), flash.get("frequency"), flash.get("size")) != (
    "esp32s3", "dio", "40MHz", "8MB"
):
    raise SystemExit(f"错误: manifest Flash 参数不符合 ESP32-S3/DIO/40MHz/8MB: {flash}")

expected = [
    ("bootloader", "0x0"),
    ("partition-table", "0x8000"),
    ("ota-data-initial", "0x10000"),
    ("app", "0x20000"),
]
segments = flash.get("segments", [])
if [(item.get("role"), item.get("offset")) for item in segments] != expected:
    raise SystemExit("错误: 发布产物烧录段或地址不符合固定布局")

def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

for item in segments:
    path = out / item["file"]
    if not path.is_file():
        raise SystemExit(f"错误: 缺少烧录段: {path}")
    if sha256(path) != item.get("sha256"):
        raise SystemExit(f"错误: manifest SHA-256 校验失败: {path}")

for line in checksums_path.read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    expected_hash, relative = line.split("  ", 1)
    path = out / relative.removeprefix("./")
    if not path.is_file() or sha256(path) != expected_hash:
        raise SystemExit(f"错误: SHA-256SUMS 校验失败: {relative}")
print("产物清单、烧录地址和 SHA-256 校验通过")
PY

BOOTLOADER="$OUT_DIR/flash/${PREFIX}-bootloader.bin"
PARTITION="$OUT_DIR/flash/${PREFIX}-partition-table.bin"
OTADATA="$OUT_DIR/flash/${PREFIX}-ota-data-initial.bin"
APP="$OUT_DIR/flash/${PREFIX}-app.bin"

if [[ "${RELEASE_FLASH_DRY_RUN:-0}" == 1 ]]; then
    echo "=== dry-run：跳过设备连接和烧录 ==="
    printf 'python3 -m esptool --chip esp32s3 --port %q --baud %q --before %q --after %q write_flash --verify --flash_mode dio --flash_freq 40m --flash_size 8MB\n' \
        "$PORT" "$BAUD" "$BEFORE" "$AFTER"
    printf '  0x0 %q 0x8000 %q 0x10000 %q 0x20000 %q\n' \
        "$BOOTLOADER" "$PARTITION" "$OTADATA" "$APP"
    exit 0
fi

if [[ ! -e "$PORT" ]]; then
    echo "错误: 串口不存在: $PORT" >&2
    echo "请设置 ESPFLASH_PORT，例如: ESPFLASH_PORT=/dev/cu.usbserial-1430" >&2
    exit 1
fi

COMMON_ARGS=(--chip esp32s3 --port "$PORT" --baud "$BAUD" --before "$BEFORE" --connect-attempts 7)
echo "=== 检查设备芯片和 Flash ==="
CHIP_OUTPUT="$(python3 -m esptool "${COMMON_ARGS[@]}" --after no_reset chip_id 2>&1)" || {
    printf '%s\n' "$CHIP_OUTPUT" >&2
    echo "错误: 无法连接设备。请按住 BOOT，短按 RST，松开 BOOT 后重试；或设置 ESPFLASH_BEFORE=default_reset。" >&2
    exit 1
}
printf '%s\n' "$CHIP_OUTPUT"
if ! printf '%s\n' "$CHIP_OUTPUT" | grep -Eiq 'ESP32[- ]?S3'; then
    echo "错误: 连接的芯片不是 ESP32-S3，已停止刷写。" >&2
    exit 1
fi

FLASH_OUTPUT="$(python3 -m esptool "${COMMON_ARGS[@]}" --after no_reset flash_id 2>&1)" || {
    printf '%s\n' "$FLASH_OUTPUT" >&2
    echo "错误: 无法读取 Flash 信息，已停止刷写。" >&2
    exit 1
}
printf '%s\n' "$FLASH_OUTPUT"
if ! printf '%s\n' "$FLASH_OUTPUT" | grep -Eiq '8MB|8388608'; then
    echo "错误: 设备 Flash 不是 8MB，已停止刷写以避免分区不匹配。" >&2
    exit 1
fi

echo "=== 开始刷入 ${VARIANT_UPPER} v${VERSION}（保留业务 NVS，不执行全擦） ==="
python3 -m esptool "${COMMON_ARGS[@]}" --after "$AFTER" write_flash \
    --verify --flash_mode dio --flash_freq 40m --flash_size 8MB \
    0x0 "$BOOTLOADER" \
    0x8000 "$PARTITION" \
    0x10000 "$OTADATA" \
    0x20000 "$APP"

echo "刷入完成：${VARIANT_UPPER} v${VERSION}，设备将按 ${AFTER} 方式复位。"
