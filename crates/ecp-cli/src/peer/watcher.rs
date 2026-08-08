//! Watcher main loop: inotify-driven peer-dirty fan-in.
//!
//! Lifecycle: run_watcher() blocks until SIGTERM. flock(watcher.lock)
//! ensures single instance per session. Fail-open: any handler error is
//! logged with backtrace and the loop continues.

use crate::commands::impact::run_for_symbol;
use crate::peer::dispatch::dispatch_peer_dirty_event;
use chrono::Utc;
use ecp_core::peer::concern::ImpactCache;
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

    let soft = Arc::new(Mutex::new(SoftState::default()));
    soft.lock()
        .expect("soft state lock poisoned")
        .rebuild(&cfg.my_session_dir);
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
                if let Err(e) = handle_event(&cfg, &soft, &my_dirty_cache, ev) {
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
    soft: &Arc<Mutex<SoftState>>,
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
            let prev: Option<Vec<ecp_core::session::overlay::SymbolRef>> = {
                let mut s = soft.lock().expect("soft state lock poisoned");
                s.rebuild(&cfg.my_session_dir);
                // Invalidate my_dirty_cache so next dispatch_peer re-reads the updated file.
                my_dirty_cache
                    .lock()
                    .expect("my_dirty_cache lock poisoned")
                    .take()
            };
            // Concerns are otherwise edge-triggered on PEER writes only: a
            // peer event that arrived while our dirty set was still empty was
            // classified Ignore and never re-evaluated — so the last session
            // to go dirty would hear nothing (8-session audit: delivery
            // staircase ending at 0). Our own dirty change is the moment new
            // overlaps can appear; rescan every peer against the new set —
            // but only when the set actually GAINED symbols, else every
            // re-save of the same symbol would spam N duplicate concerns
            // into our inbox between hook drains.
            if my_dirty_gained_symbols(&cfg.my_session_dir, prev.as_deref()) {
                rescan_peers(cfg, soft, my_dirty_cache);
            }
            continue;
        }
        dispatch_peer(cfg, soft, my_dirty_cache, sid, path)?;
    }
    Ok(())
}

/// True when the current dirty set contains a symbol name absent from the
/// previous cached set. `prev = None` (startup / first write) counts as
/// gained — rescanning once too often is safe; missing the first transition
/// recreates the late-writer blind spot.
fn my_dirty_gained_symbols(
    my_session_dir: &Path,
    prev: Option<&[ecp_core::session::overlay::SymbolRef]>,
) -> bool {
    let Some(prev) = prev else {
        return true;
    };
    let Ok(now) = DirtyFiles::read(&my_session_dir.join("dirty_files.json")) else {
        return false;
    };
    let prev_names: std::collections::HashSet<&str> =
        prev.iter().map(|s| s.name.as_str()).collect();
    now.entries
        .values()
        .flat_map(|e| &e.dirty_symbols)
        .any(|s| !prev_names.contains(s.name.as_str()))
}

/// Fail-open per peer: one unreadable peer must not block the rest.
fn rescan_peers(
    cfg: &WatcherCfg,
    soft: &Arc<Mutex<SoftState>>,
    my_dirty_cache: &Arc<Mutex<Option<Vec<ecp_core::session::overlay::SymbolRef>>>>,
) {
    let sessions_dir = cfg.repo_root.join("sessions");
    let Ok(read) = std::fs::read_dir(&sessions_dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(sid) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if sid == cfg.my_session_id || sid.starts_with('.') || sid.contains(".stale-") {
            continue;
        }
        let dirty_path = path.join("dirty_files.json");
        if !dirty_path.exists() {
            continue;
        }
        if let Err(e) = dispatch_peer(cfg, soft, my_dirty_cache, sid, &dirty_path) {
            log_watcher_error("rescan dispatch", &e);
        }
    }
}

fn dispatch_peer(
    cfg: &WatcherCfg,
    soft: &Arc<Mutex<SoftState>>,
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
    let peer_meta = SessionMeta::read(&peer_dirty_path.with_file_name("session_meta.json"))?;
    let peer_pid = peer_meta.pid.unwrap_or(0);
    let ts = Utc::now().to_rfc3339();
    let soft_guard = soft.lock().expect("soft state lock poisoned");
    for entry in peer_dirty.entries.values() {
        dispatch_peer_dirty_event(
            &cfg.my_session_dir,
            peer_sid,
            peer_pid,
            peer_meta.agent_name.as_deref(),
            &ts,
            entry,
            &my_dirty,
            &soft_guard.cache,
        )?;
    }
    Ok(())
}

/// Blast-radius depth behind SOFT. Depth 1 misses the caller-of-a-caller
/// collisions agents actually hit; past 2 the impacted-name set grows until
/// nearly every peer edit classifies SOFT and the signal drowns in its own
/// noise. `peers plan` keeps the wider default (5) because a lead reads that
/// answer once, deliberately, rather than on every save.
const SOFT_IMPACT_DEPTH: u32 = 2;

/// Seed symbols traversed per rebuild. The rebuild runs on every write to our
/// own dirty file, so its cost stays bounded however large the working set
/// grows; the symbols an agent edits together are near each other in the
/// graph, so the truncated tail mostly duplicates radii already covered.
const SOFT_IMPACT_MAX_SEEDS: usize = 32;

/// Impacted names retained. A hub symbol in a large repo can reach six figures
/// of nodes at depth 2; the watcher is a background daemon that must stay small.
const SOFT_IMPACT_MAX_NAMES: usize = 20_000;

/// Graph handle backing SOFT classification.
///
/// Loaded at most once per watcher and reused for every rebuild: the daemon
/// lives for the whole coding session, so re-resolving and re-mmapping the
/// graph on each save would put repeated I/O behind an ordinary file write.
/// The trade is that a graph published mid-session stays unseen until the
/// watcher restarts — acceptable for a heuristic whose miss mode is "no SOFT
/// hint", and the HARD path never consults it.
struct ImpactSource {
    engine: crate::engine::Engine,
    member_repo: String,
}

/// Everything the SOFT half of `classify` reads: the impacted-name set, and
/// the graph it was derived from.
#[derive(Default)]
struct SoftState {
    cache: ImpactCache,
    source: ImpactSlot,
}

/// One-shot lazy slot. `attempted` distinguishes "not loaded yet" from "load
/// failed", so a session whose repo has no published graph pays the resolution
/// cost once and then runs HARD-only.
#[derive(Default)]
struct ImpactSlot {
    attempted: bool,
    source: Option<ImpactSource>,
}

impl ImpactSlot {
    fn get(&mut self, my_session_dir: &Path) -> Option<&ImpactSource> {
        if !self.attempted {
            self.attempted = true;
            self.source = load_impact_source(my_session_dir);
        }
        self.source.as_ref()
    }
}

impl SoftState {
    /// Recompute IMPACT(my dirty symbols). Every failure path leaves the cache
    /// empty, which degrades to the HARD-only behaviour this watcher had before
    /// the graph was wired in.
    fn rebuild(&mut self, my_session_dir: &Path) {
        self.cache.invalidate();
        let seeds = seed_symbols(my_session_dir);
        if seeds.is_empty() {
            return;
        }
        let Some(source) = self.source.get(my_session_dir) else {
            return;
        };
        self.cache.refresh(impacted_names(source, &seeds));
    }
}

/// Resolve the session's own worktree, then open the graph published for it.
///
/// Deliberately `Engine::load` rather than `load_ensured`: the watcher is a
/// passive observer of other sessions and must never kick off an index build
/// behind the agent's back — a background rebuild fired from here would land
/// on the same disk as the edit that triggered it.
fn load_impact_source(my_session_dir: &Path) -> Option<ImpactSource> {
    let meta = SessionMeta::read(&my_session_dir.join("session_meta.json")).ok()?;
    if meta.source_worktree.is_empty() {
        return None;
    }
    let worktree = PathBuf::from(&meta.source_worktree);
    let member_repo = crate::repo_identity::repo_dir_name_for_cwd(&worktree).ok()?;
    let graph_path = crate::graph_path::resolve(Path::new(".ecp/graph.bin"), &worktree);
    let engine = crate::engine::Engine::load(&graph_path).ok()?;
    tracing::info!(graph = %graph_path.display(), "watcher attached graph for SOFT concerns");
    Some(ImpactSource {
        engine,
        member_repo,
    })
}

/// Distinct dirty symbol names, capped. Order follows `DirtyFiles`' BTreeMap,
/// so the surviving prefix is stable across rebuilds instead of shuffling on
/// every save.
fn seed_symbols(my_session_dir: &Path) -> Vec<String> {
    let Ok(dirty) = DirtyFiles::read(&my_session_dir.join("dirty_files.json")) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut seeds = Vec::new();
    for symbol in dirty.entries.values().flat_map(|e| &e.dirty_symbols) {
        if seeds.len() >= SOFT_IMPACT_MAX_SEEDS {
            break;
        }
        if seen.insert(symbol.name.as_str()) {
            seeds.push(symbol.name.clone());
        }
    }
    seeds
}

/// Union of every seed's blast radius. A seed the graph can't resolve
/// (ambiguous bare name, symbol newer than the graph) is skipped: one
/// unresolvable name must not cost the others their SOFT coverage.
fn impacted_names(source: &ImpactSource, seeds: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for seed in seeds {
        let Ok(local) = run_for_symbol(
            &source.engine,
            &source.member_repo,
            seed,
            "both",
            Some(SOFT_IMPACT_DEPTH),
            None,
            false,
        ) else {
            continue;
        };
        collect_impact_names(local.as_json(), &mut names);
        if names.len() >= SOFT_IMPACT_MAX_NAMES {
            break;
        }
    }
    names
}

/// Names reached by one impact traversal. The seed sits at depth 0 and is kept:
/// a peer touching it is already HARD, and `classify` checks HARD first.
fn collect_impact_names(payload: &serde_json::Value, out: &mut Vec<String>) {
    let Some(items) = payload["impact"].as_array() else {
        return;
    };
    out.extend(
        items
            .iter()
            .filter_map(|item| item["name"].as_str())
            .map(str::to_string),
    );
}

fn log_watcher_error(context: &str, err: &dyn std::fmt::Debug) {
    use std::backtrace::Backtrace;
    let bt = Backtrace::capture();
    tracing::error!(context, ?err, "watcher loop error");
    eprintln!("[watcher] error in {context}: {err:?}\nbacktrace:\n{bt}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::session::overlay::{DirtyEntry, SymbolKind, SymbolRef};
    use std::collections::BTreeMap;

    fn write_dirty(dir: &Path, symbols: &[&str]) {
        let mut entries = BTreeMap::new();
        entries.insert(
            "src/a.rs".to_string(),
            DirtyEntry {
                mtime_ns: 1,
                content_hash: "h".into(),
                fragment_id: "f".into(),
                tantivy_delta_segment: None,
                parse_failed: false,
                format: 1,
                dirty_symbols: symbols
                    .iter()
                    .map(|n| SymbolRef {
                        name: (*n).into(),
                        kind: SymbolKind::Function,
                        file: "src/a.rs".into(),
                        line_start: 1,
                        line_end: 2,
                    })
                    .collect(),
            },
        );
        DirtyFiles::write_atomic(
            &dir.join("dirty_files.json"),
            &DirtyFiles {
                version: 1,
                entries,
            },
        )
        .expect("write dirty_files.json");
    }

    #[test]
    fn seed_symbols_dedupes_repeated_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_dirty(dir.path(), &["foo", "bar", "foo"]);
        assert_eq!(seed_symbols(dir.path()), vec!["foo", "bar"]);
    }

    #[test]
    fn seed_symbols_caps_the_traversal_at_the_seed_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let many: Vec<String> = (0..SOFT_IMPACT_MAX_SEEDS * 3)
            .map(|i| format!("sym{i}"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        write_dirty(dir.path(), &refs);
        assert_eq!(seed_symbols(dir.path()).len(), SOFT_IMPACT_MAX_SEEDS);
    }

    /// A session with no dirty file at all is the startup state — it must not
    /// look like "everything is impacted".
    #[test]
    fn seed_symbols_without_a_dirty_file_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(seed_symbols(dir.path()).is_empty());
    }

    #[test]
    fn collect_impact_names_takes_every_reached_node() {
        let payload = serde_json::json!({
            "impact": [
                {"name": "seed", "depth": 0},
                {"name": "caller", "depth": 1},
                {"name": "callee", "depth": 2},
            ]
        });
        let mut out = Vec::new();
        collect_impact_names(&payload, &mut out);
        assert_eq!(out, vec!["seed", "caller", "callee"]);
    }

    /// `run_for_symbol` answers an unresolvable target with an `error` payload
    /// rather than `Err`. Reading names off it must yield nothing — a bogus
    /// entry here would classify an unrelated peer edit as SOFT.
    #[test]
    fn collect_impact_names_on_an_error_payload_adds_nothing() {
        let payload = serde_json::json!({"error": "No symbol named `nope`"});
        let mut out = Vec::new();
        collect_impact_names(&payload, &mut out);
        assert!(out.is_empty(), "error payload leaked names: {out:?}");
    }

    /// No graph, no dirty set: `rebuild` has to leave the cache empty so
    /// `classify` falls back to HARD-only instead of firing on every peer edit.
    #[test]
    fn rebuild_without_a_graph_leaves_the_cache_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_dirty(dir.path(), &["foo"]);
        let mut state = SoftState::default();
        state.rebuild(dir.path());
        assert!(!state.cache.contains("foo"));
    }
}
