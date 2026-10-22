//! The session view cache behind `Engine::overlay_view`: a later engine on
//! unchanged inputs decodes the stored view instead of building it, and any
//! change to the graph, the fragments or the cache file itself makes it
//! build again.

use ecp_cli::engine::Engine;
use ecp_cli::session::overlay_reader::{load_view_inputs, load_view_inputs_hashed};
use ecp_cli::session::overlay_writer::write_dirty_fragments_from_graphs;
use ecp_cli::session::view_cache::{self, VIEW_CACHE_FILE};
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::graph_fixture::GraphFixture;
use ecp_core::session::{DirtyFiles, OverlayView, SessionMeta};
use std::fs;
use std::path::{Path, PathBuf};
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
        DirtyFiles::write_atomic(&session_dir.join("dirty_files.json"), &DirtyFiles::empty())
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
        Engine::load(&self.graph_path)
            .unwrap()
            .with_overlay(self.session_dir.clone(), self.worktree.clone())
    }

    /// The view `OverlayView::build` makes from the current inputs.
    fn fresh(&self, engine: &Engine) -> OverlayView {
        let inputs = load_view_inputs(&self.session_dir, &self.worktree).unwrap();
        OverlayView::build(engine.graph().unwrap(), &inputs).unwrap()
    }

    /// The key `Engine::overlay_view` stores the current inputs under.
    fn key(&self, engine: &Engine) -> u64 {
        let (_, inputs_hash) = load_view_inputs_hashed(&self.session_dir, &self.worktree).unwrap();
        let graph_path = fs::canonicalize(&self.graph_path).unwrap();
        view_cache::key(
            &graph_path,
            engine.graph().unwrap().nodes.len(),
            inputs_hash,
        )
        .unwrap()
    }

    fn cache_path(&self) -> PathBuf {
        self.session_dir.join(VIEW_CACHE_FILE)
    }

    /// A valid view that differs from the real one: it adds `planted_fn`.
    fn planted(&self, engine: &Engine) -> OverlayView {
        let mut inputs = load_view_inputs(&self.session_dir, &self.worktree).unwrap();
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

#[test]
fn test_overlay_view_first_engine_writes_cache_under_engine_key() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let engine = f.engine();

    let view = engine.overlay_view().expect("a dirty file makes a view");

    assert_eq!(view, &f.fresh(&engine));
    assert_eq!(
        view_cache::read(&f.session_dir, f.key(&engine)).as_ref(),
        Some(view)
    );
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
        view_cache::read(&f.session_dir, f.key(&second)).as_ref(),
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
    let engine = Engine::load(&f.graph_path)
        .unwrap()
        .with_overlay(missing.clone(), f.worktree.clone());

    assert!(engine.overlay_view().is_none());
    assert!(!missing.exists());
}

#[cfg(unix)]
#[test]
fn test_overlay_view_read_only_session_dir_returns_built_view() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    fs::set_permissions(&f.session_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let engine = f.engine();
    let view = engine.overlay_view();
    let fresh = f.fresh(&engine);
    let leftovers = tmp_files(&f.session_dir);
    fs::set_permissions(&f.session_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(view, Some(&fresh));
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn test_overlay_view_concurrent_engines_agree_and_leave_one_valid_cache() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    let probe = f.engine();
    let fresh = f.fresh(&probe);

    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let engine = f.engine();
                assert_eq!(engine.overlay_view(), Some(&fresh));
            });
        }
    });

    assert_eq!(
        view_cache::read(&f.session_dir, f.key(&probe)).as_ref(),
        Some(&fresh)
    );
    assert!(tmp_files(&f.session_dir).is_empty());
}

#[test]
fn test_load_view_inputs_hashed_fragment_change_changes_hash() {
    let f = Fixture::new();
    let (_, clean) = load_view_inputs_hashed(&f.session_dir, &f.worktree).unwrap();
    f.edit(&["keep_fn"], 1_000);
    let (inputs, first) = load_view_inputs_hashed(&f.session_dir, &f.worktree).unwrap();
    let (_, again) = load_view_inputs_hashed(&f.session_dir, &f.worktree).unwrap();
    f.edit(&["renamed_fn"], 2_000);
    let (_, edited) = load_view_inputs_hashed(&f.session_dir, &f.worktree).unwrap();
    f.touch(3_000);
    let (skipped, unparsed) = load_view_inputs_hashed(&f.session_dir, &f.worktree).unwrap();

    assert_eq!(inputs.len(), 1);
    assert_eq!(first, again, "same inputs, same hash");
    assert_ne!(first, clean);
    assert_ne!(first, edited);
    assert!(skipped.is_empty());
    assert_eq!(unparsed, clean, "a skipped entry adds nothing");
}

/// An unreadable manifest stays an error, and the engine still serves no
/// view rather than a cached one.
#[test]
fn test_load_view_inputs_hashed_corrupt_manifest_returns_error() {
    let f = Fixture::new();
    f.edit(&["keep_fn"], 1_000);
    f.engine().overlay_view().unwrap();
    fs::write(f.session_dir.join("dirty_files.json"), "{ not json").unwrap();

    assert!(load_view_inputs_hashed(&f.session_dir, &f.worktree).is_err());
    assert!(f.engine().overlay_view().is_none());
}

#[test]
fn test_view_cache_key_missing_graph_returns_none() {
    let f = Fixture::new();

    assert!(view_cache::key(&f.graph_path.with_file_name("gone.bin"), 3, 0).is_none());
}

#[test]
fn test_view_cache_key_each_part_changes_key() {
    let f = Fixture::new();
    let graph_path = fs::canonicalize(&f.graph_path).unwrap();
    let base = view_cache::key(&graph_path, 3, 7).unwrap();

    assert_eq!(view_cache::key(&graph_path, 3, 7), Some(base));
    assert_ne!(view_cache::key(&graph_path, 4, 7), Some(base));
    assert_ne!(view_cache::key(&graph_path, 3, 8), Some(base));
    let copy = graph_path.with_file_name("copy.bin");
    fs::copy(&graph_path, &copy).unwrap();
    set_mtime(&copy, 0);
    set_mtime(&graph_path, 0);
    let same_stamp = view_cache::key(&graph_path, 3, 7).unwrap();
    assert_ne!(view_cache::key(&copy, 3, 7), Some(same_stamp), "path");
}
