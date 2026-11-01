#!/usr/bin/env python3
"""Exercise actual PCA transaction helpers against a failing register bus (F3/F4)."""
import subprocess
import tempfile
from pathlib import Path
from run_production_core import item

ROOT=Path(__file__).resolve().parents[2]
s=(ROOT/'src/hal/pca9555.rs').read_text()
functions='\n'.join(item(s,'fn '+name+'(') for name in (
    'bit_reverse','worker_sample_di','worker_write_do','worker_read_do'))
submission = item(s, 'fn submit_output(')
for chips,di,do in ((1,[0x40],[0x42]),(3,[0x40,0x44,0x42],[0x48,0x4c,0x4e])):
 source='''
#![allow(dead_code)]
use std::cell::{RefCell,Cell};
use std::collections::BTreeMap;
use std::sync::{Arc,Mutex,TryLockError};
use std::sync::atomic::{AtomicU32,AtomicU64,Ordering};
#[derive(Debug)] enum AppError { Io(String), Busy(&'static str) }
type AppResult<T> = Result<T,AppError>;
const REG_INPUT_0:u8=0;
const REG_OUTPUT_0:u8=2;
struct BusI2c { data:RefCell<BTreeMap<(u8,u8),[u8;2]>>, fail:Cell<bool> }
impl BusI2c {
 fn new()->Self { Self { data:RefCell::new(BTreeMap::new()),fail:Cell::new(false) } }
 fn read_regs2(&self,addr:u8,reg:u8)->AppResult<[u8;2]> {
  if self.fail.get() { return Err(AppError::Io("injected NACK".into())); }
  Ok(*self.data.borrow().get(&(addr,reg)).unwrap_or(&[0xff;2]))
 }
 fn write_regs2(&self,addr:u8,reg:u8,data:[u8;2])->AppResult<()> {
  if self.fail.get() { return Err(AppError::Io("injected NACK".into())); }
  self.data.borrow_mut().insert((addr,reg),data); Ok(())
 }
}
'''+f'const MAX_CHIPS:usize={chips};\nconst DI_IO_ADDRS:&[u8]=&{di};\nconst DO_IO_ADDRS:&[u8]=&{do};\n'+functions+f'''
#[path = "{ROOT / 'src/hal/io_worker_state.rs'}"] mod io_worker_state;
use io_worker_state::{{OutputQueue,OUTPUT_QUEUE_CAPACITY}};
struct AtomicBits64(AtomicU64);
impl AtomicBits64 {{ fn store_bits(&self,bits:u64) {{ self.0.store(bits,Ordering::SeqCst); }} }}
struct Pca9555WorkerState {{ outputs:Mutex<OutputQueue>,do_submit_busy:AtomicU32,do_desired:AtomicBits64 }}
struct Pca9555Duo {{ worker:Arc<Pca9555WorkerState> }}
impl Pca9555Duo {{ {submission} }}
'''+'''
fn driver() -> Pca9555Duo {
 Pca9555Duo { worker:Arc::new(Pca9555WorkerState {
  outputs:Mutex::new(OutputQueue::new()),do_submit_busy:AtomicU32::new(0),
  do_desired:AtomicBits64(AtomicU64::new(0)) }) }
}
#[test]
fn test_actual_submission_distinguishes_contention_full_and_poisoned() {
 let drv=driver();
 let guard=drv.worker.outputs.lock().unwrap();
 assert!(matches!(drv.submit_output(|_|1),Err(AppError::Busy(_))));
 assert_eq!(drv.worker.do_submit_busy.load(Ordering::Relaxed),1);
 assert_eq!(drv.worker.do_desired.0.load(Ordering::SeqCst),0);
 drop(guard);
 for value in 1..=OUTPUT_QUEUE_CAPACITY as u64 { drv.submit_output(|_|value).unwrap(); }
 assert!(matches!(drv.submit_output(|_|999),Err(AppError::Busy(_))));
 assert_eq!(drv.worker.do_desired.0.load(Ordering::SeqCst),OUTPUT_QUEUE_CAPACITY as u64);
 let mut queue=drv.worker.outputs.lock().unwrap();
 assert_eq!(queue.rejected,1);
 let first=queue.front().unwrap(); assert!(queue.confirm(first)); drop(queue);
 drv.submit_output(|_|999).unwrap();
 assert_eq!(drv.worker.do_desired.0.load(Ordering::SeqCst),999);
 let worker=Arc::clone(&drv.worker);
 let _=std::thread::spawn(move|| {let _guard=worker.outputs.lock().unwrap();panic!("injected poison");}).join();
 assert!(matches!(drv.submit_output(|_|123),Err(AppError::Io(_))));
}
#[test]
fn test_output_readback_and_nack_are_real_bus_results() {
 let bus=BusI2c::new();
 let value=0x1234_5a69_f00du64 & ((1u64<<(MAX_CHIPS*16))-1);
 worker_write_do(&bus,value).unwrap();
 assert_eq!(worker_read_do(&bus).unwrap(),value);
 bus.fail.set(true);
 assert!(worker_read_do(&bus).is_err());
 assert!(worker_write_do(&bus,0).is_err());
 bus.fail.set(false);
 // Simulate a reset output latch: actual read must see the changed register.
 bus.data.borrow_mut().insert((DO_IO_ADDRS[0],REG_OUTPUT_0),[0;2]);
 assert_ne!(worker_read_do(&bus).unwrap(),value);
}
#[test]
fn test_led_bus_failure_cannot_invalidate_input_sample() {
 let io=BusI2c::new(); let led=BusI2c::new(); led.fail.set(true);
 for &addr in DI_IO_ADDRS {io.data.borrow_mut().insert((addr,REG_INPUT_0),[0xfe,0x7f]);}
 let (bits,_)=worker_sample_di(&io).unwrap();
 for i in 0..MAX_CHIPS {assert_eq!((bits>>(i*16))&0xffff,0x0101);}
 assert!(led.write_regs2(0x40,REG_OUTPUT_0,[0;2]).is_err());
 assert_eq!(worker_sample_di(&io).unwrap().0,bits);
 io.fail.set(true); assert!(worker_sample_di(&io).is_err());
}
'''
 with tempfile.TemporaryDirectory(prefix='gateway-io-transactions-') as directory:
  p=Path(directory);(p/'tests.rs').write_text(source)
  subprocess.run(['rustc','--edition=2024','--test',str(p/'tests.rs'),'-o',str(p/'tests')],check=True)
  print(f'Actual PCA helpers: {chips} chip bank(s)',flush=True)
  subprocess.run([str(p/'tests')],check=True,timeout=30)
