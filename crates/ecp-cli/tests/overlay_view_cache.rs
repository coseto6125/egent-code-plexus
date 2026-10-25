//! The session view cache behind `Engine::overlay_view`: a later engine on
//! unchanged inputs decodes the stored view instead of building it, and any
//! change to the graph, the fragments or the cache file itself makes it
//! build again. The `test_dirty_events_*` and tombstone tests drive the
//! freshness gate over the `l1/dirty_events.json` manifest the cache reads.

use ecp_cli::auto_ensure::{self, test_counters, IndexNeed};
use ecp_cli::engine::Engine;
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_cli::session::overlay_writer::write_dirty_fragments_from_graphs;
use ecp_cli::session::view_cache::{self, VIEW_CACHE_FILE};
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::graph_fixture::GraphFixture;
use ecp_core::session::layout;
use ecp_core::session::{
    DirtyEvents, OverlayFileInput, OverlayView, SessionMeta, DIRTY_EVENTS_FORMAT,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Barrier;
use std::time::{Duration, UNIX_EPOCH};

const DIRTY: &str = "src/dirty.rs";

struct Fixture {
    _tmp: tempfile::TempDir,
    graph_path: PathBuf,
    session_dir: PathBuf,
    worktree: PathBuf,
}

impl Fixture {
    /// Base graph: `keep_fn` and `gone_fn` in the dirty file, `callee` in a
    /// clean one. The session holds no fragment yet.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let mut fx = GraphFixture::new();
        fx.func(DIRTY, "keep_fn");
        fx.func(DIRTY, "gone_fn");
        fx.func("src/clean.rs", "callee");
        let graph_path = tmp.path().join("graph.bin");
        fs::write(&graph_path, fx.into_bytes()).unwrap();

        let sid = "cache-sid";
        let session_dir = tmp.path().join("sessions").join(sid);
        fs::create_dir_all(&session_dir).unwrap();
        let meta = SessionMeta {
            version: 1,
            session_id: sid.to_string(),
            pid: None,
            started_at: "2026-10-22T10:00:00Z".into(),
            last_touched: "2026-10-22T10:00:00Z".into(),
            base_sha: "abc123def4567890abc123def4567890abc123de".into(),
            source_worktree: "/work/x".into(),
            overlay_version: 0,
            watcher_pid: None,
            last_drained_offset: 0,
            agent_name: None,
        };
        SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &meta).unwrap();
        DirtyEvents::write_atomic(
            &layout::dirty_events_path(&session_dir),
            &DirtyEvents::empty(ecp_core::config::resolve_size_cap(Path::new(
                &meta.source_worktree,
            ))),
        )
        .unwrap();
        Self {
            worktree: tmp.path().join("wt"),
            _tmp: tmp,
            graph_path,
            session_dir,
        }
    }

    /// Edit the dirty file to declare `names` (each calling `callee`) with
    /// the on-disk mtime `secs` past the epoch, and record its fragment.
    fn edit(&self, names: &[&str], secs: u64) {
        let mtime_ns = self.touch(secs);
        let nodes = names.iter().copied().map(raw_fn).collect();
        let graph = LocalGraph {
            file_path: PathBuf::from(DIRTY),
            content_hash: [1, 2, 3, 4, 5, 6, 7, 8],
            nodes,
            ..Default::default()
        };
        write_dirty_fragments_from_graphs(&self.session_dir, &[graph], &[mtime_ns]).unwrap();
    }

    /// Set the dirty file's mtime without a new fragment, as an edit the
    /// session has not parsed yet does. Returns the mtime in nanoseconds.
    fn touch(&self, secs: u64) -> u64 {
        let path = self.worktree.join(DIRTY);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("// edit at {secs}\n")).unwrap();
        set_mtime(&path, secs);
        fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn engine(&self) -> Engine {
        Engine::load(&self.graph_path).unwrap().with_overlay(
            self.session_dir.clone(),
            self.worktree.clone(),
            None,
        )
    }

    /// The inputs `Engine::overlay_view` builds from on a cache miss.
    fn inputs(&self) -> Vec<OverlayFileInput> {
        let (accepted, _) = accepted_fragments(&self.session_dir, &self.worktree).unwrap();
        decode_view_inputs(&accepted)
    }

    /// The view `OverlayView::build` makes from the current inputs.
    fn fresh(&self, engine: &Engine) -> OverlayView {
        OverlayView::build(engine.graph().unwrap(), &self.inputs()).unwrap()
    }

    /// The key `Engine::overlay_view` stores the current inputs under.
    fn key(&self, engine: &Engine) -> u64 {
        let (_, inputs_hash) = accepted_fragments(&self.session_dir, &self.worktree).unwrap();
        engine.view_cache_key(inputs_hash).unwrap()
    }

    /// The view cached under `engine`'s key for the current inputs.
    fn cached(&self, engine: &Engine) -> Option<OverlayView> {
        let nodes = engine.graph().unwrap().nodes.len();
        view_cache::read(&self.session_dir, self.key(engine), nodes)
    }

    fn cache_path(&self) -> PathBuf {
        self.session_dir.join(VIEW_CACHE_FILE)
    }

    /// A valid view that differs from the real one: it adds `planted_fn`.
    fn planted(&self, engine: &Engine) -> OverlayView {
        let mut inputs = self.inputs();
        let mut extra = inputs[0].symbols[0].clone();
        extra.name = "planted_fn".to_string();
        inputs[0].symbols.push(extra);
        OverlayView::build(engine.graph().unwrap(), &inputs).unwrap()
    }
}

fn raw_fn(name: &str) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind: NodeKind::Function,
        span: (1, 0, 5, 0),
        is_exported: true,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec!["callee".to_string()],
        field_reads: Vec::new(),
        owner_class: None,
        content_hash: 7,
    }
}

fn set_mtime(path: &Path, secs: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn names(view: &OverlayView) -> Vec<&str> {
    view.virtual_nodes()
        .iter()
        .map(|n| n.name.as_str())
        .collect()
}

fn tmp_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect()
}

/// Stands for: the reader accepts the fragments this query's refresh just
/// stamped. When the effective cap moves between the refresh and the read,
/// the refresh's cap (`with_overlay`'s third argument) decides, not a second
/// read of the config.
#[test]
fn test_overlay_view_refresh_cap_given_accepts_fragments_stamped_under_it() {
    let f = Fixture::new();
    f.edit(&["keep_fn", "added_fn"], 1_000);
    let manifest_path = layout::dirty_events_path(&f.session_dir);
    let mut manifest = DirtyEvents::read(&manifest_path).unwrap();
    let refresh_cap = ecp_core::contracts::routing::SizeCap::resolve(Some("123457"), None);
    manifest.size_cap_bytes = refresh_cap.bytes();
    DirtyEvents::write_atomic(&manifest_path, &manifest).unwrap();

    let without = Engine::load(&f.graph_path).unwrap().with_overlay(
        f.session_dir.clone(),
        f.worktree.clone(),
        None,
    );
    let with = Engine::load(&f.graph_path).unwrap().with_overlay(
        f.session_dir.clone(),
        f.worktree.clone(),
        Some(refresh_cap),
    );

    assert!(
        without.overlay_view().is_none(),
        "a foreign cap was accepted"
    );
    let view = with.overlay_view().expect("the refresh's cap was not used");
    assert!(view.virtual_nodes().iter().any(|n| n.name == "added_fn"));
}

#[test]
fn test_overlay_view_first_engine_writes_cache_under_engine_key() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let engine = f.engine();

    let view = engine.overlay_view().expect("a dirty file makes a view");

    assert_eq!(view, &f.fresh(&engine));
    assert_eq!(f.cached(&engine).as_ref(), Some(view));
    assert!(tmp_files(&f.session_dir).is_empty());
}

#[test]
fn test_overlay_view_second_engine_unchanged_inputs_returns_equal_view() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let first = f.engine();
    let built = first.overlay_view().unwrap();
    let stored = fs::read(f.cache_path()).unwrap();

    let second = f.engine();

    assert_eq!(second.overlay_view(), Some(built));
    assert_eq!(
        fs::read(f.cache_path()).unwrap(),
        stored,
        "a hit leaves the file alone"
    );
}

/// Fails when the cache is never read: the second engine would build the
/// real view instead of returning the planted one.
#[test]
fn test_overlay_view_planted_cache_under_current_key_returns_planted_view() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let first = f.engine();
    first.overlay_view().unwrap();
    let planted = f.planted(&first);
    assert_ne!(&planted, first.overlay_view().unwrap());
    view_cache::write(&f.session_dir, f.key(&first), &planted);

    let second = f.engine();

    assert_eq!(second.overlay_view(), Some(&planted));
}

#[test]
fn test_overlay_view_edited_dirty_file_rebuilds_with_edit() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let first = f.engine();
    let before = names(first.overlay_view().unwrap()).join(",");
    f.edit(&["renamed_fn"], 2_000);

    let second = f.engine();
    let view = second.overlay_view().unwrap();

    assert_eq!(before, "keep_fn");
    assert_eq!(names(view), ["renamed_fn"]);
    assert_eq!(view, &f.fresh(&second));
    assert_eq!(
        f.cached(&second).as_ref(),
        Some(view),
        "the rebuilt view replaces the stale cache"
    );
}

/// An edit the session has not parsed yet drops the fragment from the
/// inputs; the cached view of the old fragment must not come back.
#[test]
fn test_overlay_view_unparsed_edit_ignores_cache_returns_none() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    f.engine().overlay_view().unwrap();
    f.touch(2_000);

    assert!(f.engine().overlay_view().is_none());
}

#[test]
fn test_overlay_view_damaged_cache_file_rebuilds_and_rewrites() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let first = f.engine();
    first.overlay_view().unwrap();
    let good = fs::read(f.cache_path()).unwrap();
    let flip = |at: usize| {
        let mut bytes = good.clone();
        bytes[at] ^= 0xFF;
        bytes
    };
    let damaged: [(&str, Vec<u8>); 7] = [
        ("empty", Vec::new()),
        ("shorter than the header", good[..5].to_vec()),
        ("header only", good[..16].to_vec()),
        ("truncated body", good[..good.len() - 1].to_vec()),
        ("flipped key", flip(0)),
        ("flipped body byte", flip(good.len() / 2 + 8)),
        ("flipped last byte", flip(good.len() - 1)),
    ];
    for (what, bytes) in damaged {
        fs::write(f.cache_path(), &bytes).unwrap();

        let engine = f.engine();

        assert_eq!(engine.overlay_view(), Some(&f.fresh(&engine)), "{what}");
        assert_eq!(fs::read(f.cache_path()).unwrap(), good, "{what}: rewritten");
    }
}

/// A header whose key was rewritten while body and checksum stayed, as a
/// torn overwrite could leave it: the checksum covers the key, so the body
/// is never served under a key it was not written for.
#[test]
fn test_view_cache_read_key_swapped_over_intact_body_returns_none() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let probe = f.engine();
    let nodes = probe.graph().unwrap().nodes.len();
    view_cache::write(&f.session_dir, 1, &f.fresh(&probe));
    let mut bytes = fs::read(f.cache_path()).unwrap();
    bytes[..8].copy_from_slice(&2_u64.to_le_bytes());
    fs::write(f.cache_path(), &bytes).unwrap();

    assert!(view_cache::read(&f.session_dir, 2, nodes).is_none());
}

#[test]
fn test_overlay_view_cache_under_other_key_rebuilds() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let first = f.engine();
    first.overlay_view().unwrap();
    let planted = f.planted(&first);
    view_cache::write(&f.session_dir, f.key(&first) ^ 1, &planted);

    let second = f.engine();

    assert_eq!(second.overlay_view(), Some(&f.fresh(&second)));
}

/// A reindex in place (same path, new mtime) keys a different cache.
#[test]
fn test_overlay_view_graph_mtime_changed_rebuilds_ignoring_planted_view() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let first = f.engine();
    first.overlay_view().unwrap();
    let old_key = f.key(&first);
    view_cache::write(&f.session_dir, old_key, &f.planted(&first));
    set_mtime(&f.graph_path, 3_000);

    let second = f.engine();

    assert_ne!(f.key(&second), old_key);
    assert_eq!(second.overlay_view(), Some(&f.fresh(&second)));
}

#[test]
fn test_overlay_view_clean_tree_returns_none_writes_no_cache() {
    let f = Fixture::new();

    assert!(f.engine().overlay_view().is_none());
    assert!(!f.cache_path().exists());
}

#[test]
fn test_overlay_view_missing_session_dir_returns_none_creates_nothing() {
    let f = Fixture::new();
    let missing = f.session_dir.with_file_name("no-such-sid");
    let engine = Engine::load(&f.graph_path).unwrap().with_overlay(
        missing.clone(),
        f.worktree.clone(),
        None,
    );

    assert!(engine.overlay_view().is_none());
    assert!(!missing.exists());
}

/// A session dir the process cannot write to still serves the built view,
/// and the failed write leaves neither a cache nor a temp file behind.
#[cfg(unix)]
#[test]
fn test_overlay_view_read_only_session_dir_returns_built_view() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    fs::set_permissions(&f.session_dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores the mode bits, and then nothing here rejects a write.
    let probe = f.session_dir.join("write-probe");
    let writable = fs::write(&probe, b"").is_ok();
    if writable {
        fs::remove_file(&probe).unwrap();
    }

    let engine = f.engine();
    let view = engine.overlay_view();
    let fresh = f.fresh(&engine);
    let cache_written = f.cache_path().exists();
    let leftovers = tmp_files(&f.session_dir);
    fs::set_permissions(&f.session_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(view, Some(&fresh));
    if writable {
        eprintln!(
            "skipped the write-rejection checks: a 0555 dir stays writable to this user (root?)"
        );
    } else {
        assert!(!cache_written, "a read-only session dir holds no cache");
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}

#[test]
fn test_overlay_view_concurrent_engines_agree_and_leave_one_valid_cache() {
    const ENGINES: usize = 4;
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let probe = f.engine();
    let fresh = f.fresh(&probe);
    let engines: Vec<Engine> = (0..ENGINES).map(|_| f.engine()).collect();
    let barrier = Barrier::new(ENGINES);

    std::thread::scope(|scope| {
        for engine in &engines {
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                engine.overlay_view();
            });
        }
    });

    for engine in &engines {
        assert_eq!(engine.overlay_view(), Some(&fresh));
    }
    let cache_files: Vec<String> = fs::read_dir(&f.session_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(VIEW_CACHE_FILE))
        .collect();
    assert_eq!(cache_files, [VIEW_CACHE_FILE]);
    assert_eq!(f.cached(&probe).as_ref(), Some(&fresh));
    assert!(tmp_files(&f.session_dir).is_empty());
}

/// Two writers publish different views under their own keys while a reader
/// reads both keys; every hit must be exactly the view written under that
/// key (a torn file is only a miss). The file is published before the
/// threads start, so it always holds one of the two views: a reader round
/// with no rename between its two reads hits exactly once. The writers
/// rename at most `2 * WRITES` times, so at least `READS - 2 * WRITES` rounds
/// hit, whatever the scheduling. Overlap of reads and writes is likely but
/// not forced; the hit floor is.
#[test]
fn test_view_cache_concurrent_writes_and_reads_never_return_wrong_view() {
    const WRITES: usize = 300;
    const READS: usize = 2_000;
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let probe = f.engine();
    let first = f.fresh(&probe);
    f.edit(&["keep_fn", "renamed_fn"], 2_000);
    let second = f.fresh(&probe);
    assert_ne!(first, second);
    let nodes = probe.graph().unwrap().nodes.len();
    let views = [(11_u64, &first), (22_u64, &second)];
    view_cache::write(&f.session_dir, views[0].0, views[0].1);
    let barrier = Barrier::new(views.len() + 1);

    let hits = std::thread::scope(|scope| {
        for (key, view) in views {
            let (barrier, dir) = (&barrier, &f.session_dir);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..WRITES {
                    view_cache::write(dir, key, view);
                }
            });
        }
        let reader = scope.spawn(|| {
            barrier.wait();
            let mut hits = 0;
            for _ in 0..READS {
                for (key, view) in views {
                    if let Some(read) = view_cache::read(&f.session_dir, key, nodes) {
                        assert_eq!(&read, view, "key {key} read another view");
                        hits += 1;
                    }
                }
            }
            hits
        });
        reader.join().unwrap()
    });

    assert!(hits >= READS - 2 * WRITES, "{hits} hits");
    let on_disk: Vec<u64> = views
        .iter()
        .filter(|(key, view)| view_cache::read(&f.session_dir, *key, nodes).as_ref() == Some(*view))
        .map(|(key, _)| *key)
        .collect();
    assert_eq!(on_disk.len(), 1, "{on_disk:?}");
    assert!(tmp_files(&f.session_dir).is_empty());
}

#[test]
fn test_accepted_fragments_fragment_change_changes_hash() {
    let f = Fixture::new();
    let (_, clean) = accepted_fragments(&f.session_dir, &f.worktree).unwrap();
    f.edit(&["keep_fn"], 1_000);
    let (accepted, first) = accepted_fragments(&f.session_dir, &f.worktree).unwrap();
    let (_, again) = accepted_fragments(&f.session_dir, &f.worktree).unwrap();
    f.edit(&["renamed_fn"], 2_000);
    let (_, edited) = accepted_fragments(&f.session_dir, &f.worktree).unwrap();
    f.touch(3_000);
    let (skipped, unparsed) = accepted_fragments(&f.session_dir, &f.worktree).unwrap();

    assert_eq!(accepted.len(), 1);
    assert_eq!(decode_view_inputs(&accepted).len(), 1);
    assert_eq!(first, again, "same inputs, same hash");
    assert_ne!(first, clean);
    assert_ne!(first, edited);
    assert!(skipped.is_empty());
    assert_eq!(unparsed, clean, "a skipped entry adds nothing");
}

/// An unreadable manifest stays an error, and the engine still serves no
/// view rather than a cached one.
#[test]
fn test_accepted_fragments_corrupt_manifest_returns_error() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    f.engine().overlay_view().unwrap();
    fs::write(layout::dirty_events_path(&f.session_dir), "{ not json").unwrap();

    assert!(accepted_fragments(&f.session_dir, &f.worktree).is_err());
    assert!(f.engine().overlay_view().is_none());
}

#[test]
fn test_view_cache_key_pre_epoch_graph_mtime_returns_none() {
    let before_epoch = UNIX_EPOCH - Duration::from_secs(1);

    assert!(view_cache::key((Path::new("/g/graph.bin"), 10, before_epoch), 3, 0).is_none());
}

#[test]
fn test_view_cache_key_each_part_changes_key() {
    let path = Path::new("/g/graph.bin");
    let mtime = UNIX_EPOCH + Duration::from_secs(1_000);
    let base = view_cache::key((path, 10, mtime), 3, 7).unwrap();
    // Windows `SystemTime` counts 100 ns ticks, so 1 ns would round away.
    let later = mtime + Duration::from_micros(1);

    assert_eq!(view_cache::key((path, 10, mtime), 3, 7), Some(base));
    for (what, other) in [
        (
            "path",
            view_cache::key((Path::new("/g/copy.bin"), 10, mtime), 3, 7),
        ),
        ("length", view_cache::key((path, 11, mtime), 3, 7)),
        ("mtime", view_cache::key((path, 10, later), 3, 7)),
        ("node count", view_cache::key((path, 10, mtime), 4, 7)),
        ("inputs", view_cache::key((path, 10, mtime), 3, 8)),
    ] {
        assert_ne!(other, Some(base), "{what}");
    }
}

/// The key follows the file the engine mapped: a reindex in place after the
/// load leaves this engine's key alone and gives the next engine another.
#[test]
fn test_view_cache_key_graph_touched_after_load_keeps_loaded_stamp() {
    let f = Fixture::new();
    let engine = f.engine();
    let loaded = engine.view_cache_key(7).unwrap();

    set_mtime(&f.graph_path, 3_000);

    assert_eq!(engine.view_cache_key(7), Some(loaded));
    assert_ne!(f.engine().view_cache_key(7).unwrap(), loaded);
}

/// A publisher that renames a new graph over the path after the load: this
/// engine still reads the old mapping, so its key must not follow the path.
/// Unix only: Windows refuses to replace a file another handle has mapped.
#[cfg(unix)]
#[test]
fn test_view_cache_key_graph_renamed_over_after_load_keeps_loaded_stamp() {
    let f = Fixture::new();
    let engine = f.engine();
    let loaded = engine.view_cache_key(7).unwrap();
    let replacement = f.graph_path.with_extension("bin.new");
    fs::copy(&f.graph_path, &replacement).unwrap();
    set_mtime(&replacement, 3_000);

    fs::rename(&replacement, &f.graph_path).unwrap();

    assert_eq!(engine.view_cache_key(7), Some(loaded));
    assert_ne!(f.engine().view_cache_key(7).unwrap(), loaded);
}

// ------------------------------------------- freshness gate over the manifest

const CHILD_ENV: &str = "ECP_OVERLAY_VIEW_CACHE_CHILD";

/// Re-runs `test_name` alone in a child process with `env` set and returns
/// true in the parent; returns false inside the child. The child sets
/// `ECP_HOME`, `ECP_SESSION_ID` and the cap env var, and reads the
/// process-wide counters, which no sibling test may share. A drifted name
/// makes libtest run nothing and exit 0, so the child must report one pass.
fn rerun_in_child(test_name: &str, env: &[(&str, &str)]) -> bool {
    if std::env::var_os(CHILD_ENV).is_some() {
        return false;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD_ENV, "1")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "isolated test process {test_name}: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    true
}

/// A git repo with `files` committed, a graph whose sidecar names HEAD (so
/// the porcelain shortcut hands the gate its dirty set), and this process's
/// `ECP_HOME` / `ECP_SESSION_ID` pointing at a session of its own.
struct GitSession {
    _tmp: tempfile::TempDir,
    worktree: PathBuf,
    graph_path: PathBuf,
}

impl GitSession {
    fn new(session_id: &str, files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let worktree = tmp.path().join("repo");
        fs::create_dir_all(&worktree).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&worktree)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "test"]);
        for (rel, text) in files {
            fs::write(worktree.join(rel), text).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let graph_path = tmp.path().join("cache").join("graph.bin");
        fs::create_dir_all(graph_path.parent().unwrap()).unwrap();
        fs::write(&graph_path, GraphFixture::new().into_bytes()).unwrap();
        auto_ensure::write_head_sha_sidecar_with_sha(&graph_path, &git(&["rev-parse", "HEAD"]));
        std::env::set_var("ECP_HOME", tmp.path().join("ecp-home"));
        std::env::set_var("ECP_SESSION_ID", session_id);
        Self {
            _tmp: tmp,
            worktree,
            graph_path,
        }
    }

    fn session_dir(&self) -> PathBuf {
        auto_ensure::resolve_session_overlay_dir(&self.worktree).unwrap()
    }

    fn manifest(&self) -> DirtyEvents {
        DirtyEvents::read(&layout::dirty_events_path(&self.session_dir())).unwrap()
    }

    /// One query's refresh. Returns the paths handed to `reanalyze_files`.
    fn refresh(&self) -> usize {
        test_counters::reset();
        auto_ensure::ensure_fresh(IndexNeed::NearCurrent, &self.graph_path, &self.worktree)
            .unwrap();
        test_counters::reanalyze_paths()
    }

    fn accepted(&self) -> std::io::Result<usize> {
        accepted_fragments(&self.session_dir(), &self.worktree).map(|(a, _)| a.len())
    }
}

const TWO_FILES: [(&str, &str); 2] = [("a.py", "def a(): pass\n"), ("b.py", "def b(): pass\n")];

fn edit_both(s: &GitSession) {
    fs::write(s.worktree.join("a.py"), "def a2(): pass\n").unwrap();
    fs::write(s.worktree.join("b.py"), "def b2(): pass\n").unwrap();
}

/// A manifest without `format` is not this namespace's: the reader serves
/// none of its entries and the gate re-parses every dirty file, although
/// every entry's mtime and bin still match.
#[test]
fn test_dirty_events_missing_format_reparses_every_dirty_file() {
    if rerun_in_child(
        "test_dirty_events_missing_format_reparses_every_dirty_file",
        &[],
    ) {
        return;
    }
    let s = GitSession::new("missing-format-test", &TWO_FILES);
    edit_both(&s);
    assert_eq!(s.refresh(), 2);
    assert_eq!(
        s.refresh(),
        0,
        "control: a current manifest skips the re-parse"
    );
    assert_eq!(s.accepted().unwrap(), 2);
    let path = layout::dirty_events_path(&s.session_dir());
    let mut doc: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    doc.as_object_mut().unwrap().remove("format").unwrap();
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();

    let served = s.accepted();
    let reparsed = s.refresh();

    assert!(
        served.is_err(),
        "a manifest without format was served: {served:?}"
    );
    assert_eq!(reparsed, 2, "every dirty file re-parses");
    let after = s.manifest();
    assert_eq!(after.format, DIRTY_EVENTS_FORMAT);
    assert_eq!(after.entries.keys().collect::<Vec<_>>(), ["a.py", "b.py"]);
    assert_eq!(s.accepted().unwrap(), 2);
}

/// Fragments parsed under one cap are not current under another: a file
/// over the new cap would parse to an omission.
#[test]
fn test_dirty_events_size_cap_change_marks_manifest_stale() {
    if rerun_in_child(
        "test_dirty_events_size_cap_change_marks_manifest_stale",
        &[("ECP_MAX_FILE_BYTES", "1000000")],
    ) {
        return;
    }
    let s = GitSession::new("size-cap-test", &TWO_FILES);
    edit_both(&s);
    assert_eq!(s.refresh(), 2);
    assert_eq!(s.refresh(), 0, "control: the same cap skips the re-parse");
    assert_eq!(s.manifest().size_cap_bytes, 1_000_000);
    assert_eq!(s.accepted().unwrap(), 2);

    std::env::set_var("ECP_MAX_FILE_BYTES", "2000000");
    let served = s.accepted().unwrap();
    let reparsed = s.refresh();

    assert_eq!(served, 0, "fragments of another cap were served");
    assert_eq!(reparsed, 2, "every dirty file re-parses");
    assert_eq!(s.manifest().size_cap_bytes, 2_000_000);
    assert_eq!(s.accepted().unwrap(), 2);
}

/// `rm` of a tracked source file writes a tombstone: `change: deleted`, a
/// class, no fragment id and no bin. It stays current while the file is
/// absent and re-parses once the file is back.
#[test]
fn test_overlay_writer_deleted_file_writes_tombstone_without_bin() {
    if rerun_in_child(
        "test_overlay_writer_deleted_file_writes_tombstone_without_bin",
        &[],
    ) {
        return;
    }
    use ecp_core::contracts::dirty_event::PathChange;
    let s = GitSession::new("tombstone-test", &TWO_FILES);
    fs::write(s.worktree.join("a.py"), "def a2(): pass\n").unwrap();
    fs::remove_file(s.worktree.join("b.py")).unwrap();

    assert_eq!(s.refresh(), 2);

    let manifest = s.manifest();
    let (edited, gone) = (&manifest.entries["a.py"], &manifest.entries["b.py"]);
    assert_eq!(edited.change, PathChange::Modified);
    assert_eq!(gone.change, PathChange::Deleted);
    assert!(gone.class.is_source());
    assert_eq!(gone.fragment_id, None);
    assert!(gone.is_tombstone());
    let mut bins: Vec<String> = fs::read_dir(layout::fragments_dir(&s.session_dir()))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    bins.sort();
    assert_eq!(
        bins,
        [format!("{}.bin", edited.fragment_id.as_deref().unwrap())]
    );
    assert_eq!(
        s.accepted().unwrap(),
        1,
        "a tombstone is no overlay input yet"
    );

    assert_eq!(s.refresh(), 0, "a tombstone of an absent file is current");
    fs::write(s.worktree.join("b.py"), "def back(): pass\n").unwrap();
    assert_eq!(s.refresh(), 1, "the file came back: only it re-parses");
    let back = s.manifest();
    assert_eq!(back.entries["b.py"].change, PathChange::Modified);
    assert!(back.entries["b.py"].fragment_id.is_some());
}
