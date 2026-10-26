//! Watcher main loop: inotify-driven peer-dirty fan-in.
//!
//! Lifecycle: run_watcher() blocks until SIGTERM. flock(watcher.lock)
//! ensures single instance per session. Fail-open: any handler error is
//! logged with backtrace and the loop continues.

use crate::commands::impact::run_for_symbol;
use crate::peer::dispatch::dispatch_peer_dirty_event;
use chrono::Utc;
use ecp_core::peer::concern::{ImpactCache, SymbolKey};
use ecp_core::session::layout;
use ecp_core::session::{DirtyEvents, SessionMeta};
use fs2::FileExt;
use notify::event::{DataChange, ModifyKind};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use rustc_hash::FxHashSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Our own manifest, split the way `classify` consumes it: entry keys decide
/// HARD, declaration lists feed SOFT.
#[derive(Default, Clone)]
struct MyDirtySurface {
    files: Vec<String>,
    symbols: Vec<ecp_core::session::overlay::SymbolRef>,
}

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
    // l1/dirty_events.json changes (same trigger as impact_cache), avoiding N
    // reads of the same file when N peers fire dirty events in a burst.
    let my_dirty_cache: Arc<Mutex<Option<MyDirtySurface>>> = Arc::new(Mutex::new(None));

    let (tx, rx) = channel::<notify::Result<Event>>();
    let mut watcher = open_watcher(tx, notify::recommended_watcher)?;
    let sessions_dir = cfg.repo_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    watcher
        .watch(&sessions_dir, RecursiveMode::Recursive)
        .map_err(std::io::Error::other)?;

    // Stamped before the rescan: a manifest written in between is delivered
    // twice rather than never.
    let mut stamps = ManifestStamps::default();
    stamps.changed(&sessions_dir);
    // Concerns are edge-triggered on writes, so every peer that went dirty
    // before this point is invisible until it happens to save again. That is
    // the normal case for a watcher restarted mid-session, and for the window
    // between process start and the `watch` call above.
    rescan_peers(&cfg, &soft, &my_dirty_cache);

    let mut event_count: u32 = 0;
    let mut last_reconcile = Instant::now();
    loop {
        let received = rx.recv_timeout(RECONCILE_EVERY);
        let timed_out = matches!(received, Err(std::sync::mpsc::RecvTimeoutError::Timeout));
        match received {
            Ok(Ok(ev)) => {
                event_count = event_count.wrapping_add(1);
                if let Err(e) = handle_event(&cfg, &soft, &my_dirty_cache, &mut stamps, ev) {
                    log_watcher_error("event handler", &e);
                }
            }
            Ok(Err(e)) => log_watcher_error("notify error", &e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if last_reconcile.elapsed() >= RECONCILE_EVERY {
            last_reconcile = Instant::now();
            for manifest in stamps.changed(&sessions_dir) {
                let ev = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                    .add_path(manifest);
                if let Err(e) = handle_event(&cfg, &soft, &my_dirty_cache, &mut stamps, ev) {
                    log_watcher_error("reconcile", &e);
                }
            }
        }
        if timed_out {
            continue;
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
    my_dirty_cache: &Arc<Mutex<Option<MyDirtySurface>>>,
    stamps: &mut ManifestStamps,
    ev: Event,
) -> std::io::Result<()> {
    if !matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
        return Ok(());
    }
    let created = matches!(ev.kind, EventKind::Create(_));
    for path in &ev.paths {
        let Some(session_dir) = event_session_dir(path, created) else {
            continue;
        };
        stamps.record(&layout::dirty_events_path(session_dir));
        let Some(sid) = session_dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if sid == cfg.my_session_id {
            let prev: Option<MyDirtySurface> = {
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
            if my_dirty_gained_symbols(&cfg.my_session_dir, prev.as_ref()) {
                rescan_peers(cfg, soft, my_dirty_cache);
            }
            continue;
        }
        dispatch_peer(cfg, soft, my_dirty_cache, sid, session_dir)?;
    }
    Ok(())
}

/// The session whose dirty set an event on `path` changed. `created` is true
/// for a create event.
fn event_session_dir(path: &Path, created: bool) -> Option<&Path> {
    // `<sessions>/<sid>/l1/dirty_events.json`: the legacy manifest an older
    // binary writes is not today's dirty set.
    manifest_session_dir(path).or_else(|| {
        (created && path.is_dir())
            .then(|| created_dir_session(path))
            .flatten()
    })
}

/// The session dir whose manifest `path` is, or `None` for any other path.
fn manifest_session_dir(path: &Path) -> Option<&Path> {
    let session_dir = path.parent()?.parent()?;
    (layout::dirty_events_path(session_dir) == path).then_some(session_dir)
}

/// The session dir of a newly created `<sid>` or `<sid>/l1` that already
/// holds a manifest. inotify watches a new directory only after its create
/// event, so a manifest written right after `l1/` is made raises no event of
/// its own; without this, a session's first dirty write never reaches peers.
fn created_dir_session(dir: &Path) -> Option<&Path> {
    [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .find(|d| layout::dirty_events_path(d).is_file())
}

/// True when the current dirty set contains a `(file, name)` absent from the
/// previous cached set. `prev = None` (startup / first write) counts as
/// gained — rescanning once too often is safe; missing the first transition
/// recreates the late-writer blind spot.
///
/// Keyed on the pair, not the name: going dirty on `run` in a second file has
/// to count as a gain, or the new file's overlap never reaches our inbox.
fn my_dirty_gained_symbols(my_session_dir: &Path, prev: Option<&MyDirtySurface>) -> bool {
    let Some(prev) = prev else {
        return true;
    };
    let Ok(now) = DirtyEvents::read(&layout::dirty_events_path(my_session_dir)) else {
        return false;
    };
    // A newly dirty FILE counts even when it declares nothing, since that is
    // exactly what HARD keys on now.
    let prev_files: std::collections::HashSet<&str> =
        prev.files.iter().map(String::as_str).collect();
    if now.entries.keys().any(|f| !prev_files.contains(f.as_str())) {
        return true;
    }
    let prev_keys: std::collections::HashSet<(&str, &str)> = prev
        .symbols
        .iter()
        .map(|s| (s.file.as_str(), s.name.as_str()))
        .collect();
    now.entries
        .values()
        .flat_map(|e| &e.dirty_symbols)
        .any(|s| !prev_keys.contains(&(s.file.as_str(), s.name.as_str())))
}

/// How often every manifest's stamp is re-read. inotify reports nothing for a
/// manifest written before the watch on its new `l1/` exists, and drops events
/// when its queue overflows; the stamp comparison delivers those writes late
/// instead of never.
const RECONCILE_EVERY: Duration = Duration::from_secs(1);

/// `(mtime, len)` of each session manifest as last handled.
#[derive(Default)]
struct ManifestStamps(rustc_hash::FxHashMap<PathBuf, (std::time::SystemTime, u64)>);

impl ManifestStamps {
    fn stamp(manifest: &Path) -> Option<(std::time::SystemTime, u64)> {
        let meta = std::fs::metadata(manifest).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    fn record(&mut self, manifest: &Path) {
        match Self::stamp(manifest) {
            Some(stamp) => self.0.insert(manifest.to_path_buf(), stamp),
            None => self.0.remove(manifest),
        };
    }

    /// Manifests under `sessions_dir` whose stamp differs from the recorded
    /// one. Records the new stamps.
    fn changed(&mut self, sessions_dir: &Path) -> Vec<PathBuf> {
        let Ok(read) = std::fs::read_dir(sessions_dir) else {
            return Vec::new();
        };
        read.flatten()
            .filter_map(|entry| {
                let manifest = layout::dirty_events_path(&entry.path());
                let stamp = Self::stamp(&manifest)?;
                (self.0.insert(manifest.clone(), stamp) != Some(stamp)).then_some(manifest)
            })
            .collect()
    }
}

/// Poll period when the native watcher cannot start.
const POLL_FALLBACK_INTERVAL: Duration = Duration::from_millis(500);

/// The native watcher fails to start once the user's inotify instances run
/// out (`fs.inotify.max_user_instances`, 128 by default on Linux); without a
/// fallback this session would never hear from its peers again.
fn open_watcher(
    tx: Sender<notify::Result<Event>>,
    native: impl FnOnce(Sender<notify::Result<Event>>) -> notify::Result<notify::RecommendedWatcher>,
) -> std::io::Result<Box<dyn Watcher>> {
    match native(tx.clone()) {
        Ok(w) => Ok(Box::new(w)),
        Err(e) => {
            tracing::warn!(error = %e, "native file watcher unavailable, polling instead");
            let cfg = notify::Config::default().with_poll_interval(POLL_FALLBACK_INTERVAL);
            let poll = notify::PollWatcher::new(tx, cfg).map_err(std::io::Error::other)?;
            Ok(Box::new(poll))
        }
    }
}

/// Fail-open per peer: one unreadable peer must not block the rest.
fn rescan_peers(
    cfg: &WatcherCfg,
    soft: &Arc<Mutex<SoftState>>,
    my_dirty_cache: &Arc<Mutex<Option<MyDirtySurface>>>,
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
        if !layout::dirty_events_path(&path).exists() {
            continue;
        }
        if let Err(e) = dispatch_peer(cfg, soft, my_dirty_cache, sid, &path) {
            log_watcher_error("rescan dispatch", &e);
        }
    }
}

fn dispatch_peer(
    cfg: &WatcherCfg,
    soft: &Arc<Mutex<SoftState>>,
    my_dirty_cache: &Arc<Mutex<Option<MyDirtySurface>>>,
    peer_sid: &str,
    peer_session_dir: &Path,
) -> std::io::Result<()> {
    let peer_dirty = DirtyEvents::read(&layout::dirty_events_path(peer_session_dir))?;
    // Populate cache on first call after invalidation; reuse across burst of peer events.
    let (my_files, my_dirty) = {
        let mut guard = my_dirty_cache.lock().expect("my_dirty_cache lock poisoned");
        if guard.is_none() {
            *guard = Some(
                DirtyEvents::read(&layout::dirty_events_path(&cfg.my_session_dir))
                    .map(|d| {
                        // Entry KEYS decide HARD, so they are read directly —
                        // a file whose parse yielded no declarations still
                        // counts, and `dirty_symbols` would have dropped it.
                        let files: Vec<String> = d.entries.keys().cloned().collect();
                        let symbols: Vec<ecp_core::session::overlay::SymbolRef> = d
                            .entries
                            .into_values()
                            .flat_map(|e| e.dirty_symbols)
                            .collect();
                        MyDirtySurface { files, symbols }
                    })
                    .unwrap_or_default(),
            );
        }
        let surface = guard.clone().unwrap_or_default();
        (surface.files, surface.symbols)
    };
    let peer_meta = SessionMeta::read(&peer_session_dir.join("session_meta.json"))?;
    // A session that crashed leaves its dirty manifest behind. On the
    // event-driven path a write proves the peer is alive, but the startup
    // rescan walks the directory unconditionally and would replay a dead
    // session's file as a live concern — an alarm `main` never raises.
    //
    // Only PROVABLY dead peers are skipped, and a dead pid alone does not
    // prove it: the recorded pid is the one-shot `ecp` process that enrolled
    // the session, so probing it rejected every peer that has ever existed
    // (FU-2026-06-10-cc120f78889c).
    if !ecp_core::peer::registry::meta_alive(&peer_meta) {
        return Ok(());
    }
    let peer_pid = peer_meta.pid.unwrap_or(0);
    let ts = Utc::now().to_rfc3339();
    let soft_guard = soft.lock().expect("soft state lock poisoned");
    for (peer_file, entry) in &peer_dirty.entries {
        dispatch_peer_dirty_event(
            &cfg.my_session_dir,
            peer_sid,
            peer_pid,
            peer_meta.agent_name.as_deref(),
            &ts,
            peer_file,
            entry,
            &my_files,
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
/// Held for the watcher's life once opened: the daemon runs for a whole coding
/// session, so re-mmapping the graph on each save would put repeated I/O behind
/// an ordinary file write. The trade is that a graph published mid-session
/// stays unseen until the watcher restarts — acceptable for a heuristic whose
/// miss mode is "no SOFT hint", and the HARD path never consults it.
struct ImpactSource {
    engine: crate::engine::Engine,
    member_repo: String,
}

/// One dirty symbol to traverse from. The file qualifies the name: bare
/// `run` / `new` / `handle` match many definitions, and `run_for_symbol`
/// rejects an ambiguous target outright — which would silently drop SOFT
/// coverage for exactly the names agents edit most.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seed {
    name: String,
    file: String,
}

/// Everything the SOFT half of `classify` reads: the impacted-name set, the
/// graph it was derived from, and the seeds it was derived from.
#[derive(Default)]
struct SoftState {
    cache: ImpactCache,
    source: Option<ImpactSource>,
    /// Seeds behind the current cache. An agent iterating inside one function
    /// rewrites `l1/dirty_events.json` on every save without changing which symbols
    /// are dirty; recomputing the same radius each time would run dozens of
    /// graph traversals on the watcher's single event-loop thread for a result
    /// that cannot have changed.
    seeds: Vec<Seed>,
    /// Graph path the last failed load resolved to. A session whose index is
    /// still building must keep retrying, but once the answer is "that exact
    /// path does not load", repeating the resolve + mmap + header-validate on
    /// every save buys nothing — the retry waits for the path to move.
    failed_graph: Option<PathBuf>,
}

impl SoftState {
    /// Recompute IMPACT(my dirty symbols). Every failure path leaves the cache
    /// empty, which degrades to the HARD-only behaviour this watcher had before
    /// the graph was wired in.
    fn rebuild(&mut self, my_session_dir: &Path) {
        let seeds = seed_symbols(my_session_dir);
        // The `source.is_some()` half matters: an unchanged seed list must not
        // short-circuit the load retry below, or a session whose first rebuild
        // ran before its index finished would stay SOFT-less for as long as it
        // keeps editing the same symbols.
        if self.source.is_some() && seeds == self.seeds {
            return;
        }
        let seeds_changed = seeds != self.seeds;
        self.seeds = seeds;
        self.cache.invalidate();
        if self.seeds.is_empty() {
            return;
        }
        if seeds_changed {
            warn_on_truncated_seeds(my_session_dir, self.seeds.len());
        }
        if self.source.is_none() {
            match load_impact_source(my_session_dir, self.failed_graph.as_deref()) {
                LoadOutcome::Loaded(source) => {
                    self.failed_graph = None;
                    self.source = Some(*source);
                }
                LoadOutcome::Failed(path) => self.failed_graph = path,
                LoadOutcome::SameFailurePath => {}
            }
        }
        let Some(source) = self.source.as_ref() else {
            return;
        };
        self.cache.refresh(impacted_names(source, &self.seeds));
    }
}

enum LoadOutcome {
    // Boxed: the engine holds an mmap handle and dwarfs the other variants,
    // which clippy's `large_enum_variant` flags — the value is moved into
    // `SoftState` once and never matched in a hot loop.
    Loaded(Box<ImpactSource>),
    /// Resolution or open failed; carries the path so the next attempt can tell
    /// whether anything moved. `None` when we never got as far as a path.
    Failed(Option<PathBuf>),
    /// The same path that already failed — skipped without touching the disk.
    SameFailurePath,
}

/// Resolve the session's own worktree, then open the graph published for it.
///
/// Deliberately `Engine::load` rather than `load_ensured`: the watcher is a
/// passive observer of other sessions and must never kick off an index build
/// behind the agent's back — a background rebuild fired from here would land
/// on the same disk as the edit that triggered it.
fn load_impact_source(my_session_dir: &Path, failed_graph: Option<&Path>) -> LoadOutcome {
    let Ok(meta) = SessionMeta::read(&my_session_dir.join("session_meta.json")) else {
        return LoadOutcome::Failed(None);
    };
    if meta.source_worktree.is_empty() {
        return LoadOutcome::Failed(None);
    }
    let worktree = PathBuf::from(&meta.source_worktree);
    let Ok(member_repo) = crate::repo_identity::repo_dir_name_for_cwd(&worktree) else {
        return LoadOutcome::Failed(None);
    };
    let graph_path = crate::graph_path::resolve(Path::new(".ecp/graph.bin"), &worktree);
    if failed_graph == Some(graph_path.as_path()) {
        return LoadOutcome::SameFailurePath;
    }
    let Ok(engine) = crate::engine::Engine::load(&graph_path) else {
        return LoadOutcome::Failed(Some(graph_path));
    };
    tracing::info!(graph = %graph_path.display(), "watcher attached graph for SOFT concerns");
    LoadOutcome::Loaded(Box::new(ImpactSource {
        engine,
        member_repo,
    }))
}

/// The seed cap makes SOFT narrower than the `IMPACT(MY_DIRTY_SYMBOLS)` it
/// documents. Emitted only when the seed set actually changed, so a save loop
/// on an over-cap dirty set does not write the same line hundreds of times.
fn warn_on_truncated_seeds(my_session_dir: &Path, kept: usize) {
    if kept < SOFT_IMPACT_MAX_SEEDS {
        return;
    }
    let Ok(dirty) = DirtyEvents::read(&layout::dirty_events_path(my_session_dir)) else {
        return;
    };
    let total = dirty
        .entries
        .values()
        .flat_map(|e| &e.dirty_symbols)
        .map(|s| (s.file.as_str(), s.name.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .len();
    if total > kept {
        tracing::warn!(
            kept,
            dropped = total - kept,
            "dirty set exceeds the SOFT seed cap — declarations past the cap get no SOFT coverage"
        );
    }
}

/// Distinct dirty symbols, capped. Order follows `DirtyEvents`' BTreeMap, so
/// the surviving prefix is stable across rebuilds instead of shuffling on every
/// save — which is what lets `rebuild` compare seed lists for equality.
fn seed_symbols(my_session_dir: &Path) -> Vec<Seed> {
    let Ok(dirty) = DirtyEvents::read(&layout::dirty_events_path(my_session_dir)) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut seeds = Vec::new();
    for symbol in dirty.entries.values().flat_map(|e| &e.dirty_symbols) {
        if seeds.len() >= SOFT_IMPACT_MAX_SEEDS {
            break;
        }
        if seen.insert((symbol.name.as_str(), symbol.file.as_str())) {
            seeds.push(Seed {
                name: symbol.name.clone(),
                file: symbol.file.clone(),
            });
        }
    }
    seeds
}

/// Union of every seed's blast radius. A seed the graph can't resolve (still
/// ambiguous within its file, or newer than the graph) is skipped: one
/// unresolvable name must not cost the others their SOFT coverage.
fn impacted_names(source: &ImpactSource, seeds: &[Seed]) -> FxHashSet<SymbolKey> {
    let mut keys = FxHashSet::default();
    for seed in seeds {
        let Ok(local) = run_for_symbol(
            &source.engine,
            &source.member_repo,
            &seed.name,
            Some(seed.file.as_str()),
            "both",
            Some(SOFT_IMPACT_DEPTH),
            None,
            false,
            Some(SOFT_IMPACT_MAX_NAMES),
        ) else {
            continue;
        };
        if collect_impact_keys(local.as_json(), &seed.file, &mut keys) {
            break;
        }
    }
    keys
}

/// `(file, name)` pairs reached by one impact traversal, deduped into `out`.
/// Returns true once `out` is full, so a single hub seed reaching six figures
/// of nodes stops the walk instead of being counted after the fact.
///
/// The depth-0 entries are the definitions `--file` resolved to, and `--file`
/// matches by substring: `src/lib.rs` also selects `vendor/x/src/lib.rs`. A
/// payload that resolved to any other file unioned a foreign definition's
/// radius into ours, so it is dropped whole rather than pruned — the seed loses
/// its SOFT coverage, which beats attributing a stranger's callers to it.
///
/// The seed itself sits at depth 0 and is kept: a peer touching it is already
/// HARD, and `classify` checks HARD first.
fn collect_impact_keys(
    payload: &serde_json::Value,
    seed_file: &str,
    out: &mut FxHashSet<SymbolKey>,
) -> bool {
    let Some(items) = payload["impact"].as_array() else {
        return out.len() >= SOFT_IMPACT_MAX_NAMES;
    };
    let resolved_elsewhere = items.iter().any(|item| {
        item["depth"].as_u64() == Some(0) && item["filePath"].as_str() != Some(seed_file)
    });
    if resolved_elsewhere {
        return out.len() >= SOFT_IMPACT_MAX_NAMES;
    }
    for item in items {
        if out.len() >= SOFT_IMPACT_MAX_NAMES {
            return true;
        }
        if let (Some(file), Some(name)) = (item["filePath"].as_str(), item["name"].as_str()) {
            out.insert((file.to_string(), name.to_string()));
        }
    }
    out.len() >= SOFT_IMPACT_MAX_NAMES
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
    use ecp_core::contracts::dirty_event::{PathChange, PathClass};
    use ecp_core::contracts::routing::SizeCap;
    use ecp_core::session::overlay::{SymbolKind, SymbolRef};
    use ecp_core::session::DirtyEventEntry;

    fn write_dirty_in(dir: &Path, file: &str, symbols: &[&str]) {
        let mut manifest = DirtyEvents::empty(SizeCap::BUILT_IN);
        manifest.entries.insert(
            file.to_string(),
            DirtyEventEntry {
                change: PathChange::Modified,
                class: PathClass::SOURCE,
                mtime_ns: 1,
                fragment_id: Some("f".into()),
                content_hash: Some("f".into()),
                parse_failed: false,
                format: 1,
                dirty_symbols: symbols
                    .iter()
                    .map(|n| SymbolRef {
                        name: (*n).into(),
                        kind: SymbolKind::Function,
                        file: file.into(),
                        line_start: 1,
                        line_end: 2,
                    })
                    .collect(),
            },
        );
        DirtyEvents::write_atomic(&layout::dirty_events_path(dir), &manifest)
            .expect("write l1/dirty_events.json");
    }

    fn write_dirty(dir: &Path, symbols: &[&str]) {
        write_dirty_in(dir, "src/a.rs", symbols);
    }

    /// Only today's manifest path names a session: an older binary's
    /// `dirty_files.json` and the bins beside the manifest do not.
    #[test]
    fn test_manifest_session_dir_new_manifest_path_returns_its_session() {
        let session = Path::new("/home/.ecp/repo/sessions/sid");
        assert_eq!(
            manifest_session_dir(&layout::dirty_events_path(session)),
            Some(session)
        );
        for other in [
            layout::legacy_dirty_files_path(session),
            layout::fragment_path(session, "f"),
            session.join("session_meta.json"),
        ] {
            assert_eq!(manifest_session_dir(&other), None, "{}", other.display());
        }
    }

    /// Stands for: a manifest written right after its `l1/` (or its session
    /// dir) is created still reaches peers. inotify adds the watch on a new
    /// directory only after its create event, so the manifest's own event is
    /// lost; the directory's create event must name the session instead.
    /// Failed before: only the manifest path itself named a session, and the
    /// 8-session fan-out test lost peers on the CI runner.
    #[test]
    fn test_event_session_dir_created_l1_with_manifest_returns_session() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("sessions").join("sid");
        write_dirty(&session, &["verify_token"]);
        let l1 = layout::dirty_events_path(&session)
            .parent()
            .unwrap()
            .to_path_buf();

        assert_eq!(event_session_dir(&l1, true), Some(session.as_path()));
        assert_eq!(event_session_dir(&session, true), Some(session.as_path()));
        assert_eq!(
            event_session_dir(&layout::dirty_events_path(&session), false),
            Some(session.as_path())
        );
        // A modify of another file in the session, or a non-create event on
        // the directory, names nothing: it would re-dispatch on every inbox
        // write.
        assert_eq!(event_session_dir(&l1, false), None);
        assert_eq!(event_session_dir(&session.join("inbox.jsonl"), true), None);
        let fragments = layout::fragments_dir(&session);
        std::fs::create_dir_all(&fragments).unwrap();
        assert_eq!(event_session_dir(&fragments, true), None);
    }

    /// A created `l1/` without a manifest yet names nothing; the manifest's
    /// own event follows once the watch exists.
    #[test]
    fn test_event_session_dir_created_l1_without_manifest_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("sessions").join("sid");
        let fragments = layout::fragments_dir(&session);
        std::fs::create_dir_all(&fragments).unwrap();
        let l1 = fragments.parent().unwrap();
        assert_eq!(event_session_dir(l1, true), None);
        assert_eq!(event_session_dir(&session, true), None);
    }

    /// The watcher reads our dirty set from `l1/dirty_events.json`: a file
    /// written there counts as newly dirty, and a session holding only the
    /// legacy manifest has nothing dirty.
    #[test]
    fn test_my_dirty_gained_symbols_new_namespace_file_reports_dirty() {
        let legacy_only = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            layout::legacy_dirty_files_path(legacy_only.path()),
            br#"{"version":1,"entries":{"src/old.rs":{"mtime_ns":1,"content_hash":"h","fragment_id":"h","tantivy_delta_segment":null}}}"#,
        )
        .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        write_dirty_in(dir.path(), "src/b.rs", &[]);
        let empty = MyDirtySurface::default();

        assert!(my_dirty_gained_symbols(dir.path(), Some(&empty)));
        assert!(!my_dirty_gained_symbols(legacy_only.path(), Some(&empty)));
        assert!(seed_symbols(legacy_only.path()).is_empty());
    }

    fn seed(name: &str, file: &str) -> Seed {
        Seed {
            name: name.into(),
            file: file.into(),
        }
    }

    #[test]
    fn seed_symbols_dedupes_repeated_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_dirty(dir.path(), &["foo", "bar", "foo"]);
        assert_eq!(
            seed_symbols(dir.path()),
            vec![seed("foo", "src/a.rs"), seed("bar", "src/a.rs")]
        );
    }

    /// The file travels with the name: `run_for_symbol` rejects a bare name
    /// with several definitions, so a seed that lost its file would contribute
    /// nothing for exactly the common names agents edit most.
    #[test]
    fn seed_symbols_keeps_the_file_that_disambiguates_the_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_dirty_in(dir.path(), "src/b.rs", &["run"]);
        assert_eq!(seed_symbols(dir.path()), vec![seed("run", "src/b.rs")]);
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

    fn key(file: &str, name: &str) -> SymbolKey {
        (file.to_string(), name.to_string())
    }

    #[test]
    fn collect_impact_keys_takes_every_reached_node_with_its_file() {
        let payload = serde_json::json!({
            "impact": [
                {"name": "seed", "filePath": "src/a.rs", "depth": 0},
                {"name": "caller", "filePath": "src/b.rs", "depth": 1},
                {"name": "callee", "filePath": "src/c.rs", "depth": 2},
            ]
        });
        let mut out = FxHashSet::default();
        assert!(!collect_impact_keys(&payload, "src/a.rs", &mut out));
        let mut got: Vec<&SymbolKey> = out.iter().collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                &key("src/a.rs", "seed"),
                &key("src/b.rs", "caller"),
                &key("src/c.rs", "callee"),
            ]
        );
    }

    /// `--file` matches by substring, so `src/lib.rs` also selects
    /// `vendor/x/src/lib.rs` and the payload unions both definitions' radii.
    /// A payload that resolved anywhere but the seed's own file is dropped
    /// whole rather than attributing a stranger's callers to our seed.
    #[test]
    fn collect_impact_keys_drops_a_payload_that_resolved_to_another_file() {
        let payload = serde_json::json!({
            "impact": [
                {"name": "run", "filePath": "src/lib.rs", "depth": 0},
                {"name": "run", "filePath": "vendor/x/src/lib.rs", "depth": 0},
                {"name": "foreign_caller", "filePath": "vendor/x/src/main.rs", "depth": 1},
            ]
        });
        let mut out = FxHashSet::default();
        assert!(!collect_impact_keys(&payload, "src/lib.rs", &mut out));
        assert!(out.is_empty(), "foreign radius leaked in: {out:?}");
    }

    /// The cap has to stop the walk mid-payload. Counting after a whole seed's
    /// traversal would let one hub symbol seat six figures of names in a daemon
    /// documented to hold 20k.
    #[test]
    fn collect_impact_keys_stops_inside_an_oversized_payload() {
        let items: Vec<serde_json::Value> = (0..SOFT_IMPACT_MAX_NAMES + 500)
            .map(|i| serde_json::json!({"name": format!("n{i}"), "filePath": "src/a.rs"}))
            .collect();
        let payload = serde_json::json!({ "impact": items });
        let mut out = FxHashSet::default();
        assert!(collect_impact_keys(&payload, "src/a.rs", &mut out));
        assert_eq!(out.len(), SOFT_IMPACT_MAX_NAMES);
    }

    /// A payload with no `impact` array (the shape an unresolvable target
    /// produces) must yield nothing — a bogus entry here would classify an
    /// unrelated peer edit as SOFT.
    #[test]
    fn collect_impact_keys_on_an_error_payload_adds_nothing() {
        let payload = serde_json::json!({"error": "No symbol named `nope`"});
        let mut out = FxHashSet::default();
        assert!(!collect_impact_keys(&payload, "src/a.rs", &mut out));
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
        assert!(!state.cache.contains("src/a.rs", "foo"));
    }

    /// The cache is derived from the seeds, so a rebuild that sees the same
    /// seeds must be a no-op. An agent iterating inside one function rewrites
    /// the manifest on every save without changing which symbols are dirty.
    #[test]
    fn rebuild_tracks_the_seeds_its_cache_was_built_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_dirty(dir.path(), &["foo"]);
        let mut state = SoftState::default();
        state.rebuild(dir.path());
        assert_eq!(state.seeds, vec![seed("foo", "src/a.rs")]);

        write_dirty(dir.path(), &["foo", "bar"]);
        state.rebuild(dir.path());
        assert_eq!(
            state.seeds,
            vec![seed("foo", "src/a.rs"), seed("bar", "src/a.rs")]
        );
    }

    /// A dirty set emptied between saves must clear the cache, not leave the
    /// previous radius firing SOFT concerns for symbols we no longer touch.
    #[test]
    fn rebuild_clears_the_cache_when_the_dirty_set_empties() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_dirty(dir.path(), &["foo"]);
        let mut state = SoftState::default();
        state.cache.refresh([key("src/a.rs", "stale")]);
        state.seeds = vec![seed("foo", "src/a.rs")];

        write_dirty(dir.path(), &[]);
        state.rebuild(dir.path());
        assert!(state.seeds.is_empty());
        assert!(!state.cache.contains("src/a.rs", "stale"));
    }

    #[test]
    fn test_open_watcher_native_failure_polls_and_delivers_events() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = channel::<notify::Result<Event>>();
        let mut watcher = open_watcher(tx, |_| {
            Err(notify::Error::io(std::io::Error::from_raw_os_error(24)))
        })
        .expect("polling fallback");
        watcher.watch(dir.path(), RecursiveMode::Recursive).unwrap();
        let target = dir.path().join("l1").join("dirty_events.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"{}").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while !seen && std::time::Instant::now() < deadline {
            if let Ok(Ok(ev)) = rx.recv_timeout(Duration::from_millis(200)) {
                seen = ev.paths.iter().any(|p| p.ends_with("l1/dirty_events.json"));
            }
        }
        assert!(
            seen,
            "polling watcher never reported the new dirty_events.json"
        );
    }
}
