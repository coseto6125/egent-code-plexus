//! Cross-platform advisory file lock for registry serialization.
//! Spec §2.1 Layer 1.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
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

        let start = Instant::now();
        let mut checked_dead_holder = false;
        loop {
            if file.try_lock_exclusive().is_ok() {
                record_owner_pid(&file);
                return Ok(Self { _file: file });
            }
            // One-shot dead-holder probe: if the recorded owner PID is gone,
            // the kernel should already have dropped its flock, so the next
            // try wins. We still loop (don't assume) and never delete the
            // lockfile — flock is fd-scoped, not content-scoped, so there is
            // no stale state to clear.
            if !checked_dead_holder {
                checked_dead_holder = true;
                if owner_pid_is_dead(path) {
                    continue;
                }
            }
            if start.elapsed() >= LOCK_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "registry lock {} busy for >{}s (holder pid {})",
                        path.display(),
                        LOCK_TIMEOUT.as_secs(),
                        read_owner_pid(path).map_or_else(|| "?".to_string(), |p| p.to_string()),
                    ),
                ));
            }
            std::thread::sleep(LOCK_RETRY_INTERVAL);
        }
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
        record_owner_pid(&file);
        Ok(Self { _file: file })
    }
}

/// Write the current PID into the (locked) lockfile body. Best-effort: a
/// failure here only loses diagnostics, never correctness — the flock is what
/// guards the registry, not the file content.
fn record_owner_pid(file: &File) {
    use std::io::Seek;
    let mut f = file;
    let _ = f.rewind();
    let _ = f.set_len(0);
    let _ = write!(f, "{}", std::process::id());
    let _ = f.flush();
}

fn read_owner_pid(path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse().ok()
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
