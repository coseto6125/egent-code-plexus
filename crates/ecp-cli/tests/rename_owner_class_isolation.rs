//! T1-11 regression test: rename must isolate by owner_class.
//!
//! Before the fix, `ecp rename validate new_validate` matched every graph node
//! named `validate` regardless of its owning class.  With `Foo.validate` as
//! the target, `Bar.validate` must remain untouched.
//!
//! Test strategy (same synthetic-graph injection pattern as
//! `rename_excludes_heuristic.rs`):
//!   1. Create a minimal git repo with two Python class files.
//!   2. Run `ecp admin index` to produce a valid `graph.bin` header.
//!   3. Overwrite `graph.bin` with a hand-crafted graph that has two
//!      `Method` nodes both named `validate` but with different `owner_class`
//!      values (`Foo` vs `Bar`).
//!   4. Run `ecp rename Foo.validate new_validate` and assert that only
//!      foo.py is mutated; bar.py is untouched.

mod common;

use common::run_git;
use ecp_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use ecp_core::pool::StringPool;
use rkyv::rancor::Error;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// ---------------------------------------------------------------------------
// Graph-bin injection helpers
// ---------------------------------------------------------------------------

fn find_graph_bin(repo: &Path) -> std::path::PathBuf {
    let ecp_dir = repo.join(".ecp");
    assert!(
        ecp_dir.is_dir(),
        ".ecp dir missing after index: {}",
        ecp_dir.display()
    );
    let mut queue = vec![ecp_dir];
    while let Some(dir) = queue.first().cloned() {
        queue.remove(0);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                return path;
            }
            if path.is_dir() {
                queue.push(path);
            }
        }
    }
    panic!(
        "graph.bin not found after admin index in {}",
        repo.join(".ecp").display()
    )
}

fn build_index(repo: &Path) {
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", repo.to_str().unwrap()])
        .env("HOME", repo)
        .current_dir(repo)
        .output()
        .expect("ecp admin index failed to spawn");
    assert!(
        out.status.success(),
        "ecp admin index failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
}

fn serialize_graph(graph: &ZeroCopyGraph) -> Vec<u8> {
    rkyv::to_bytes::<Error>(graph)
        .expect("serialize graph")
        .into_vec()
}

/// Build a graph with two `Method` nodes both named `validate`, one owned by
/// `Foo` (in foo.py) and one owned by `Bar` (in bar.py).  No edges needed —
/// the owner_class filter alone determines which node the rename targets.
fn two_class_validate_graph(foo_file: &str, bar_file: &str) -> Vec<u8> {
    let mut pool = StringPool::new();

    let foo_path_ref = pool.add(foo_file);
    let bar_path_ref = pool.add(bar_file);

    let name_ref = pool.add("validate");
    let owner_foo_ref = pool.add("Foo");
    let owner_bar_ref = pool.add("Bar");

    let uid_foo = ecp_core::uid::compute(
        ecp_core::graph::NodeKind::Method,
        foo_file,
        Some("Foo"),
        "validate",
    );
    let uid_bar = ecp_core::uid::compute(
        ecp_core::graph::NodeKind::Method,
        bar_file,
        Some("Bar"),
        "validate",
    );

    let files = vec![
        File {
            path: foo_path_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
        File {
            path: bar_path_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
    ];

    let nodes = vec![
        Node {
            uid: uid_foo,
            name: name_ref,
            file_idx: 0,
            kind: NodeKind::Method,
            span: (2, 4, 3, 0),
            community_id: 0,
            owner_class: owner_foo_ref,
        },
        Node {
            uid: uid_bar,
            name: name_ref,
            file_idx: 1,
            kind: NodeKind::Method,
            span: (2, 4, 3, 0),
            community_id: 0,
            owner_class: owner_bar_ref,
        },
    ];

    let n = nodes.len();
    let name_index: Vec<u32> = (0..n as u32).collect();

    serialize_graph(&ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        string_pool: pool.bytes,
        files,
        nodes,
        edges: vec![],
        out_offsets: vec![0u32, 0, 0],
        in_offsets: vec![0u32, 0, 0],
        in_edge_idx: vec![],
        name_index,
        process_start: n as u32,
        ..ZeroCopyGraph::default()
    })
}

// ---------------------------------------------------------------------------
// Repo setup helper
// ---------------------------------------------------------------------------

/// Create a repo with two Python class files, both containing a `validate`
/// method.  Injects a synthetic graph after indexing.
fn setup_two_class_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    // foo.py: class Foo with a validate method
    std::fs::write(
        root.join("foo.py"),
        "class Foo:\n    def validate(self):\n        return True\n",
    )
    .unwrap();

    // bar.py: class Bar with a validate method
    std::fs::write(
        root.join("bar.py"),
        "class Bar:\n    def validate(self):\n        return True\n",
    )
    .unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    let graph_bin = find_graph_bin(root);
    std::fs::write(&graph_bin, two_class_validate_graph("foo.py", "bar.py")).unwrap();

    repo
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Renaming `Foo.validate` must mutate `foo.py` but leave `bar.py` untouched.
///
/// Before the fix: target_symbol `Foo.validate` never matched any node (n.name
/// is always `validate`, never `Foo.validate`), so 0 files were renamed —
/// the rename silently did nothing for dotted targets.
#[test]
fn test_rename_foo_validate_leaves_bar_untouched() {
    let repo = setup_two_class_repo();
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "Foo.validate",
            "Foo.check",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // foo.py must have been rewritten (validate → check)
    let foo_content = std::fs::read_to_string(root.join("foo.py")).unwrap_or_default();
    assert!(
        foo_content.contains("check"),
        "foo.py must have `validate` renamed to `check`; foo.py=\n{foo_content}\nstdout={stdout}\nstderr={stderr}",
    );

    // bar.py must remain untouched
    let bar_content = std::fs::read_to_string(root.join("bar.py")).unwrap_or_default();
    assert!(
        !bar_content.contains("check"),
        "bar.py must NOT be mutated when renaming Foo.validate; bar.py=\n{bar_content}\nstdout={stdout}\nstderr={stderr}",
    );
    assert!(
        bar_content.contains("validate"),
        "bar.py validate method must remain intact; bar.py=\n{bar_content}",
    );
}

/// Renaming a bare name (`validate`) must return 0 hits because both nodes
/// are methods (have an owner_class) — bare-name targets resolve only
/// module-level (top-level) symbols, not class methods.
#[test]
fn test_rename_bare_name_matches_only_top_level() {
    let repo = setup_two_class_repo();
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "validate",
            "check",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Neither file should be modified — validate is a method in both, not
    // a top-level function, so a bare rename must not touch either.
    let foo_content = std::fs::read_to_string(root.join("foo.py")).unwrap_or_default();
    let bar_content = std::fs::read_to_string(root.join("bar.py")).unwrap_or_default();

    assert!(
        foo_content.contains("validate"),
        "foo.py must be untouched for bare-name rename; foo.py=\n{foo_content}\nstdout={stdout}",
    );
    assert!(
        bar_content.contains("validate"),
        "bar.py must be untouched for bare-name rename; bar.py=\n{bar_content}\nstdout={stdout}",
    );
    assert!(
        stdout.contains("No occurrences")
            || stdout.contains("0 occurrences")
            || stdout.contains("not found"),
        "bare-name rename against method-only graph must report no matches; stdout={stdout}",
    );
}

/// Positive case for tightened bare-name semantics: a module-level function
/// (no owner_class) must still be reachable via bare-name rename.  Without
/// this test, the negative test above could pass trivially even if the filter
/// incorrectly rejected ALL nodes regardless of owner_class.
#[test]
fn test_rename_bare_name_hits_top_level_function() {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    std::fs::write(root.join("util.py"), "def validate(x):\n    return True\n").unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    // Inject a synthetic graph: one Function node named `validate` with
    // owner_class = StrRef::default() (top-level / no class).
    let graph_bin = find_graph_bin(root);
    let synthetic = {
        let mut pool = StringPool::new();
        let path_ref = pool.add("util.py");
        let name_ref = pool.add("validate");
        let uid_ref = ecp_core::uid::compute(
            ecp_core::graph::NodeKind::Function,
            "util.py",
            None,
            "validate",
        );

        serialize_graph(&ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            string_pool: pool.bytes,
            files: vec![File {
                path: path_ref,
                mtime: 0,
                content_hash: [0; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![Node {
                uid: uid_ref,
                name: name_ref,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (1, 0, 2, 0),
                community_id: 0,
                owner_class: ecp_core::pool::StrRef::default(),
            }],
            edges: vec![],
            out_offsets: vec![0u32, 0],
            in_offsets: vec![0u32, 0],
            in_edge_idx: vec![],
            name_index: vec![0],
            process_start: 1,
            ..ZeroCopyGraph::default()
        })
    };
    std::fs::write(&graph_bin, synthetic).unwrap();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "validate",
            "check",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let util_content = std::fs::read_to_string(root.join("util.py")).unwrap_or_default();
    assert!(
        util_content.contains("check"),
        "top-level `validate` must be renamed by bare-name target; util.py=\n{util_content}\nstdout={stdout}\nstderr={stderr}",
    );
    assert!(
        !util_content.contains("def validate"),
        "old name must no longer appear as the function definition; util.py=\n{util_content}",
    );
}
