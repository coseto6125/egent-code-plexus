//! Reusable detached subprocess pattern for fire-and-forget jobs that
//! must outlive the current `ecp` invocation. The job runs under a
//! non-blocking `flock` so concurrent triggers no-op cleanly.

use std::path::Path;
use std::process::{Command, Stdio};

/// Optional marker files written by the background job to report
/// outcome — consumed asynchronously by e.g. the UserPromptSubmit hook.
pub struct BgMarkers<'a> {
    pub log: &'a Path,
    pub complete: &'a Path,
    pub failed: &'a Path,
}

/// Spec for a detached background `ecp <args>` invocation.
pub struct BgJob<'a> {
    /// CLI args appended after the resolved `ecp` binary path.
    /// Example: `&["admin", "index", "--repo", repo_str]`.
    pub args: &'a [&'a str],
    /// Non-blocking `flock` target. If another process holds it, the
    /// launcher exits 0 immediately (no-op).
    pub lock: &'a Path,
    /// Subprocess working directory.
    pub cwd: &'a Path,
    /// Retry policy: `(max_attempts, sleep_secs_between_attempts)`.
    /// Use `(1, 0)` for one-shot jobs.
    pub retry: (u32, u32),
    /// Optional marker files. `None` = fire-and-forget, no result file.
    pub markers: Option<BgMarkers<'a>>,
}

/// The two-line shell preamble used to guard `spawn_bg`'s inner job
/// with a non-blocking `flock`. Exposed so tests can pin to the same
/// quoting + redirect behaviour as production (otherwise tests would
/// re-implement the template and silently drift).
pub fn flock_preamble(lock: &Path) -> String {
    let lock_dir = lock.with_file_name(format!(
        "{}.d",
        lock.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ecp.lock")
    ));
    format!(
        "if command -v flock >/dev/null 2>&1; then\n  exec 9>{lock} || exit 0\n  flock -n 9 || exit 0\nelse\n  mkdir {lock_dir} 2>/dev/null || exit 0\n  trap \"rmdir {lock_dir}\" EXIT INT TERM\n  : > {lock}\nfi\n",
        lock = shell_quote(lock),
        lock_dir = shell_quote(lock_dir),
    )
}

/// Spawn the job as a detached subprocess. Returns `true` iff the
/// launcher subprocess started. The job's actual outcome surfaces
/// via the marker files (if configured).
pub fn spawn_bg(job: BgJob) -> bool {
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let args_quoted: Vec<String> = job.args.iter().map(|arg| shell_quote(*arg)).collect();
    let args_joined = args_quoted.join(" ");

    let shell = if let Some(markers) = &job.markers {
        format!(
            r#"{preamble}: > {log}
MAX={max}; ATTEMPT=0
while [ $ATTEMPT -lt $MAX ]; do
  ATTEMPT=$((ATTEMPT+1))
  echo "=== attempt $ATTEMPT/$MAX ===" >> {log}
  if {ecp} {args} >> {log} 2>&1; then
    rm -f {failed}
    : > {complete}
    exit 0
  fi
  [ $ATTEMPT -lt $MAX ] && sleep {sleep_secs}
done
rm -f {complete}
: > {failed}
"#,
            preamble = flock_preamble(job.lock),
            log = shell_quote(markers.log),
            ecp = shell_quote(&self_exe),
            args = args_joined,
            max = job.retry.0,
            sleep_secs = job.retry.1,
            complete = shell_quote(markers.complete),
            failed = shell_quote(markers.failed),
        )
    } else {
        format!(
            r#"{preamble}MAX={max}; ATTEMPT=0
while [ $ATTEMPT -lt $MAX ]; do
  ATTEMPT=$((ATTEMPT+1))
  if {ecp} {args} >/dev/null 2>&1; then
    exit 0
  fi
  [ $ATTEMPT -lt $MAX ] && sleep {sleep_secs}
done
"#,
            preamble = flock_preamble(job.lock),
            ecp = shell_quote(&self_exe),
            args = args_joined,
            max = job.retry.0,
            sleep_secs = job.retry.1,
        )
    };

    Command::new("sh")
        .arg("-c")
        .arg(&shell)
        .current_dir(job.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

pub(crate) fn shell_quote<P: AsRef<Path>>(p: P) -> String {
    let s = p.as_ref().to_string_lossy().to_string();
    let escaped = s.replace('\'', r"'\''");
    format!("'{}'", escaped)
}
