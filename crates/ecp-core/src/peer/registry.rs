//! Enumerate alive peer sessions sharing the same repo `common_dir`.

use crate::session::SessionMeta;
use chrono::{DateTime, Utc};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct PeerSession {
    pub session_id: String,
    pub pid: u32,
    pub last_touched: DateTime<Utc>,
    pub base_sha: String,
    pub watcher_alive: bool,
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
        if id.is_empty() || id == exclude_self || id.starts_with('.') {
            continue;
        }
        let meta_path = path.join("meta.json");
        let Ok(meta) = SessionMeta::read(&meta_path) else {
            continue;
        };
        let Some(pid) = meta.pid else { continue };
        if !pid_alive(pid) {
            continue;
        }
        let Ok(last_touched) = meta.last_touched.parse::<DateTime<Utc>>() else {
            continue;
        };
        let watcher_alive = meta.watcher_pid.is_some_and(pid_alive);
        out.push(PeerSession {
            session_id: id.to_string(),
            pid,
            last_touched,
            base_sha: meta.base_sha,
            watcher_alive,
        });
    }
    out
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
