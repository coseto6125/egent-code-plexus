//! Every parse path reads the one effective `SizeCap` of the repo: the cold
//! index, the incremental reanalyze, the overlay writer and `impact
//! --baseline` (C5 routing contract, open point 1). The cold index guard is
//! `routing_cold_index.rs::test_cold_index_repo_config_raises_cap_file_indexed`.
//!
//! Each test plants a file between the 1 MiB built-in cap and a 2 MiB repo
//! cap, so a path that ignores `[index] max_file_bytes` turns a file the base
//! graph holds into a `file-too-large` omission.

use ecp_cli::git::DiffScope;
use ecp_cli::reanalyze::reanalyze_files;
use ecp_cli::session::overlay_writer::{write_dirty_fragment, FragmentInput, OverlayWriter};
use ecp_core::session::{layout, DirtyEvents, SessionMeta};
use std::path::{Path, PathBuf};

const ONE_AND_HALF_MIB: usize = 3 * 512 * 1024;
const REPO_CAP_2_MIB: &[u8] = b"[index]\nmax_file_bytes = 2097152\n";

fn write(root: &Path, rel: &str, content: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A Python file of `bytes` bytes that defines `name` first and pads with comments.
fn python_of_size(name: &str, bytes: usize) -> Vec<u8> {
    let mut source = format!("def {name}():\n    return 1\n").into_bytes();
    source.resize(bytes, b'#');
    if let Some(last) = source.last_mut() {
        *last = b'\n';
    }
    source
}

fn session_dir_for(worktree: &Path) -> PathBuf {
    let session_dir = worktree.join("sessions").join("s1");
    std::fs::create_dir_all(&session_dir).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: "s1".into(),
        pid: None,
        started_at: "2026-10-25T10:00:00Z".into(),
        last_touched: "2026-10-25T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: worktree.to_string_lossy().into_owned(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    DirtyEvents::write_atomic(
        &layout::dirty_events_path(&session_dir),
        &DirtyEvents::empty(ecp_core::config::resolve_size_cap(worktree)),
    )
    .unwrap();
    session_dir
}

fn dirty_symbol_names(session_dir: &Path, rel: &str) -> Vec<String> {
    let df = DirtyEvents::read(&layout::dirty_events_path(session_dir)).unwrap();
    let entry = df.entries.get(rel).expect("the file has a manifest entry");
    assert!(
        !entry.parse_failed,
        "{rel}: an omission is not a parse failure"
    );
    entry.dirty_symbols.iter().map(|s| s.name.clone()).collect()
}

/// Contract: the incremental reanalyze (the production overlay's parse)
/// parses a dirty file under the repo cap, as the cold index does.
///
/// Failed before this change: `reanalyze_files` built its pipeline with the
/// 1 MiB built-in cap, so the dirty file became a `file-too-large` omission
/// that replaced the symbols the base graph held.
#[test]
fn test_reanalyze_files_repo_config_raises_cap_symbols_returned() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), ".ecp/config.toml", REPO_CAP_2_MIB);
    write(
        repo.path(),
        "big.py",
        &python_of_size("size_cap_reanalyze_fn", ONE_AND_HALF_MIB),
    );

    let graphs = reanalyze_files(repo.path(), &DiffScope::Unstaged, &["big.py".to_owned()]);

    assert_eq!(graphs.len(), 1);
    let graph = &graphs[0];
    assert!(
        graph.blind_spots.iter().all(|b| b.kind != "file-too-large"),
        "{:?}",
        graph.blind_spots
    );
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| node.name == "size_cap_reanalyze_fn"),
        "the dirty file's symbols reach the overlay"
    );
}

/// Contract: with no repo config the built-in cap stands, and the reanalyze
/// omits the file with the reason instead of dropping it.
#[test]
fn test_reanalyze_files_no_repo_config_over_built_in_cap_omitted() {
    let repo = tempfile::tempdir().unwrap();
    write(
        repo.path(),
        "big.py",
        &python_of_size("size_cap_default_fn", ONE_AND_HALF_MIB),
    );

    let graphs = reanalyze_files(repo.path(), &DiffScope::Unstaged, &["big.py".to_owned()]);

    assert_eq!(graphs.len(), 1);
    assert!(graphs[0].nodes.is_empty());
    assert_eq!(
        graphs[0]
            .blind_spots
            .iter()
            .map(|b| b.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["file-too-large"]
    );
}

/// Contract: `OverlayWriter::append_dirty` parses under the cap of the root
/// the file path sits in.
///
/// Failed before this change: its pipeline held the 1 MiB built-in cap, so
/// the entry had no symbols.
#[test]
fn test_append_dirty_repo_config_raises_cap_symbols_recorded() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), ".ecp/config.toml", REPO_CAP_2_MIB);
    write(
        repo.path(),
        "big.py",
        &python_of_size("size_cap_append_fn", ONE_AND_HALF_MIB),
    );
    let session_dir = session_dir_for(repo.path());

    let mut writer = OverlayWriter::new(&session_dir);
    writer
        .append_dirty(&repo.path().join("big.py"), "h", "f")
        .unwrap();

    assert!(dirty_symbol_names(&session_dir, "big.py").contains(&"size_cap_append_fn".to_string()));
}

/// Contract: the re-parse route of the overlay writer reads the cap of the
/// session's worktree. Over the cap the fragment is the `file-too-large`
/// omission, so the entry holds no symbols; a repo cap that admits the file
/// gives its symbols.
///
/// Failed before this change: `parse_file_raw` applied no cap, so the
/// oversized file was parsed in the overlay and omitted from the cold index.
#[test]
fn test_write_dirty_fragment_cap_follows_session_worktree_config() {
    let source = python_of_size("size_cap_fragment_fn", ONE_AND_HALF_MIB);
    let input = FragmentInput {
        rel_path: "big.py".into(),
        content: source,
        mtime_ns: 1,
    };

    let default_repo = tempfile::tempdir().unwrap();
    let session_dir = session_dir_for(default_repo.path());
    let outcome = write_dirty_fragment(&session_dir, &input).unwrap();
    assert!(!outcome.parse_failed);
    assert!(
        dirty_symbol_names(&session_dir, "big.py").is_empty(),
        "over the built-in cap the overlay holds the omission, as the cold index does"
    );

    let raised_repo = tempfile::tempdir().unwrap();
    write(raised_repo.path(), ".ecp/config.toml", REPO_CAP_2_MIB);
    let session_dir = session_dir_for(raised_repo.path());
    write_dirty_fragment(&session_dir, &input).unwrap();
    assert!(
        dirty_symbol_names(&session_dir, "big.py").contains(&"size_cap_fragment_fn".to_string())
    );
}
