#!/usr/bin/env python3
"""Compile and execute the production AI filter/normalisation code on host."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
config = (ROOT / 'src/config.rs').read_text()
start = config.index('pub mod ai_calib {')
end = config.index('\n}', start) + 2
with tempfile.TemporaryDirectory(prefix='imc-ai-transfer-') as directory:
    p = Path(directory)
    (p / 'tests.rs').write_text('#![allow(dead_code)]\nmod config {\n' + config[start:end] + '\n}\n' + f'#[path = "{ROOT / "src/channel/ai_transfer.rs"}"] mod ai_transfer;\n')
    subprocess.run(['rustc', '+stable', '--edition=2024', '--test', str(p / 'tests.rs'), '-o', str(p / 'tests')], check=True)
    subprocess.run([str(p / 'tests')], check=True, timeout=30)
