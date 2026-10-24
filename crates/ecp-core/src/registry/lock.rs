//! Cross-platform advisory file lock for registry serialization.
//! Spec §2.1 Layer 1.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};

/// Upper bound on how long [`FileLock::acquire_exclusive`] retries before
/// giving up. registry.json writes are millisecond-scale (a ~2.5 KB JSON
/// rewrite), so under normal contention the lock is acquired in one or two
/// retries. A multi-second ceiling only ever fires when a holder is wedged —
/// at which point a clean `Err` lets the caller surface "registry busy" to
/// the user instead of hanging the process forever (the freeze this replaces).
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// RAII exclusive file lock. Lock is released when dropped.
pub struct FileLock {
    _file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // A concurrent fork can retain this open file description until exec.
        // Release the lock at the guard boundary instead of the last close.
        let _ = FileExt::unlock(&self._file);
    }
}

impl FileLock {
    /// Acquire an exclusive lock on `path`, retrying for up to [`LOCK_TIMEOUT`].
    /// Creates the lockfile if missing. Records the owning PID in the lockfile
    /// body so a stuck holder can be diagnosed (and, when the holder is a dead
    /// PID, the wait short-circuits — see below).
    ///
    /// This replaces a previously *unbounded* `lock_exclusive()`: if a holder
    /// died without the kernel releasing its `flock` (observed on WSL after a
    /// SIGKILL), every other acquirer blocked forever, which presented as a
    /// whole-machine freeze. Bounded retry + dead-holder detection makes the
    /// worst case a timeout `Err`, never a permanent hang.
    pub fn acquire_exclusive(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        lock_exclusive_within(&file, path, LOCK_TIMEOUT)?;
        Ok(Self { _file: file })
    }

    /// [`FileLock::acquire_exclusive`] with the caller's wait budget, for a
    /// lock whose holder does more than a registry write (a whole build).
    pub fn acquire_exclusive_within(path: &Path, timeout: Duration) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        lock_exclusive_within(&file, path, timeout)?;
        Ok(Self { _file: file })
    }

    /// Try to acquire exclusive lock without blocking.
    pub fn try_exclusive(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.try_lock_exclusive()?;
        record_owner_pid(path);
        Ok(Self { _file: file })
    }

    /// Probe a lock that another program owns, without creating anything:
    /// `Ok(None)` when the lock file is absent, `Err` of kind `WouldBlock`
    /// while a holder has it, any other error as it came.
    ///
    /// No owner sidecar is written. The lock this probes can sit inside a
    /// directory an older ecp is still building, and a file written there
    /// would be published with that build.
    pub fn try_existing(path: &Path) -> io::Result<Option<Self>> {
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(e) if is_lock_contended(&e) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("lock {} is held", path.display()),
            )),
            Err(e) => Err(e),
        }
    }

    /// Replace the owner sidecar of `path` with `<pid> <detail>`, for a lock
    /// that several keys share: the waiter then learns which key the holder
    /// holds it for, not only who. Best-effort, like the PID write.
    pub fn record_owner(path: &Path, detail: &str) {
        let _ = std::fs::write(
            owner_pid_sidecar(path),
            format!("{} {detail}", std::process::id()),
        );
    }

    /// The owner sidecar of `path`: the PID and what the holder wrote after
    /// it (empty for a holder that wrote its PID only).
    pub fn owner(path: &Path) -> Option<(u32, String)> {
        let s = std::fs::read_to_string(owner_pid_sidecar(path)).ok()?;
        let s = s.trim();
        let (pid, rest) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
        Some((pid.parse().ok()?, rest.trim().to_string()))
    }
}

/// True when `err` is a lock attempt that failed only because another
/// descriptor holds the lock. Windows reports that as `ERROR_LOCK_VIOLATION`,
/// which std does not map to `WouldBlock`.
pub fn is_lock_contended(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
        || err.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

/// The PID the last holder of `path` recorded, for a diagnostic that names it.
pub fn lock_owner_pid(path: &Path) -> Option<u32> {
    read_owner_pid(path)
}

/// Bounded exclusive lock on an already-open `file`, with the dead-holder
/// probe and the diagnostic error that [`FileLock::acquire_exclusive`] uses.
///
/// Separate from `FileLock` so a caller that must keep its own `File` — the
/// build lock is handed down the call chain and dropped at an exact point, just
/// before the publish rename — gets the same bounded wait rather than its own
/// copy of the loop. `timeout` is a parameter because the two callers wait for
/// different things: a registry write is millisecond-scale, a build lock is
/// waiting out somebody else's whole index.
pub fn lock_exclusive_within(file: &File, path: &Path, timeout: Duration) -> io::Result<()> {
    let contended = fs2::lock_contended_error().raw_os_error();
    let start = Instant::now();
    let mut checked_dead_holder = false;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                record_owner_pid(path);
                return Ok(());
            }
            // Only contention is worth retrying. Anything else — a bad
            // descriptor, a filesystem that will not lock — does not become
            // true by waiting, and retrying it would replace the real error
            // with a timeout that names the wrong cause.
            Err(e) if e.raw_os_error() != contended => return Err(e),
            Err(_) => {}
        }
        // One-shot dead-holder probe: if the recorded owner PID is gone, the
        // kernel should already have dropped its flock, so the next try wins.
        // We still loop (don't assume) and never delete the lockfile — flock is
        // fd-scoped, not content-scoped, so there is no stale state to clear.
        if !checked_dead_holder {
            checked_dead_holder = true;
            if owner_pid_is_dead(path) {
                continue;
            }
        }
        if start.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "lock {} busy for >{}s (holder pid {})",
                    path.display(),
                    timeout.as_secs(),
                    read_owner_pid(path).map_or_else(|| "?".to_string(), |p| p.to_string()),
                ),
            ));
        }
        std::thread::sleep(LOCK_RETRY_INTERVAL);
    }
}

/// Path of the PID sidecar written next to the lockfile.
///
/// The owner PID lives in a SEPARATE file, not the lockfile body, because on
/// Windows `LockFileEx` (which `fs2` uses) locks the byte range: while the lock
/// is held, any other handle reading the lockfile content fails. Writing the
/// PID into the locked file would make it unreadable by exactly the path that
/// needs it — the dead-holder probe (a different process) and the diagnostic
/// read. The sidecar is never flocked, so it stays readable on every platform.
fn owner_pid_sidecar(path: &Path) -> std::path::PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".owner");
    std::path::PathBuf::from(p)
}

/// Record the current PID in the lock's PID sidecar. Best-effort: a failure here
/// only loses diagnostics, never correctness — the flock is what guards the
/// registry, not this file.
fn record_owner_pid(path: &Path) {
    let _ = std::fs::write(owner_pid_sidecar(path), std::process::id().to_string());
}

/// The first word of the sidecar: [`FileLock::record_owner`] writes more
/// words after the PID.
fn read_owner_pid(path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(owner_pid_sidecar(path))
        .ok()?
        .read_to_string(&mut s)
        .ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// True when the lockfile names an owner PID that is no longer alive. A missing
/// or unparseable PID is treated as "not provably dead" (returns false) so we
/// never short-circuit on ambiguous state.
fn owner_pid_is_dead(path: &Path) -> bool {
    match read_owner_pid(path) {
        Some(pid) => !crate::peer::registry::pid_alive(pid),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn test_file_lock_drop_with_duplicate_descriptor_releases_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("inherited.lock");
        let held = FileLock::acquire_exclusive(&path).unwrap();
        // A duplicate shares the open file description, as a child does
        // between fork and exec when another thread starts a command.
        let duplicate = held._file.try_clone().unwrap();
        assert!(FileLock::try_exclusive(&path).is_err());

        drop(held);

        let _next = FileLock::try_exclusive(&path)
            .expect("dropping the guard must release the lock while a duplicate remains open");
        drop(duplicate);
    }

    /// The point of the bound is that a wedged holder ends the wait with an
    /// error instead of never ending it. Asserting only `is_err()` would also
    /// pass for a hang that the test harness eventually killed, so the elapsed
    /// time is part of the assertion.
    #[test]
    fn lock_exclusive_within_gives_up_and_names_the_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("busy.lock");
        let held = FileLock::acquire_exclusive(&path).expect("first acquisition wins");

        let waiter = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let start = Instant::now();
        let err = lock_exclusive_within(&waiter, &path, Duration::from_millis(200))
            .expect_err("a held lock must not be handed out twice");
        let waited = start.elapsed();

        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        // Both bounds, because either alone passes for the wrong timeout: an
        // upper bound of seconds accepts a caller that waits far longer than it
        // asked to, and no lower bound accepts one that never waited at all.
        assert!(
            waited >= Duration::from_millis(200),
            "the wait ended before the timeout it was given: {waited:?}"
        );
        assert!(
            waited < Duration::from_millis(900),
            "the wait must end near the timeout, not run on: {waited:?}"
        );
        assert!(
            err.to_string().contains(&std::process::id().to_string()),
            "the error has to name the holder or it cannot be diagnosed: {err}"
        );
        drop(held);
    }

    /// The other half: once the holder lets go, the same call succeeds. Without
    /// this the test above would also pass for a function that never acquires
    /// anything.
    #[test]
    fn lock_exclusive_within_acquires_a_free_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("free.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        lock_exclusive_within(&file, &path, Duration::from_millis(200))
            .expect("an unheld lock must be granted");
    }

    /// gc probes a lock that an older ecp may own. An absent lock is reported
    /// as absent, and the probe leaves no file behind: a file created inside
    /// an older build's staging dir would be published with that build.
    #[test]
    fn test_file_lock_try_existing_absent_none_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("absent.lock");
        assert!(FileLock::try_existing(&path).unwrap().is_none());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn test_file_lock_try_existing_held_would_block() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("held.lock");
        let held = FileLock::acquire_exclusive(&path).unwrap();
        let err = FileLock::try_existing(&path)
            .err()
            .expect("a held lock must read as busy");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(is_lock_contended(&err));
        drop(held);
        assert!(FileLock::try_existing(&path).unwrap().is_some());
    }

    #[test]
    fn test_file_lock_try_existing_free_writes_no_owner_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.lock");
        std::fs::write(&path, b"").unwrap();
        let probe = FileLock::try_existing(&path).unwrap();
        assert!(probe.is_some());
        assert!(!owner_pid_sidecar(&path).exists());
    }

    /// A holder that names its key keeps the PID readable for the
    /// dead-holder probe, which reads the first word only.
    #[test]
    fn test_file_lock_record_owner_detail_pid_still_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ab.lock");
        let held = FileLock::acquire_exclusive(&path).unwrap();
        FileLock::record_owner(&path, "build abab");
        assert_eq!(lock_owner_pid(&path), Some(std::process::id()));
        assert_eq!(
            FileLock::owner(&path),
            Some((std::process::id(), "build abab".to_string()))
        );
        drop(held);
        std::fs::write(owner_pid_sidecar(&path), "77\n").unwrap();
        assert_eq!(FileLock::owner(&path), Some((77, String::new())));
    }

    /// A zero budget ends at once with the holder's PID, so a build waiter
    /// can report who it waited for.
    #[test]
    fn test_file_lock_acquire_exclusive_within_zero_budget_names_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("build.lock");
        let held = FileLock::acquire_exclusive(&path).unwrap();
        let err = FileLock::acquire_exclusive_within(&path, Duration::ZERO)
            .err()
            .expect("a held lock must not be handed out twice");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(lock_owner_pid(&path), Some(std::process::id()));
        drop(held);
        FileLock::acquire_exclusive_within(&path, Duration::ZERO)
            .expect("a free lock is granted on the first try");
    }

    /// A lock that cannot be taken for a reason other than contention must
    /// surface that reason at once, not wait out the timeout and report
    /// contention that never happened.
    #[cfg(unix)]
    #[test]
    fn lock_exclusive_within_returns_a_non_contention_error_at_once() {
        use std::os::fd::FromRawFd;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("never.lock");
        // `i32::MAX` is above any reachable `RLIMIT_NOFILE`, so it is never an
        // open descriptor and `try_lock_exclusive` fails with EBADF — an error
        // no amount of waiting turns into a lock. Picking a number rather than
        // closing a real descriptor keeps this deterministic: cargo runs tests
        // in parallel threads, and a closed number can be reused by one of
        // them. `ManuallyDrop` keeps the `File` from closing it on the way out.
        let file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(i32::MAX) });

        let start = Instant::now();
        let err = lock_exclusive_within(&file, &path, Duration::from_secs(30))
            .expect_err("an invalid descriptor cannot be locked");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a non-contention error must return at once, not after the timeout: {:?}",
            start.elapsed()
        );
        assert_ne!(
            err.kind(),
            io::ErrorKind::WouldBlock,
            "the real cause must survive rather than becoming a timeout: {err}"
        );
    }
}
