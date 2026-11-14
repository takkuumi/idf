#!/usr/bin/env python3
"""Execute production UART timing functions, including the 9600-baud regression."""
from pathlib import Path
import subprocess,tempfile
ROOT=Path(__file__).resolve().parents[2]
s=(ROOT/'src/rs485/port.rs').read_text()
def item(marker):
    a=s.index(marker);b=s.index('{',a);depth=1;end=b+1
    while depth:
        depth+=(s[end]=='{')-(s[end]=='}');end+=1
    return s[a:end]
helpers='\n'.join(item(m) for m in ['fn ms_to_ticks(','fn rtu_silence_us(','fn rtu_silence_timeout_ms(','fn tx_guard_ms('])
module=ROOT/'src/rs485/timing.rs'
timing=f'#[path="{module}"] pub mod timing;' if module.exists() else ''
program='''#![allow(dead_code,non_upper_case_globals)]
mod rs485 {
'''+timing+'''mod port_timing {
const portTICK_PERIOD_MS:u32=1;
'''+helpers+'''
#[test] fn actual_9600_silence_is_five_milliseconds_not_four_seconds() { assert_eq!(rtu_silence_timeout_ms(9600),5); }
#[test] fn actual_transmit_guard_and_ticks_remain_bounded() {assert_eq!(tx_guard_ms(1200),20);assert_eq!(tx_guard_ms(9600),4);assert_eq!(tx_guard_ms(115200),3);assert_eq!(ms_to_ticks(0),1);}
}}
'''
with tempfile.TemporaryDirectory(prefix='imc-uart-timing-') as name:
    p=Path(name);(p/'tests.rs').write_text(program)
    subprocess.run(['rustc','+stable','--edition=2024','--test',str(p/'tests.rs'),'-o',str(p/'tests')],check=True)
    subprocess.run([str(p/'tests')],check=True,timeout=30)
