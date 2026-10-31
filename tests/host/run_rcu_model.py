#!/usr/bin/env python3
"""Bounded Loom exploration of production RCU; only primitive/time adapters differ."""
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
source = (ROOT/'src/bus/rcu.rs').read_text().split('#[cfg(test)]')[0]
# Adapt library primitives, const constructors (Loom is runtime initialized),
# the exclusive Drop load, and the scheduler wait. Publication/reclamation logic
# remains the actual production source. Fail loudly if these anchors change.
changes = {
    'use std::sync::{Arc, Mutex};': 'use loom::sync::{Arc, Mutex};',
    'use std::sync::atomic::': 'use loom::sync::atomic::',
    'pub const fn empty()': 'pub fn empty()',
    'let ptr = *self.ptr.get_mut();': 'let ptr = self.ptr.load(Ordering::SeqCst);',
    'std::thread::sleep(std::time::Duration::from_millis(1));': 'loom::thread::yield_now();',
}
for old,new in changes.items():
    if old not in source: raise SystemExit('Production anchor changed: '+old)
    source = source.replace(old,new)
source += r'''
#[test]
fn test_rcu_acquisition_and_two_publications() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.max_branches = 1000;
    builder.max_duration = Some(std::time::Duration::from_secs(90));
    builder.check(|| {
        let rcu = loom::sync::Arc::new(Rcu::new(1u32));
        let reader = rcu.clone();
        let writer = rcu.clone();
        let r = loom::thread::spawn(move || {
            let value = reader.read().unwrap();
            loom::thread::yield_now();
            assert!((1..=3).contains(&*value));
        });
        let w = loom::thread::spawn(move || { writer.write(2); writer.write(3); });
        r.join().unwrap();
        w.join().unwrap();
        assert_eq!(*rcu.read().unwrap(), 3);
    });
}
'''
with tempfile.TemporaryDirectory(prefix='gateway-rcu-model-') as directory:
    path=Path(directory);(path/'src').mkdir()
    (path/'src/lib.rs').write_text(source)
    (path/'Cargo.toml').write_text('[package]\nname="gateway-rcu-model"\nversion="0.0.0"\nedition="2024"\n[dependencies]\nloom="=0.7.2"\n[workspace]\n')
    subprocess.run(['cargo','+stable','test','--offline','--manifest-path',str(path/'Cargo.toml'),'--','--nocapture'],cwd=path,check=True,timeout=180)
