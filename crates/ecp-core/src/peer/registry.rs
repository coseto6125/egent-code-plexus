//! Enumerate alive peer sessions sharing the same repo `common_dir`.

use crate::session::SessionMeta;
use chrono::{DateTime, Utc};
use std::fs;
use std::path::Path;

/// How long a session stays visible after its last sign of activity.
///
/// `session_meta.pid` is the pid of the one-shot `ecp` process that wrote the
/// file — it dies before any peer can probe it, so it proves nothing about the
/// agent that owns the session. The heartbeat in `last_touched` is the only
/// evidence a stateless CLI leaves behind, and this window is what turns it
/// into a liveness answer.
///
/// Deliberately generous, because the two failure directions do not cost the
/// same. Too short and a working agent is judged dead: its targets drop out of
/// `peers plan`, the answer comes back `overlaps: []`, and two agents edit the
/// same symbol believing they are alone — the feature failing silently at the
/// one moment it exists for. Too long and a departed agent stays listed, which
/// costs one extra warning about a file whose overlay entry is still on disk
/// and therefore still genuinely differs from the published graph.
///
/// Two hours covers the case that actually goes quiet: a single long tool call
/// (a 40-minute test run, a background build) beats once on dispatch and then
/// emits nothing while the worktree stays dirty. `admin gc` still archives a
/// session after `SESSION_IDLE_HOURS`, so this window bounds visibility, not
/// disk.
pub const SESSION_LIVE_TTL_MINS: i64 = 120;

#[derive(Debug, Clone)]
pub struct PeerSession {
    pub session_id: String,
    /// `None` for watch-path enrolments, which record no pid at all.
    pub pid: Option<u32>,
    pub last_touched: DateTime<Utc>,
    pub base_sha: String,
    pub watcher_alive: bool,
    /// Raw watcher pid from session_meta. `None` = watcher never started;
    /// `Some(_)` with `!watcher_alive` = watcher pid recorded but process died.
    /// Lets callers distinguish "not-started" from "dead" without re-reading meta.
    pub watcher_pid: Option<u32>,
    /// Team-visible agent name from session_meta (None for solo sessions).
    pub agent_name: Option<String>,
}

pub fn alive_peers(repo_root: &Path, exclude_self: &str) -> Vec<PeerSession> {
    let sessions_dir = repo_root.join("sessions");
    let Ok(read) = fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if id.is_empty() || id == exclude_self || id.starts_with('.') || id.contains(".stale-") {
            continue;
        }
        let meta_path = path.join("session_meta.json");
        let Ok(meta) = SessionMeta::read(&meta_path) else {
            continue;
        };
        let Ok(last_touched) = meta.last_touched.parse::<DateTime<Utc>>() else {
            continue;
        };
        if !session_alive(meta.pid, meta.watcher_pid, last_touched, Utc::now()) {
            continue;
        }
        let watcher_alive = meta.watcher_pid.is_some_and(pid_alive);
        out.push(PeerSession {
            session_id: id.to_string(),
            pid: meta.pid,
            last_touched,
            base_sha: meta.base_sha,
            watcher_alive,
            watcher_pid: meta.watcher_pid,
            agent_name: meta.agent_name,
        });
    }
    out
}

/// A session is alive when something still vouches for it: a running watcher
/// daemon, a pid that happens to outlive the write (long-running hosts, tests),
/// or an `ecp` invocation inside [`SESSION_LIVE_TTL_MINS`].
///
/// A dead pid on its own is not evidence of death — see the constant's note.
pub fn session_alive(
    pid: Option<u32>,
    watcher_pid: Option<u32>,
    last_touched: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    watcher_pid.is_some_and(pid_alive)
        || pid.is_some_and(pid_alive)
        || now.signed_duration_since(last_touched).num_minutes() < SESSION_LIVE_TTL_MINS
}

/// [`session_alive`] for a meta read off disk. An unparseable `last_touched`
/// is treated as dead, matching what `alive_peers` does with the same record.
pub fn meta_alive(meta: &SessionMeta) -> bool {
    meta.last_touched
        .parse::<DateTime<Utc>>()
        .is_ok_and(|t| session_alive(meta.pid, meta.watcher_pid, t, Utc::now()))
}

pub fn pid_alive(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    #[cfg(unix)]
    {
        use nix::sys::signal;
        use nix::unistd::Pid;
        signal::kill(Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(windows)]
    {
        use std::ffi::c_void;

        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;

        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(
                desired_access: u32,
                inherit_handle: i32,
                process_id: u32,
            ) -> *mut c_void;
            fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
            fn CloseHandle(object: *mut c_void) -> i32;
        }

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }

        let mut exit_code = 0;
        let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        unsafe {
            CloseHandle(handle);
        }
        ok != 0 && exit_code == STILL_ACTIVE
    }
}
