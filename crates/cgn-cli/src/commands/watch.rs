//! `cgn watch` CLI surface.

use crate::peer::watcher::{run_watcher, WatcherCfg};
use crate::session::resolver::resolve_session_id;
use clap::Args;
use cgn_core::peer::registry::pid_alive;
use cgn_core::session::SessionMeta;
use cgn_core::GnxError;
use std::path::PathBuf;

fn default_repo_root() -> std::io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(&cwd)?;
    Ok(cgn_core::registry::resolve_home_gnx().join(repo_dir))
}

#[derive(Args, Debug, Clone)]
pub struct WatchArgs {
    #[arg(long)]
    pub start: bool,
    #[arg(long)]
    pub stop: bool,
    #[arg(long)]
    pub status: bool,
    #[arg(long)]
    pub foreground: bool,
    #[arg(long)]
    pub repo: Option<PathBuf>,
}

pub fn run(args: WatchArgs) -> Result<(), GnxError> {
    let repo_root = match args.repo.clone() {
        Some(p) => p,
        None => default_repo_root()?,
    };
    let session_id = resolve_session_id(None);
    let session_dir = repo_root.join("sessions").join(&session_id);
    std::fs::create_dir_all(&session_dir)?;

    match (args.start, args.stop, args.status, args.foreground) {
        (_, _, _, true) => start_foreground(repo_root, session_id, session_dir),
        (true, false, false, false) => start_background(repo_root, session_id, session_dir),
        (false, true, false, false) => stop_watcher(&session_dir),
        (false, false, true, false) => print_status(&session_dir),
        _ => Err(GnxError::InvalidArgument(
            "specify exactly one of --start | --stop | --status | --foreground".into(),
        )),
    }
}

fn start_foreground(repo_root: PathBuf, sid: String, session_dir: PathBuf) -> Result<(), GnxError> {
    if std::env::var("CGN_TEST_EXIT_AFTER_INIT").is_ok() {
        eprintln!("[cgn watch] test mode — exiting after init");
        return Ok(());
    }
    let cfg = WatcherCfg {
        repo_root,
        my_session_id: sid,
        my_session_dir: session_dir.clone(),
        lock_path: session_dir.join("watcher.lock"),
    };
    run_watcher(cfg).map_err(|e| GnxError::Io(e))
}

#[cfg(unix)]
fn start_background(repo_root: PathBuf, sid: String, session_dir: PathBuf) -> Result<(), GnxError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let watcher_log = session_dir.join("watcher.log");
    let log_writer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&watcher_log)?;
    let log_writer2 = log_writer.try_clone()?;
    let exe = std::env::current_exe()?;
    let child = unsafe {
        Command::new(exe)
            .args([
                "watch",
                "--foreground",
                "--repo",
                repo_root.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_writer))
            .stderr(Stdio::from(log_writer2))
            .pre_exec(|| {
                nix::unistd::setsid().map_err(|e| std::io::Error::other(format!("setsid: {e}")))?;
                Ok(())
            })
            .spawn()?
    };
    let pid = child.id();
    let meta_path = session_dir.join("meta.json");
    let mut meta = SessionMeta::read(&meta_path).unwrap_or_else(|_| SessionMeta {
        version: 1,
        session_id: sid.clone(),
        pid: None,
        started_at: chrono::Utc::now().to_rfc3339(),
        last_touched: chrono::Utc::now().to_rfc3339(),
        base_sha: "0".repeat(40),
        source_worktree: String::new(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    });
    meta.watcher_pid = Some(pid);
    SessionMeta::write_atomic(&meta_path, &meta)?;
    eprintln!("[cgn watch] forked watcher pid={pid}, sid={sid}");
    Ok(())
}

#[cfg(not(unix))]
fn start_background(_: PathBuf, _: String, _: PathBuf) -> Result<(), GnxError> {
    Err(GnxError::InvalidArgument(
        "background watch not yet supported on this platform; use --foreground".into(),
    ))
}

fn stop_watcher(session_dir: &std::path::Path) -> Result<(), GnxError> {
    let meta_path = session_dir.join("meta.json");
    let mut meta = match SessionMeta::read(&meta_path) {
        Ok(m) => m,
        Err(_) => {
            println!("no watcher running");
            return Ok(());
        }
    };
    let Some(pid) = meta.watcher_pid else {
        println!("no watcher running");
        return Ok(());
    };
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
    }
    meta.watcher_pid = None;
    SessionMeta::write_atomic(&meta_path, &meta)?;
    println!("watcher pid={pid} signalled SIGTERM");
    Ok(())
}

fn print_status(session_dir: &std::path::Path) -> Result<(), GnxError> {
    let meta_path = session_dir.join("meta.json");
    let meta = SessionMeta::read(&meta_path).ok();
    match meta.and_then(|m| m.watcher_pid) {
        Some(pid) if pid_alive(pid) => println!("watcher running pid={pid}"),
        Some(pid) => println!("watcher pid={pid} dead (stale), no watcher"),
        None => println!("no watcher (not running)"),
    }
    let log = session_dir.join("watcher.log");
    if let Ok(content) = std::fs::read_to_string(&log) {
        println!("--- watcher.log tail ---");
        let lines: Vec<&str> = content.lines().collect();
        for line in lines.iter().rev().take(5).rev() {
            println!("{line}");
        }
    }
    Ok(())
}
