//! Watcher main loop: inotify-driven peer-dirty fan-in.
//!
//! Lifecycle: run_watcher() blocks until SIGTERM. flock(watcher.lock)
//! ensures single instance per session. Fail-open: any handler error is
//! logged with backtrace and the loop continues.

use crate::peer::dispatch::dispatch_peer_dirty_event;
use chrono::Utc;
use ecp_core::peer::concern::ImpactCache;
use ecp_core::peer::registry::alive_peers;
use ecp_core::session::overlay::DirtyFiles;
use ecp_core::session::SessionMeta;
use fs2::FileExt;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct WatcherCfg {
    pub repo_root: PathBuf,
    pub my_session_id: String,
    pub my_session_dir: PathBuf,
    pub lock_path: PathBuf,
}

pub fn run_watcher(cfg: WatcherCfg) -> std::io::Result<()> {
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&cfg.lock_path)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::AlreadyExists, e))?;
    tracing::info!(
        pid = std::process::id(),
        session = %cfg.my_session_id,
        "watcher acquired flock"
    );

    let cache = Arc::new(Mutex::new(rebuild_impact_cache(&cfg.my_session_dir)));
    // Cached copy of our own dirty symbols. Invalidated whenever our own
    // dirty_files.json changes (same trigger as impact_cache), avoiding N
    // reads of the same file when N peers fire dirty events in a burst.
    let my_dirty_cache: Arc<Mutex<Option<Vec<ecp_core::session::overlay::SymbolRef>>>> =
        Arc::new(Mutex::new(None));

    let (tx, rx) = channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(tx).map_err(std::io::Error::other)?;
    let sessions_dir = cfg.repo_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    watcher
        .watch(&sessions_dir, RecursiveMode::Recursive)
        .map_err(std::io::Error::other)?;

    let mut event_count: u32 = 0;
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(ev)) => {
                event_count = event_count.wrapping_add(1);
                if let Err(e) = handle_event(&cfg, &cache, &my_dirty_cache, ev) {
                    log_watcher_error("event handler", &e);
                }
            }
            Ok(Err(e)) => log_watcher_error("notify error", &e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if event_count.is_multiple_of(ecp_core::peer::retention::ROTATE_CHECK_EVERY_N_EVENTS) {
            let _ = ecp_core::peer::retention::rotate_if_needed(
                &cfg.my_session_dir.join("msg.log"),
                ecp_core::peer::retention::MSG_LOG_ROTATE_BYTES,
                ecp_core::peer::retention::MSG_LOG_KEEP_ROTATED,
            );
            let _ = ecp_core::peer::retention::rotate_if_needed(
                &cfg.my_session_dir.join("watcher.log"),
                ecp_core::peer::retention::WATCHER_LOG_ROTATE_BYTES,
                ecp_core::peer::retention::WATCHER_LOG_KEEP_ROTATED,
            );
        }
    }
    Ok(())
}

fn handle_event(
    cfg: &WatcherCfg,
    cache: &Arc<Mutex<ImpactCache>>,
    my_dirty_cache: &Arc<Mutex<Option<Vec<ecp_core::session::overlay::SymbolRef>>>>,
    ev: Event,
) -> std::io::Result<()> {
    if !matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
        return Ok(());
    }
    for path in &ev.paths {
        if !path.ends_with("dirty_files.json") {
            continue;
        }
        let Some(sid) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
        else {
            continue;
        };
        if sid == cfg.my_session_id {
            let mut c = cache.lock().expect("impact cache lock poisoned");
            *c = rebuild_impact_cache(&cfg.my_session_dir);
            // Invalidate my_dirty_cache so next dispatch_peer re-reads the updated file.
            *my_dirty_cache.lock().expect("my_dirty_cache lock poisoned") = None;
            continue;
        }
        dispatch_peer(cfg, cache, my_dirty_cache, sid, path)?;
    }
    Ok(())
}

fn dispatch_peer(
    cfg: &WatcherCfg,
    cache: &Arc<Mutex<ImpactCache>>,
    my_dirty_cache: &Arc<Mutex<Option<Vec<ecp_core::session::overlay::SymbolRef>>>>,
    peer_sid: &str,
    peer_dirty_path: &Path,
) -> std::io::Result<()> {
    let peer_dirty = DirtyFiles::read(peer_dirty_path)?;
    // Populate cache on first call after invalidation; reuse across burst of peer events.
    let my_dirty = {
        let mut guard = my_dirty_cache.lock().expect("my_dirty_cache lock poisoned");
        if guard.is_none() {
            *guard = Some(
                DirtyFiles::read(&cfg.my_session_dir.join("dirty_files.json"))
                    .map(|d| {
                        d.entries
                            .into_values()
                            .flat_map(|e| e.dirty_symbols)
                            .collect()
                    })
                    .unwrap_or_default(),
            );
        }
        guard.clone().unwrap_or_default()
    };
    let peer_meta = SessionMeta::read(&peer_dirty_path.with_file_name("meta.json"))?;
    let peer_pid = peer_meta.pid.unwrap_or(0);
    let ts = Utc::now().to_rfc3339();
    let cache_guard = cache.lock().expect("impact cache lock poisoned");
    for entry in peer_dirty.entries.values() {
        dispatch_peer_dirty_event(
            &cfg.my_session_dir,
            peer_sid,
            peer_pid,
            &ts,
            entry,
            &my_dirty,
            &cache_guard,
        )?;
    }
    Ok(())
}

fn rebuild_impact_cache(my_session_dir: &Path) -> ImpactCache {
    // v1 stub: real implementation queries the graph for IMPACT(my_dirty_symbols).
    // Empty cache means SOFT detection requires explicit refresh by an external
    // engine; HARD detection (same symbol intersection) still works correctly.
    // Wiring to graph engine deferred per spec §17.
    let _ = my_session_dir;
    ImpactCache::default()
}

fn log_watcher_error(context: &str, err: &dyn std::fmt::Debug) {
    use std::backtrace::Backtrace;
    let bt = Backtrace::capture();
    tracing::error!(context, ?err, "watcher loop error");
    eprintln!("[watcher] error in {context}: {err:?}\nbacktrace:\n{bt}");
}

/// Documented API scaffold for the multi-agent peer-sync plan
/// (`docs/superpowers/plans/2026-05-17-multi-agent-peer-sync.md` §1938).
/// Not yet wired into the watcher loop — kept as the stable surface that
/// consumers reach for to ask "which peer sessions are live?" without
/// reaching into `alive_peers`' raw `PeerInfo` shape.
#[allow(dead_code)]
pub fn alive_peer_sessions(repo_root: &Path, exclude_self: &str) -> Vec<String> {
    alive_peers(repo_root, exclude_self)
        .into_iter()
        .map(|p| p.session_id)
        .collect()
}
