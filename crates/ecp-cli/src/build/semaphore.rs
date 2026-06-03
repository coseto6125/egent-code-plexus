//! Global build concurrency cap. Per-repo `.build.lock` already serializes
//! rebuilds *of one repo*, but nothing bounds how many *different* repos
//! rebuild at once — and each L2 build issues ~180 MB of I/O. On WSL2 the
//! `~/.ecp` vhdx saturates under a handful of concurrent builds and hangs the
//! host (load spiked to 8.4 with 8 concurrent agents). This caps the number of
//! *simultaneous* L2 builds machine-wide using the same `fs2` advisory file
//! locks the orchestrator already relies on (cross-platform, no new deps).
//!
//! It gates only the heavy rebuild path — cache hits, warm-attach, and every
//! query are untouched, so steady-state usage (graph already built) never
//! queues here. First build / SHA change is the only thing that can wait.

use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// Env override for the cap. `0` or unparseable falls back to the auto value.
const ENV_MAX: &str = "ECP_MAX_CONCURRENT_BUILDS";

/// How long a builder waits for a free slot before proceeding anyway. The cap is
/// a safety throttle, not a correctness barrier: if every slot is held for this
/// long we let the build run rather than stall indefinitely (the per-repo
/// `.build.lock` still prevents duplicate work for the same SHA).
const SLOT_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// A held build slot. Dropping it releases the underlying advisory lock.
pub struct BuildSlot {
    _file: File,
}

/// Compute the machine-wide cap on simultaneous L2 builds.
///
/// The true limiting factor is concurrent disk I/O saturation, which no OS
/// exposes an API for — so this is a deliberately conservative, env-overridable
/// heuristic keyed off the one reliable signal (core count) plus environment
/// class. Only the WSL2 branch is backed by a real crash measurement; the
/// native Windows / macOS / Linux ceilings are conservative inferences.
pub fn max_concurrent_builds() -> usize {
    if let Ok(v) = std::env::var(ENV_MAX) {
        if let Ok(k) = v.parse::<usize>() {
            if k >= 1 {
                return k;
            }
        }
    }
    let cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    compute_cap(cores, env_class())
}

/// Environment classes with materially different concurrent-I/O ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvClass {
    /// WSL2: `~/.ecp` lives on a vhdx; concurrent I/O hangs the host. Measured.
    Wsl2,
    /// Native Windows: NTFS + Defender real-time scan amplify concurrent writes
    /// (inferred, not measured — the only receipt is the os-error-5 rename lock).
    Windows,
    /// Native macOS / Linux: assumed SSD; I/O is not the bottleneck (inferred).
    Unix,
}

fn env_class() -> EnvClass {
    if is_wsl2() {
        EnvClass::Wsl2
    } else if cfg!(windows) {
        EnvClass::Windows
    } else {
        EnvClass::Unix
    }
}

/// Reliable: WSL2 stamps its kernel release. (`rotational`/disk-type probing is
/// NOT reliable here — vhdx reports as a rotational HDD.)
fn is_wsl2() -> bool {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.to_lowercase().contains("wsl"))
        .unwrap_or(false)
}

/// Pure cap computation (testable without touching the host).
pub fn compute_cap(cores: usize, class: EnvClass) -> usize {
    let div = |n: usize, d: usize| (n / d).max(1);
    match class {
        // Measured crash region; stay well under it.
        EnvClass::Wsl2 => div(cores, 8).clamp(2, 3),
        // Inferred: one ceiling tighter than native Unix.
        EnvClass::Windows => div(cores, 4).clamp(2, 4),
        // Inferred: SSD, I/O not limiting.
        EnvClass::Unix => div(cores, 4).clamp(2, 6),
    }
}

/// Acquire one of the global build slots, waiting up to [`SLOT_WAIT_TIMEOUT`].
/// Returns `None` only on unexpected I/O errors creating the slot dir/files, in
/// which case the caller should proceed unthrottled (degrade open, never block
/// a build on the throttle's own failure).
pub fn acquire(home_ecp: &Path) -> Option<BuildSlot> {
    let k = max_concurrent_builds();
    let dir = home_ecp.join(".build-slots");
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let slots: Vec<PathBuf> = (0..k).map(|i| dir.join(format!("slot-{i}"))).collect();

    let deadline = Instant::now() + SLOT_WAIT_TIMEOUT;
    loop {
        for path in &slots {
            if let Ok(file) = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(path)
            {
                if file.try_lock_exclusive().is_ok() {
                    return Some(BuildSlot { _file: file });
                }
            }
        }
        if Instant::now() >= deadline {
            // All slots held past the timeout — proceed anyway (throttle is
            // best-effort; per-repo .build.lock still guards correctness).
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl2_cap_stays_in_safe_band_across_core_counts() {
        // Never below the floor, never above the measured-safe ceiling.
        for cores in [1, 4, 8, 16, 32, 128] {
            let k = compute_cap(cores, EnvClass::Wsl2);
            assert!(
                (2..=3).contains(&k),
                "cores={cores} → k={k} out of WSL2 band"
            );
        }
    }

    #[test]
    fn windows_ceiling_is_tighter_than_unix() {
        // At high core counts, Windows is capped one band below Unix.
        assert_eq!(compute_cap(64, EnvClass::Windows), 4);
        assert_eq!(compute_cap(64, EnvClass::Unix), 6);
    }

    #[test]
    fn floor_is_two_even_on_tiny_machines() {
        for class in [EnvClass::Wsl2, EnvClass::Windows, EnvClass::Unix] {
            assert_eq!(compute_cap(1, class), 2, "{class:?} floor");
        }
    }

    #[test]
    fn scales_with_cores_until_ceiling() {
        // Unix: cores/4 until it hits the ceiling of 6.
        assert_eq!(compute_cap(8, EnvClass::Unix), 2);
        assert_eq!(compute_cap(16, EnvClass::Unix), 4);
        assert_eq!(compute_cap(24, EnvClass::Unix), 6);
        assert_eq!(compute_cap(48, EnvClass::Unix), 6); // capped
    }
}
