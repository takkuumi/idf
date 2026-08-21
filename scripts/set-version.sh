#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-}"

if [[ ! "$VERSION" =~ ^[0-9]\.[0-9]\.[0-9]$ ]]; then
    echo "错误: 版本必须为 X.Y.Z，且每段为 0-9（例如 2.2.3）" >&2
    echo "原因: 设备协议将版本编码为三位十进制数，不能无损表达多位版本段。" >&2
    exit 2
fi

python3 - "$ROOT_DIR" "$VERSION" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
version = sys.argv[2]

def replace_exact(path: Path, pattern: str, replacement: str, label: str) -> None:
    original = path.read_text(encoding="utf-8")
    updated, count = re.subn(pattern, replacement, original, count=1, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"错误: {path} 中未唯一匹配 {label}")
    path.write_text(updated, encoding="utf-8")

replace_exact(
    root / "Cargo.toml",
    r'^(version\s*=\s*)"[^"]+"',
    rf'\g<1>"{version}"',
    "[package].version",
)
replace_exact(
    root / "sdkconfig.defaults",
    r'^CONFIG_APP_PROJECT_VER="[^"]+"$',
    f'CONFIG_APP_PROJECT_VER="{version}"',
    "CONFIG_APP_PROJECT_VER",
)

lock = root / "Cargo.lock"
if lock.exists():
    text = lock.read_text(encoding="utf-8")
    pattern = r'(?ms)(^name = "esp32s3-iot-gateway"\nversion = ")[^"]+("$)'
    text, count = re.subn(pattern, rf'\g<1>{version}\g<2>', text, count=1)
    if count != 1:
        raise SystemExit("错误: Cargo.lock 中未唯一匹配本地包版本")
    lock.write_text(text, encoding="utf-8")
PY

echo "固件版本已更新为 ${VERSION}: Cargo.toml + sdkconfig.defaults"
echo "请提交版本变更后运行: just release"
