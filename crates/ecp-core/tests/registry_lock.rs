//! Tests for cross-platform file lock (spec §2.1 Layer 1).

use ecp_core::registry::FileLock;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn second_acquirer_waits_until_holder_releases() {
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
    // Retries (50ms cadence) until the 200ms holder releases — well under the
    // 5s timeout, so this resolves to Ok, not a timeout Err.
    let _g = FileLock::acquire_exclusive(&path).unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(150),
        "expected to wait ~200ms for the holder, got {elapsed:?}"
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

/// The core deadlock fix: a live holder that never releases must NOT hang the
/// acquirer forever — it returns a timeout `Err` after the bounded retry
/// window. (We don't wait the full 5s here; we just assert it returns Err
/// rather than blocking the test thread indefinitely, by holding across a
/// generous-but-finite join.)
#[test]
fn acquire_times_out_instead_of_hanging_when_holder_never_releases() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let _holder = FileLock::acquire_exclusive(&path).unwrap();

    let start = Instant::now();
    let r = FileLock::acquire_exclusive(&path);
    let elapsed = start.elapsed();

    assert!(
        r.is_err(),
        "expected timeout Err, got Ok (would have hung pre-fix)"
    );
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(8),
        "expected to give up near the 5s ceiling, got {elapsed:?}"
    );
}

/// `acquire_exclusive` records the caller's PID in the lockfile body so a
/// stuck holder is diagnosable and the dead-holder probe has something to read.
#[test]
fn lockfile_records_owner_pid() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let _guard = FileLock::acquire_exclusive(&path).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    let recorded: u32 = body.trim().parse().expect("pid written to lockfile");
    assert_eq!(recorded, std::process::id());
}
