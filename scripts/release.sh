#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VARIANT_ARG="${1:-all}"
case "$VARIANT_ARG" in
    all) VARIANTS=(default f3 f4) ;;
    default|f3|f4) VARIANTS=("$VARIANT_ARG") ;;
    f16) VARIANTS=(default) ;;
    *) echo "错误: 仅支持 all、default/f16、f3 或 f4" >&2; exit 2 ;;
esac

for command_name in cargo espflash python3 git; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "错误: 缺少发布依赖: ${command_name}" >&2
        exit 1
    fi
done
python3 -m esptool version >/dev/null

read_version() {
    python3 - "$1" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
if not match:
    raise SystemExit(f"无法从 {sys.argv[1]} 读取版本")
print(match.group(1))
PY
}

VERSION="$(read_version Cargo.toml)"
SDK_VERSION="$(sed -n 's/^CONFIG_APP_PROJECT_VER="\([^"]*\)"$/\1/p' sdkconfig.defaults)"
if [[ ! "$VERSION" =~ ^[0-9]\.[0-9]\.[0-9]$ ]]; then
    echo "错误: Cargo.toml 版本 ${VERSION} 不满足设备协议的 X.Y.Z 单数字约束" >&2
    exit 1
fi
if [[ "$SDK_VERSION" != "$VERSION" ]]; then
    echo "错误: Cargo.toml (${VERSION}) 与 sdkconfig.defaults (${SDK_VERSION}) 版本不一致" >&2
    echo "请运行: just set-version ${VERSION}" >&2
    exit 1
fi

GIT_COMMIT="$(git rev-parse HEAD)"
GIT_DIRTY=false
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
    GIT_DIRTY=true
fi
if [[ "$GIT_DIRTY" == true && "${ALLOW_DIRTY_RELEASE:-0}" != 1 ]]; then
    echo "错误: 正式发布要求 Git 工作区干净，避免产物无法追溯。" >&2
    echo "提交改动后重试；仅开发验证可使用 ALLOW_DIRTY_RELEASE=1 just release。" >&2
    exit 1
fi

RELEASE_ROOT="$ROOT_DIR/release"
FINAL_DIR="$RELEASE_ROOT/v${VERSION}"
TEMP_DIR="$RELEASE_ROOT/.v${VERSION}.tmp.$$"
mkdir -p "$RELEASE_ROOT"
if [[ -e "$FINAL_DIR" && "${OVERWRITE_RELEASE:-0}" != 1 ]]; then
    echo "错误: 发布目录已存在: ${FINAL_DIR}" >&2
    echo "版本化产物默认不可覆盖。确认重建时使用 OVERWRITE_RELEASE=1。" >&2
    exit 1
fi
rm -rf "$TEMP_DIR"
mkdir -p "$TEMP_DIR"
trap 'rm -rf "$TEMP_DIR"' EXIT

build_variant() {
    local variant="$1"
    local variant_upper
    variant_upper="$(printf '%s' "$variant" | tr '[:lower:]' '[:upper:]')"
    local target_dir="$ROOT_DIR/target/release-${variant}"
    local output_dir="$TEMP_DIR/$variant"
    local flash_dir="$output_dir/flash"
    local prefix="gateway-v${VERSION}-${variant}"

    echo ""
    echo "=== 完整编译 ${variant_upper} / v${VERSION} ==="
    cargo clean --target-dir "$target_dir"
    if [[ "$variant" == default || "$variant" == f16 ]]; then
        CARGO_TARGET_DIR="$target_dir" cargo build \
            --release --locked --bin gateway
    else
        CARGO_TARGET_DIR="$target_dir" cargo build \
            --release --locked --bin gateway --features "$variant"
    fi

    local elf="$target_dir/xtensa-esp32s3-espidf/release/gateway"
    if [[ ! -f "$elf" ]]; then
        echo "错误: 未找到 ${variant_upper} ELF: $elf" >&2
        exit 1
    fi

    local resolved=()
    while IFS= read -r path; do resolved+=("$path"); done < <(
        python3 - "$target_dir" <<'PY'
import json
import sys
from pathlib import Path

target = Path(sys.argv[1])
candidates = list(target.glob("**/out/build/flasher_args.json"))
if len(candidates) != 1:
    raise SystemExit(f"错误: 期望唯一 flasher_args.json，实际 {len(candidates)} 个")
manifest = candidates[0]
data = json.loads(manifest.read_text(encoding="utf-8"))
expected = {
    "0x0": "bootloader",
    "0x8000": "partition-table",
    "0x10000": "otadata",
    "0x20000": "app",
}
files = data.get("flash_files", {})
if set(files) != set(expected):
    raise SystemExit(f"错误: 非预期烧录地址: {sorted(files)}")
settings = data.get("flash_settings", {})
if settings.get("flash_mode") != "dio" or settings.get("flash_size", "").lower() != "8mb" or settings.get("flash_freq") not in ("40m", "40mhz"):
    raise SystemExit(f"错误: Flash 参数偏离 DIO/40MHz/8MB: {settings}")
base = manifest.parent
paths = [manifest]
for offset in ("0x0", "0x8000", "0x10000"):
    path = (base / files[offset]).resolve()
    if not path.is_file():
        raise SystemExit(f"错误: 缺少 {offset} 构建段: {path}")
    paths.append(path)
for path in paths:
    print(path)
PY
    )
    if [[ "${#resolved[@]}" -ne 4 ]]; then
        echo "错误: 无法解析 ESP-IDF 烧录产物" >&2
        exit 1
    fi

    mkdir -p "$flash_dir"
    cp "${resolved[1]}" "$flash_dir/${prefix}-bootloader.bin"
    cp "${resolved[2]}" "$flash_dir/${prefix}-partition-table.bin"
    cp "${resolved[3]}" "$flash_dir/${prefix}-ota-data-initial.bin"
    cp "$elf" "$output_dir/${prefix}.elf"

    local app="$flash_dir/${prefix}-app.bin"
    ESPFLASH_SKIP_UPDATE_CHECK=true espflash save-image \
        --chip esp32s3 --flash-mode dio --flash-freq 40mhz --flash-size 8mb \
        "$elf" "$app"
    python3 -m esptool image_info "$app" > "$output_dir/app-image-info.txt"

    python3 - "$app" "$VERSION" <<'PY'
import struct
import sys
from pathlib import Path

path = Path(sys.argv[1])
expected_version = sys.argv[2]
data = path.read_bytes()
factory_size = 0x240000
if len(data) > factory_size:
    raise SystemExit(
        f"错误: app 大小 {len(data)} 超过 factory 分区 {factory_size} 字节"
    )
if len(data) < 0x50 or struct.unpack_from("<I", data, 0x20)[0] != 0xABCD5432:
    raise SystemExit("错误: app 缺少有效的 ESP-IDF 应用描述符")
embedded_version = data[0x30:0x50].split(b"\0", 1)[0].decode("ascii")
if embedded_version != expected_version:
    raise SystemExit(
        f"错误: app 内嵌版本 {embedded_version!r} 与发布版本 {expected_version!r} 不一致"
    )
print(f"app 容量校验: {len(data)}/{factory_size} bytes ({len(data) / factory_size:.1%})")
PY

    local factory_image="$output_dir/${prefix}-factory-new-device.bin"
    python3 -m esptool --chip esp32s3 merge_bin \
        --output "$factory_image" --flash_mode dio --flash_freq 40m --flash_size 8MB \
        0x0 "$flash_dir/${prefix}-bootloader.bin" \
        0x8000 "$flash_dir/${prefix}-partition-table.bin" \
        0x10000 "$flash_dir/${prefix}-ota-data-initial.bin" \
        0x20000 "$app"

    cat > "$output_dir/flash-command.txt" <<EOF
# 推荐：四段独立烧录，不覆盖业务 NVS。先设置串口，例如:
# export ESPFLASH_PORT=/dev/cu.usbserial-1430
python3 -m esptool --chip esp32s3 --port "\${ESPFLASH_PORT:?请设置 ESPFLASH_PORT}" --baud 460800 write_flash \\
  --flash_mode dio --flash_freq 40m --flash_size 8MB \\
  0x0 flash/${prefix}-bootloader.bin \\
  0x8000 flash/${prefix}-partition-table.bin \\
  0x10000 flash/${prefix}-ota-data-initial.bin \\
  0x20000 flash/${prefix}-app.bin

# factory-new-device.bin 仅用于新设备/全量产烧录；从 0x0 写入会擦写间隙中的 NVS。
# python3 -m esptool --chip esp32s3 --port "\$ESPFLASH_PORT" write_flash 0x0 ${prefix}-factory-new-device.bin
EOF

    python3 - "$output_dir" "$variant" "$VERSION" "$GIT_COMMIT" "$GIT_DIRTY" <<'PY'
import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

out = Path(sys.argv[1])
variant, version, commit = sys.argv[2:5]
dirty = sys.argv[5] == "true"
prefix = f"gateway-v{version}-{variant}"

def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

segments = []
for role, offset, filename in (
    ("bootloader", "0x0", f"flash/{prefix}-bootloader.bin"),
    ("partition-table", "0x8000", f"flash/{prefix}-partition-table.bin"),
    ("ota-data-initial", "0x10000", f"flash/{prefix}-ota-data-initial.bin"),
    ("app", "0x20000", f"flash/{prefix}-app.bin"),
):
    path = out / filename
    segments.append({
        "role": role,
        "offset": offset,
        "file": filename,
        "size_bytes": path.stat().st_size,
        "sha256": sha256(path),
    })

manifest = {
    "schema_version": 1,
    "product": "esp32s3-iot-gateway",
    "firmware_version": version,
    "hardware_variant": variant.upper(),
    "cargo_features": ["default", variant],
    "built_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    "git": {"commit": commit, "dirty": dirty},
    "flash": {
        "chip": "esp32s3",
        "mode": "dio",
        "frequency": "40MHz",
        "size": "8MB",
        "segments": segments,
    },
    "factory_image": {
        "file": f"{prefix}-factory-new-device.bin",
        "offset": "0x0",
        "new_devices_only": True,
        "sha256": sha256(out / f"{prefix}-factory-new-device.bin"),
    },
    "elf": {
        "file": f"{prefix}.elf",
        "sha256": sha256(out / f"{prefix}.elf"),
    },
}
(out / "manifest.json").write_text(
    json.dumps(manifest, ensure_ascii=True, indent=2) + "\n",
    encoding="utf-8",
)

checksum_lines = []
for path in sorted(out.rglob("*"), key=lambda item: item.relative_to(out).as_posix()):
    if path.is_file() and path.name != "SHA256SUMS":
        relative = path.relative_to(out).as_posix()
        checksum_lines.append(f"{sha256(path)}  ./{relative}")
(out / "SHA256SUMS").write_text(
    "\n".join(checksum_lines) + "\n",
    encoding="utf-8",
)
PY

    python3 - "$output_dir" <<'PY'
import hashlib
import sys
from pathlib import Path

out = Path(sys.argv[1])
for line in (out / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
    expected, relative = line.split("  ", 1)
    path = out / relative.removeprefix("./")
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != expected:
        raise SystemExit(f"错误: SHA-256 校验失败: {relative}")
    print(f"{relative}: OK")
PY
}

for variant in "${VARIANTS[@]}"; do
    build_variant "$variant"
done

if [[ -e "$FINAL_DIR" ]]; then
    rm -rf "$FINAL_DIR"
fi
mv "$TEMP_DIR" "$FINAL_DIR"
trap - EXIT

echo ""
echo "发布完成: $FINAL_DIR"
for variant in "${VARIANTS[@]}"; do
    variant_upper="$(printf '%s' "$variant" | tr '[:lower:]' '[:upper:]')"
    echo "  ${variant_upper}: $FINAL_DIR/$variant"
done
