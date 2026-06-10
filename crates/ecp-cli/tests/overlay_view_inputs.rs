//! Roundtrip + validation tests for the v2 fragment path feeding
//! `OverlayView` (FU-2026-06-10-398a846bca42): `write_dirty_fragments_from_graphs`
//! must persist owner_class / calls / imports, and `load_view_inputs` must
//! refuse entries that would serve stale overlay data (reverted files, v1 bins).

use ecp_cli::session::overlay_reader::load_view_inputs;
use ecp_cli::session::overlay_writer::write_dirty_fragments_from_graphs;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

fn make_session_dir(tmp: &Path, sid: &str) -> PathBuf {
    let session_dir = tmp.join("sessions").join(sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-06-10T10:00:00Z".into(),
        last_touched: "2026-06-10T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    DirtyFiles::write_atomic(&session_dir.join("dirty_files.json"), &DirtyFiles::empty()).unwrap();
    session_dir
}

fn rn(name: &str, owner: Option<&str>, calls: &[&str]) -> RawNode {
    RawNode {
        name: name.to_string(),
        kind: if owner.is_some() {
            NodeKind::Method
        } else {
            NodeKind::Function
        },
        span: (1, 0, 5, 0),
        is_exported: true,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: calls.iter().map(|c| c.to_string()).collect(),
        field_reads: Vec::new(),
        owner_class: owner.map(str::to_string),
        content_hash: 7,
    }
}

/// Write a real worktree file and return its stat mtime in nanoseconds —
/// the same value `apply_l1_overlay_updates` records into the manifest.
fn write_worktree_file(worktree: &Path, rel: &str) -> u64 {
    let p = worktree.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "pub fn x() {}\n").unwrap();
    std::fs::metadata(&p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

fn graph_for(rel: &str, nodes: Vec<RawNode>, imports: Vec<RawImport>) -> LocalGraph {
    LocalGraph {
        file_path: PathBuf::from(rel),
        content_hash: [1, 2, 3, 4, 5, 6, 7, 8],
        nodes,
        imports,
        ..Default::default()
    }
}

#[test]
fn v2_roundtrip_preserves_owner_calls_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "rt-sid");
    let mtime = write_worktree_file(&worktree, "src/dirty.rs");

    let graph = graph_for(
        "src/dirty.rs",
        vec![
            rn("free_fn", None, &["callee_a", "callee_b"]),
            rn("a_method", Some("MyType"), &[]),
        ],
        vec![RawImport {
            source: "crate::other".into(),
            imported_name: "callee_a".into(),
            alias: None,
            binding_kind: None,
        }],
    );
    write_dirty_fragments_from_graphs(&session_dir, &[graph], &[mtime]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree).unwrap();
    assert_eq!(inputs.len(), 1, "one valid v2 entry expected");
    let file = &inputs[0];
    assert_eq!(file.rel_path, "src/dirty.rs");
    assert_eq!(file.imports.len(), 1);
    assert_eq!(file.imports[0].imported_name, "callee_a");

    let free_fn = file.symbols.iter().find(|s| s.name == "free_fn").unwrap();
    assert_eq!(free_fn.calls, vec!["callee_a", "callee_b"]);
    // span row 1 (0-based) → line 2 (1-based Node convention).
    assert_eq!(free_fn.start_line, 2);

    let method = file.symbols.iter().find(|s| s.name == "a_method").unwrap();
    assert_eq!(
        method.owner_class.as_deref(),
        Some("MyType"),
        "owner_class must survive the roundtrip — method uids depend on it"
    );
}

#[test]
fn reverted_file_entry_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "rev-sid");
    let real_mtime = write_worktree_file(&worktree, "src/dirty.rs");

    let graph = graph_for("src/dirty.rs", vec![rn("free_fn", None, &[])], vec![]);
    // Manifest records a DIFFERENT mtime than the file on disk — the
    // file was reverted/touched after the fragment write.
    write_dirty_fragments_from_graphs(&session_dir, &[graph], &[real_mtime + 1]).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree).unwrap();
    assert!(
        inputs.is_empty(),
        "a stale manifest entry must never beat fresh base data: {inputs:?}"
    );
}

#[test]
fn v1_format_entry_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), "v1-sid");
    let mtime = write_worktree_file(&worktree, "src/old.rs");

    let manifest = session_dir.join("dirty_files.json");
    let mut df = DirtyFiles::read(&manifest).unwrap();
    df.entries.insert(
        "src/old.rs".into(),
        DirtyEntry {
            mtime_ns: mtime,
            content_hash: "cafe".into(),
            fragment_id: "cafe".into(),
            tantivy_delta_segment: None,
            parse_failed: false,
            dirty_symbols: vec![],
            format: 1,
        },
    );
    DirtyFiles::write_atomic(&manifest, &df).unwrap();

    let inputs = load_view_inputs(&session_dir, &worktree).unwrap();
    assert!(
        inputs.is_empty(),
        "v1 bins carry no calls/imports — the view must treat them as clean"
    );
}
