#!/usr/bin/env python3
"""Execute the production event bus tests with a host Spin shim."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

SOURCE = r'''
mod sync {
    use std::cell::UnsafeCell;
    use std::hint::spin_loop;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub struct Spin<T: ?Sized> { flag: AtomicBool, inner: UnsafeCell<T> }
    unsafe impl<T: ?Sized + Send> Sync for Spin<T> {}
    unsafe impl<T: ?Sized + Send> Send for Spin<T> {}
    impl<T> Spin<T> { pub const fn new(value: T) -> Self { Self { flag: AtomicBool::new(false), inner: UnsafeCell::new(value) } } }
    impl<T: ?Sized> Spin<T> {
        pub fn lock(&self) -> Guard<'_, T> {
            while self.flag.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() { spin_loop(); }
            Guard { lock: self }
        }
    }
    pub struct Guard<'a, T: ?Sized> { lock: &'a Spin<T> }
    impl<T: ?Sized> std::ops::Deref for Guard<'_, T> { type Target=T; fn deref(&self)->&T { unsafe { &*self.lock.inner.get() } } }
    impl<T: ?Sized> std::ops::DerefMut for Guard<'_, T> { fn deref_mut(&mut self)->&mut T { unsafe { &mut *self.lock.inner.get() } } }
    impl<T: ?Sized> Drop for Guard<'_, T> { fn drop(&mut self) { self.lock.flag.store(false, Ordering::Release); } }
}
#[path = "__EVENT_BUS__"] mod event_bus;
'''.replace("__EVENT_BUS__", str(ROOT / "src/bus/event_bus.rs"))


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="gateway-event-bus-") as temporary:
        root = Path(temporary)
        source = root / "event_bus_test.rs"
        binary = root / "tests"
        source.write_text(SOURCE)
        subprocess.run(["rustc", "+stable", "--edition=2024", "--test", str(source), "-o", str(binary)], check=True)
        subprocess.run([str(binary), "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
