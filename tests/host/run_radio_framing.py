#!/usr/bin/env python3
"""Execute actual bounded LoRa RTU length framing on the host."""
from pathlib import Path
import subprocess,tempfile
root=Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix='imc-radio-framing-') as name:
    p=Path(name);(p/'tests.rs').write_text(f'#![allow(dead_code)]\n#[path="{root / "src/rs485/framing.rs"}"] mod framing;\n')
    subprocess.run(['rustc','+stable','--edition=2024','--test',str(p/'tests.rs'),'-o',str(p/'tests')],check=True)
    subprocess.run([str(p/'tests')],check=True,timeout=30)
