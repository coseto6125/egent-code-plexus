//! Tests for cross-platform file lock (spec §2.1 Layer 1).

use cgn_core::registry::FileLock;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn exclusive_lock_blocks_second_acquirer() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let barrier = Arc::new(Barrier::new(2));
    let b1 = barrier.clone();
    let p1 = path.clone();

    let t1 = thread::spawn(move || {
        let _guard = FileLock::acquire_exclusive(&p1).unwrap();
        b1.wait();
        thread::sleep(Duration::from_millis(200));
    });

    barrier.wait();
    let start = Instant::now();
    let _g = FileLock::acquire_exclusive(&path).unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(150),
        "expected to block ~200ms, got {elapsed:?}"
    );
    t1.join().unwrap();
}

#[test]
fn try_lock_returns_immediately_when_busy() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let _guard = FileLock::acquire_exclusive(tmp.path()).unwrap();

    let r = FileLock::try_exclusive(tmp.path());
    assert!(r.is_err(), "expected try_exclusive to fail when held");
}
