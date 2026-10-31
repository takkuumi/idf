#!/usr/bin/env python3
"""Check storage tuning in defaults and explicitly supplied generated sdkconfigs."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Disabled options must be present as either CONFIG_x=n or # CONFIG_x is not set.
# Requiring their presence also catches options silently dropped by Kconfig.
EXPECTED = {
    "CONFIG_ESPTOOLPY_FLASHMODE_DIO": "y",
    "CONFIG_ESPTOOLPY_FLASHFREQ_40M": "y",
    "CONFIG_ESPTOOLPY_FLASHSIZE_8MB": "y",
    "CONFIG_FREERTOS_HZ": "1000",
    "CONFIG_NVS_ASSERT_ERROR_CHECK": "n",
    "CONFIG_NVS_ALLOCATE_CACHE_IN_SPIRAM": "n",
    "CONFIG_SPI_FLASH_BYPASS_BLOCK_ERASE": "n",
    "CONFIG_SPI_FLASH_PLACE_FUNCTIONS_IN_IRAM": "y",
    "CONFIG_SPI_FLASH_YIELD_DURING_ERASE": "y",
    "CONFIG_SPI_FLASH_ERASE_YIELD_DURATION_MS": "20",
    "CONFIG_SPI_FLASH_ERASE_YIELD_TICKS": "1",
    "CONFIG_SPI_FLASH_WRITE_CHUNK_SIZE": "8192",
    "CONFIG_SPI_FLASH_AUTO_SUSPEND": "n",
}


def check(path: Path) -> list[str]:
    settings: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if match := re.fullmatch(r"(CONFIG_\w+)=(.*)", line.strip()):
            settings[match[1]] = match[2]
        elif match := re.fullmatch(r"# (CONFIG_\w+) is not set", line.strip()):
            settings[match[1]] = "n"
    errors = [
        f"{key}: expected {expected}, got {settings.get(key, '<missing>')}"
        for key, expected in EXPECTED.items()
        if settings.get(key) != expected
    ]
    if "CONFIG_SPI_FLASH_USE_4BLOCK_ERASE" in settings:
        errors.append("obsolete CONFIG_SPI_FLASH_USE_4BLOCK_ERASE must be removed")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--generated", type=Path, nargs="+", required=True,
        help="Explicit generated out/sdkconfig paths; never checks defaults alone.",
    )
    args = parser.parse_args()
    failed = False
    for path in [ROOT / "sdkconfig.defaults", *args.generated]:
        try:
            errors = check(path)
        except OSError as error:
            errors = [str(error)]
        if errors:
            failed = True
            print(f"FAIL {path}")
            for error in errors:
                print(f"  {error}")
        else:
            print(f"PASS {path} ({len(EXPECTED)} storage/configuration checks)")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
