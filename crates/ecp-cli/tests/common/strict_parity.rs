//! Strict overlay parity (architecture plan, area 11): the overlay view (L1
//! fragments over the L2 index of the base commit) against a fresh full index
//! of the same edited tree, over the node multiset and every edge kind.
//!
//! Both sides run the production code. The full index and the base index
//! come from `run_analyzer_for_paths`, the analyzer step of `admin index`.
//! The overlay side re-parses the dirty set with `reanalyze_files` (shadow
//! cascade included), writes the fragments and tombstones with
//! `write_dirty_events`, reads them back through `view_inputs` and
//! `decode_view_inputs`, and builds `OverlayView`. The dirty set is the
//! edited paths, as `git status --porcelain` lists them: a deleted path and
//! both paths of a rename are dirty, `reanalyze_files` skips the ones
//! missing on disk exactly as a query does, and each of those gets a
//! tombstone, as the freshness gate writes one. Fragment mtimes are the on-disk
//! ones, so this harness tests the view; the zero-mtime shadow fragment of
//! the CLI path has its own fixture (`shadow_zero_mtime.rs`).
//!
//! Unlike `parity.rs`, nothing is deduplicated: each section is a multiset,
//! so a doubled edge on one side is a difference. Sections:
//!
//! | section | key |
//! |---|---|
//! | `identity` | node uid, kind, name, file, start line |
//! | `edge` | rel, source and target identity, exact confidence |
//! | `parent` | node identity, owner class, each incoming `Defines` / `HasMethod` / `HasProperty` source |
//! | `input` | a re-parsed file whose fragment, or a missing path whose tombstone, the reader dropped |
//!
//! An edge stores no line of its own, so the endpoints' start lines stand in.
//! The overlay's `Calls` reason is the uncommitted-edit marker, so reasons
//! stay out of the key. Index-time outputs (processes and the name-paired
//! heuristic edges, plan "Beyond calls") are left out: the plan asserts
//! their caveat, not their value.
//!
//! Needs `mod common;` at the test crate root.

#![allow(dead_code)]

use crate::common::{commit_all, run_git};
use ecp_cli::commands::admin::index::run_analyzer_for_paths;
use ecp_cli::git::DiffScope;
use ecp_cli::reanalyze::reanalyze_files;
use ecp_cli::session::overlay_reader::{decode_view_inputs, view_inputs};
use ecp_cli::session::overlay_writer::write_dirty_events;
use ecp_core::contracts::dirty_event::PathClass;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::{layout, DirtyEvents, OverlayView, SessionMeta};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One working-tree change. Paths are repo-relative with `/`.
#[derive(Debug, Clone, Copy)]
pub enum Edit<'a> {
    /// Replace a file's source, or add a file.
    Write(&'a str, &'a str),
    Delete(&'a str),
    /// Move a file, content unchanged.
    Rename(&'a str, &'a str),
}

/// A tree at the base commit plus the edits an agent made on top of it.
pub struct Scenario<'a> {
    /// Baseline file stem under `tests/baselines/strict_parity/`.
    pub name: &'a str,
    /// Owner a new baseline gets under bless mode.
    pub owner: &'a str,
    pub files: &'a [(&'a str, &'a str)],
    pub edits: &'a [Edit<'a>],
}

/// The comparable content of one graph view, one line per element.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub identity: Vec<String>,
    pub edge: Vec<String>,
    pub parent: Vec<String>,
}

fn is_index_time_kind(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Process)
}

fn is_index_time_rel(rel: RelType) -> bool {
    matches!(
        rel,
        RelType::StepInProcess
            | RelType::MirrorsField
            | RelType::EventTopicMirror
            | RelType::CompensatedBy
    )
}

fn is_containment(rel: RelType) -> bool {
    matches!(
        rel,
        RelType::Defines | RelType::HasMethod | RelType::HasProperty
    )
}

/// Every visible node and edge of `merged`, rendered.
pub fn snapshot(merged: &MergedGraph<'_>) -> Snapshot {
    let base_len = merged.base_len();
    let describe = |idx: u32| -> Option<String> {
        let node = merged.node(idx)?;
        if is_index_time_kind(node.kind()) {
            return None;
        }
        Some(format!(
            "{} {}:{} {} #{:016x}",
            node.kind().as_str(),
            node.file_path(merged).unwrap_or("-"),
            node.start_line(),
            node.name(merged),
            node.uid()
        ))
    };
    let visible: Vec<u32> = (0..merged.node_count())
        .filter(|&idx| idx >= base_len || merged.base_visible(idx))
        .collect();

    let mut snap = Snapshot::default();
    let mut parents: FxHashMap<u32, Vec<String>> = FxHashMap::default();
    for edge in merged.all_edges() {
        let rel = edge.rel_type();
        if is_index_time_rel(rel) {
            continue;
        }
        let (Some(source), Some(target)) = (describe(edge.source), describe(edge.target)) else {
            continue;
        };
        if is_containment(rel) {
            parents
                .entry(edge.target)
                .or_default()
                .push(format!("{} {source}", rel.as_str()));
        }
        snap.edge.push(format!(
            "{} {source} -> {target} conf={:?}",
            rel.as_str(),
            edge.confidence()
        ));
    }
    for idx in visible {
        let Some(node) = describe(idx) else {
            continue;
        };
        let owner = merged
            .node(idx)
            .and_then(|n| n.owner_class(merged))
            .unwrap_or("-");
        match parents.get(&idx) {
            None => snap.parent.push(format!("{node} owner={owner} <- none")),
            Some(sources) => snap.parent.extend(
                sources
                    .iter()
                    .map(|source| format!("{node} owner={owner} <- {source}")),
            ),
        }
        snap.identity.push(node);
    }
    snap
}

/// Multiset difference of two snapshots, one sorted line per surplus key:
/// `<section> <full-only|overlay-only> x<count> <key>`.
pub fn diff(full: &Snapshot, overlay: &Snapshot) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (section, full_lines, overlay_lines) in [
        ("identity", &full.identity, &overlay.identity),
        ("edge", &full.edge, &overlay.edge),
        ("parent", &full.parent, &overlay.parent),
    ] {
        let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
        for line in full_lines {
            *counts.entry(line).or_default() += 1;
        }
        for line in overlay_lines {
            *counts.entry(line).or_default() -= 1;
        }
        for (line, count) in counts {
            if count != 0 {
                let side = if count > 0 {
                    "full-only"
                } else {
                    "overlay-only"
                };
                out.insert(format!("{section} {side} x{} {line}", count.abs()));
            }
        }
    }
    out
}

/// The tree after `edits`, by path.
fn edited_tree(files: &[(&str, &str)], edits: &[Edit<'_>]) -> BTreeMap<String, String> {
    let mut tree: BTreeMap<String, String> = files
        .iter()
        .map(|(path, source)| (path.to_string(), source.to_string()))
        .collect();
    for edit in edits {
        match *edit {
            Edit::Write(path, source) => {
                tree.insert(path.to_string(), source.to_string());
            }
            Edit::Delete(path) => {
                tree.remove(path);
            }
            Edit::Rename(from, to) => {
                let source = tree.remove(from).expect("a rename moves an existing file");
                tree.insert(to.to_string(), source);
            }
        }
    }
    tree
}

/// The paths `git status --porcelain` reports for `edits`.
fn dirty_paths(edits: &[Edit<'_>]) -> Vec<String> {
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for edit in edits {
        match *edit {
            Edit::Write(path, _) | Edit::Delete(path) => {
                paths.insert(path.to_string());
            }
            Edit::Rename(from, to) => {
                paths.insert(from.to_string());
                paths.insert(to.to_string());
            }
        }
    }
    paths.into_iter().collect()
}

fn write_tree<'s>(root: &Path, tree: impl IntoIterator<Item = (&'s str, &'s str)>) {
    for (path, source) in tree {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, source).unwrap();
    }
}

/// The production full index of `tree`, archived.
fn full_index<'s>(tree: impl IntoIterator<Item = (&'s str, &'s str)>) -> rkyv::util::AlignedVec {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    write_tree(src.path(), tree);
    let (_, graph): (usize, ZeroCopyGraph) =
        run_analyzer_for_paths(src.path(), out.path(), None, None).expect("full index");
    rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap()
}

fn mtime_ns(path: &Path) -> u64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

fn new_session_dir(root: &Path, worktree: &Path) -> PathBuf {
    let sid = "strict-parity";
    let dir = root.join("sessions").join(sid);
    fs::create_dir_all(&dir).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-10-25T00:00:00Z".into(),
        last_touched: "2026-10-25T00:00:00Z".into(),
        base_sha: "0000000000000000000000000000000000000000".into(),
        source_worktree: worktree.display().to_string(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&dir.join("session_meta.json"), &meta).unwrap();
    DirtyEvents::write_atomic(
        &layout::dirty_events_path(&dir),
        &DirtyEvents::empty(SizeCap::BUILT_IN),
    )
    .unwrap();
    dir
}

/// The difference lines between the overlay view of `scenario` and a fresh
/// full index of its edited tree.
pub fn observe(scenario: &Scenario<'_>) -> BTreeSet<String> {
    let edited = edited_tree(scenario.files, scenario.edits);
    let full_bytes = full_index(edited.iter().map(|(p, s)| (p.as_str(), s.as_str())));
    let base_bytes = full_index(scenario.files.iter().copied());

    // The worktree: base commit, edits on disk and unstaged.
    let repo = tempfile::tempdir().unwrap();
    write_tree(repo.path(), scenario.files.iter().copied());
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    commit_all(repo.path(), "base");
    for edit in scenario.edits {
        match *edit {
            Edit::Write(path, source) => write_tree(repo.path(), [(path, source)]),
            Edit::Delete(path) => fs::remove_file(repo.path().join(path)).unwrap(),
            Edit::Rename(from, to) => {
                let target = repo.path().join(to);
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::rename(repo.path().join(from), target).unwrap();
            }
        }
    }

    let dirty = dirty_paths(scenario.edits);
    let graphs = reanalyze_files(repo.path(), &DiffScope::Unstaged, &dirty);
    let mtimes: Vec<u64> = graphs
        .iter()
        .map(|g| mtime_ns(&repo.path().join(&g.file_path)))
        .collect();
    // The freshness gate's rule: a dirty path absent at the stat gets a
    // tombstone, not a fragment.
    let tombstones: Vec<(String, PathClass)> = dirty
        .into_iter()
        .filter(|path| !repo.path().join(path).exists())
        .map(|path| (path, PathClass::SOURCE))
        .collect();
    let session_root = tempfile::tempdir().unwrap();
    let session = new_session_dir(session_root.path(), repo.path());
    write_dirty_events(
        &session,
        ecp_core::config::resolve_size_cap(repo.path()),
        &graphs,
        &mtimes,
        &HashMap::new(),
        &tombstones,
    )
    .expect("fragment write");
    let read = view_inputs(&session, repo.path()).expect("fragment read");
    let inputs = decode_view_inputs(&read.fragments);

    let mut lines = BTreeSet::new();
    let read_back: BTreeSet<&str> = inputs
        .iter()
        .map(|i| i.rel_path.as_str())
        .chain(read.tombstones.iter().map(String::as_str))
        .collect();
    let written = graphs
        .iter()
        .map(|graph| graph.file_path.to_string_lossy().replace('\\', "/"))
        .chain(tombstones.iter().map(|(path, _)| path.clone()));
    for path in written {
        if !read_back.contains(path.as_str()) {
            lines.insert(format!("input dropped {path}"));
        }
    }

    let full = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&full_bytes).unwrap();
    let base = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&base_bytes).unwrap();
    let view = OverlayView::build_with_removed(base, &inputs, &read.tombstones);
    if let Some(view) = &view {
        assert_eq!(
            OverlayView::from_cache_bytes(&view.to_cache_bytes(), base.nodes.len()).as_ref(),
            Some(view),
            "view cache round trip"
        );
    }
    let full_snap = snapshot(&MergedGraph::new(full, None));
    let overlay_snap = snapshot(&MergedGraph::new(base, view.as_ref()));
    lines.extend(diff(&full_snap, &overlay_snap));
    lines
}

/// `tests/baselines/strict_parity/<name>.txt`.
pub fn baseline_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/baselines/strict_parity")
        .join(format!("{name}.txt"))
}

/// Observe `scenario` and hold the result to its baseline.
pub fn check(scenario: &Scenario<'_>) {
    let observed = observe(scenario);
    crate::baseline::check(&baseline_path(scenario.name), scenario.owner, &observed);
}
