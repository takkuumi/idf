#!/usr/bin/env python3
"""Execute actual firmware D0 store/D1 read/runtime parser contracts on host.

Only HAL/NVS are stubbed. This is not a physical durability or UART test.
"""
from __future__ import annotations

import os
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def item(text: str, marker: str) -> str:
    """Extract the actual brace-balanced production item; fail on source drift."""
    start = text.index(marker)
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


logic = (ROOT / 'src/ble_at/logic_handlers.rs').read_text()
master = (ROOT / 'src/modbus/rtu_master.rs').read_text()
source = r'''
#![allow(dead_code)]
extern crate self as log;
#[macro_export] macro_rules! error { ($($arg:tt)*) => {{ let _ = format_args!($($arg)*); }}; }
mod heapless {
 pub struct Vec<T,const N:usize>(std::vec::Vec<T>);
 impl<T,const N:usize> Vec<T,N> {
  pub fn new()->Self { Self(std::vec::Vec::new()) }
  pub fn push(&mut self,t:T)->Result<(),T>{ if self.0.len()==N {Err(t)}else{self.0.push(t);Ok(())} }
  pub fn extend_from_slice(&mut self,v:&[T])->Result<(),()> where T:Clone { if self.0.len()+v.len()>N {Err(())}else{self.0.extend_from_slice(v);Ok(())} }
 }
 impl<T,const N:usize> std::ops::Deref for Vec<T,N>{type Target=std::vec::Vec<T>; fn deref(&self)->&Self::Target{&self.0}}
 impl<T,const N:usize> std::ops::DerefMut for Vec<T,N>{fn deref_mut(&mut self)->&mut Self::Target{&mut self.0}}
}
use heapless::Vec as HVec;
static HOLDING:std::sync::Mutex<[u16;2048]>=std::sync::Mutex::new([0;2048]);
__ACTUAL_CONFIG__
mod bus { pub mod backends {
 pub fn arm_logic_write(){} pub fn disarm_logic_write(){}
 pub fn write_hold_regs(addr:u16,words:&[u16])->bool { let i=(addr-2176)as usize; crate::HOLDING.lock().unwrap()[i..i+words.len()].copy_from_slice(words); true }
 pub fn read_hold_reg(addr:u16)->Option<u16>{crate::HOLDING.lock().unwrap().get(addr.checked_sub(2176)?as usize).copied()}
}}
mod device {pub mod holding_store {pub fn save_to_nvs()->Result<(),String>{Ok(())}}}
const MAX_POLL_ITEMS:usize=64; const MONITOR_RESULT_COUNT:usize=128; const USER_RESULT_BASE:u16=4000; const USER_RESULT_COUNT:usize=224;
'''
source = source.replace('__ACTUAL_CONFIG__', '#[path = \"' + str(ROOT/'src/config.rs') + '\"] mod config;')
source += '\n#[derive(Clone,Copy,Debug)]\n'+item(master,'pub(crate) struct PollItem ')
source += '\n'+'\n'.join(item(master,'fn '+name+'(') for name in ['holding_word_at','holding_word','result_index','result_range_is_valid','parse_poll_table','parse_poll_table_at'])
source += '\n'+item(logic,'fn store_device_function_config(')+'\n'+item(logic,'fn load_device_function_config(')
source += r'''
const ANALOG:[u8;22]=[0,3,0,0,0,0,0,0,12,1,0,0,0,0,0,0,15,255,0,0,0,0];
const RS485:[u8;24]=[0,2,0,0,0,0,0,0,1,1,6,1,0,3,16,0,15,160,4,1,0,0,16,0];
fn packet(records:&[&[u8]])->std::vec::Vec<u8>{
 let mut bytes=(records.len() as u16).to_le_bytes().to_vec();
 for record in records{bytes.push((record.len()+1)as u8);bytes.extend_from_slice(record);}
 bytes
}
fn reset(){*HOLDING.lock().unwrap()=[0;2048];}
fn main(){
 let offset=(2300-2176)as usize;
 reset();
 let one=packet(&[&ANALOG]); assert!(store_device_function_config(&one));
 assert_eq!(load_device_function_config().unwrap().as_slice(),one.as_slice());
 let stored=*HOLDING.lock().unwrap();
 assert_eq!(&stored[offset..offset+4],&[1,3,14,0x0003]);
 assert!(parse_poll_table(&stored).is_ok());
 println!("PASS D0/D1 analog roundtrip and actual runtime layout (relative offset=3, header=0003)");
 reset();
 let data=packet(&[&RS485]); assert!(store_device_function_config(&data));
 let stored=*HOLDING.lock().unwrap(); let polls=parse_poll_table(&stored).unwrap();
 let terminal=stored[offset+2] as usize;
 let staged=&stored[offset..offset+terminal];
 let staged_summary=parse_poll_table_at(staged,0,0,(4000-2300) as u16).unwrap();
 assert_eq!(staged_summary.len(),1);
 assert_eq!(polls.len(),1);
 assert_eq!((polls[0].port,polls[0].slave,polls[0].func,polls[0].start,polls[0].count,polls[0].dest_reg),(1,1,3,4096,1,4000));
 println!("PASS D0 config produces the expected real RS485 poll item");
 reset();
 let data=packet(&[&ANALOG,&RS485]);assert!(store_device_function_config(&data));
 let stored=*HOLDING.lock().unwrap(); assert_eq!(&stored[offset..offset+4],&[2,4,15,27]);
 assert_eq!(load_device_function_config().unwrap().as_slice(),data.as_slice());
 assert_eq!(parse_poll_table(&stored).unwrap().len(),1);
 println!("PASS mixed records use contiguous relative offsets and roundtrip without field shifts");
 assert!(store_device_function_config(&[0,0]));
 assert_eq!(load_device_function_config().unwrap().as_slice(),&[0,0]);
 assert!(parse_poll_table(&*HOLDING.lock().unwrap()).unwrap().is_empty());
 println!("PASS explicit empty configuration is persisted and readable");
 let before=*HOLDING.lock().unwrap();
 let mut trailing=packet(&[&ANALOG]);trailing.push(0);
 for invalid in [vec![1],vec![33,0],vec![1,0,3,17,34],packet(&[&ANALOG[..21]]),trailing]{
   assert!(!store_device_function_config(&invalid)); assert_eq!(*HOLDING.lock().unwrap(),before);
 }
 println!("PASS malformed lengths, odd records, trailing bytes and excess counts cannot mutate holding");
 {let mut words=HOLDING.lock().unwrap();words[offset]=1;words[offset+1]=2303;words[offset+2]=14;}
 assert!(load_device_function_config().is_none());
 println!("PASS malformed absolute-pointer table is rejected instead of decoded as a valid configuration");
}
'''

with tempfile.TemporaryDirectory(prefix='imc-function-contract-') as tmp:
    p = Path(tmp)
    (p / 'probe.rs').write_text(source)
    version = re.search(r'^version\s*=\s*"([^"]+)"',(ROOT/'Cargo.toml').read_text(),re.M).group(1)
    subprocess.run(['rustc','+stable','--edition=2024',str(p/'probe.rs'),'-o',str(p/'probe')],check=True,env={**os.environ,'CARGO_PKG_VERSION':version})
    subprocess.run([str(p/'probe')],check=True)
