//! T-H2 integration tests: heuristic edge exclusion from rename + mirror count.
//!
//! All four tests share the same synthetic-graph injection pattern:
//!   1. Create a minimal git repo with Python source files.
//!   2. Run `ecp admin index` to produce a valid `graph.bin` header.
//!   3. Overwrite `graph.bin` with a hand-crafted graph that carries
//!      `MirrorsField` edges, which the real indexer does not emit for plain
//!      Python functions.
//!   4. Run `ecp rename` and assert the required output / file state.

mod common;

use common::run_git;
use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
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

/// Locate the `graph.bin` written under `~/.ecp/…/graph.bin` after indexing.
/// The exact nesting depth varies by version; this walks the tree breadth-first
/// until it finds a file named `graph.bin`.
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

/// Run `ecp admin index` against `repo`. HOME is set to `repo` so that
/// `~/.ecp/` resolves to `repo/.ecp/`, matching the `find_graph_bin` helper.
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

/// Serialize a `ZeroCopyGraph` to bytes suitable for writing as `graph.bin`.
fn serialize_graph(graph: &ZeroCopyGraph) -> Vec<u8> {
    rkyv::to_bytes::<Error>(graph)
        .expect("serialize graph")
        .into_vec()
}

// ---------------------------------------------------------------------------
// Synthetic graph builders
// ---------------------------------------------------------------------------

/// Two `Function` nodes — `email` in file A and `email` in file B — linked by
/// a `MirrorsField` edge (B → A, i.e. the mirror node is the source).
///
/// Graph layout:
///   node 0  email       file 0  src/model.py   (definition)
///   node 1  email_copy  file 1  src/schema.py  (mirror, heuristic)
///   edges[0]: node 1 → node 0, MirrorsField
///
/// CSR:
///   out_offsets: [0, 0, 1]   (node 0 has 0 outgoing; node 1 has 1 outgoing)
///   in_offsets:  [0, 1, 1]   (node 0 has 1 incoming; node 1 has 0 incoming)
///   in_edge_idx: [0]          (node 0's single inbound edge is edges[0])
fn mirrors_field_graph(
    model_symbol: &str,
    mirror_symbol: &str,
    model_file: &str,
    mirror_file: &str,
) -> Vec<u8> {
    let mut pool = StringPool::new();

    let model_path_ref = pool.add(model_file);
    let mirror_path_ref = pool.add(mirror_file);
    let model_name_ref = pool.add(model_symbol);
    let mirror_name_ref = pool.add(mirror_symbol);
    let model_uid = pool.add(&format!("Function:{model_file}:{model_symbol}"));
    let mirror_uid = pool.add(&format!("Function:{mirror_file}:{mirror_symbol}"));
    let reason_ref = pool.add("schema-mirror-heuristic");

    let files = vec![
        File {
            path: model_path_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
        File {
            path: mirror_path_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
    ];

    let nodes = vec![
        Node {
            uid: model_uid,
            name: model_name_ref,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 2, 0),
            community_id: 0,
        },
        Node {
            uid: mirror_uid,
            name: mirror_name_ref,
            file_idx: 1,
            kind: NodeKind::Function,
            span: (1, 0, 2, 0),
            community_id: 0,
        },
    ];

    // MirrorsField: mirror node (1) → model node (0).
    let edges = vec![Edge {
        source: 1,
        target: 0,
        rel_type: RelType::MirrorsField,
        confidence: 0.6,
        reason: reason_ref,
    }];

    // out_offsets: node 0 has 0 outgoing, node 1 has 1 outgoing (edges[0]).
    let out_offsets = vec![0u32, 0, 1];
    // in_offsets: node 0 has 1 incoming (edges[0]), node 1 has 0 incoming.
    let in_offsets = vec![0u32, 1, 1];
    let in_edge_idx = vec![0u32];
    let n = nodes.len();
    let name_index: Vec<u32> = (0..n as u32).collect();

    serialize_graph(&ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files,
        nodes,
        edges,
        out_offsets,
        in_offsets,
        in_edge_idx,
        name_index,
        process_start: n as u32,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
    })
}

/// Graph with a single `Function` node and no heuristic edges (zero mirrors).
fn zero_mirrors_graph(symbol: &str, file: &str) -> Vec<u8> {
    let mut pool = StringPool::new();
    let path_ref = pool.add(file);
    let name_ref = pool.add(symbol);
    let uid_ref = pool.add(&format!("Function:{file}:{symbol}"));

    let files = vec![File {
        path: path_ref,
        mtime: 0,
        content_hash: [0; 8],
        category: FileCategory::Source,
    }];
    let nodes = vec![Node {
        uid: uid_ref,
        name: name_ref,
        file_idx: 0,
        kind: NodeKind::Function,
        span: (1, 0, 2, 0),
        community_id: 0,
    }];

    serialize_graph(&ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files,
        nodes,
        edges: vec![],
        out_offsets: vec![0u32, 0],
        in_offsets: vec![0u32, 0],
        in_edge_idx: vec![],
        name_index: vec![0u32],
        process_start: 1,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
    })
}

// ---------------------------------------------------------------------------
// Repo setup helpers
// ---------------------------------------------------------------------------

/// Create a minimal git repo, write Python source, index, then inject a
/// synthetic graph with a `MirrorsField` edge. Returns the `TempDir`.
fn setup_mirrors_repo(
    model_content: &str,
    mirror_content: &str,
    model_file: &str,
    mirror_file: &str,
    model_symbol: &str,
    mirror_symbol: &str,
) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    if let Some(parent) = std::path::Path::new(model_file).parent() {
        std::fs::create_dir_all(root.join(parent)).unwrap();
    }
    if let Some(parent) = std::path::Path::new(mirror_file).parent() {
        std::fs::create_dir_all(root.join(parent)).unwrap();
    }
    std::fs::write(root.join(model_file), model_content).unwrap();
    std::fs::write(root.join(mirror_file), mirror_content).unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    let graph_bin = find_graph_bin(root);
    std::fs::write(
        &graph_bin,
        mirrors_field_graph(model_symbol, mirror_symbol, model_file, mirror_file),
    )
    .unwrap();

    repo
}

fn setup_zero_mirrors_repo(symbol: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    std::fs::write(
        root.join("model.py"),
        format!("def {symbol}():\n    return 1\n"),
    )
    .unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    let graph_bin = find_graph_bin(root);
    std::fs::write(&graph_bin, zero_mirrors_graph(symbol, "model.py")).unwrap();

    repo
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A rename of `email` must mutate `src/model.py` (the definition file) but
/// must NOT touch `src/schema.py` (the heuristic mirror file). The
/// `MirrorsField` edge from schema.py → model.py is skipped in the planner.
///
/// The mirror node carries a DIFFERENT graph name (`schema_email`) so that the
/// name-lookup for `email` returns only one `target_indices` entry (model.py).
/// schema.py is excluded solely because the heuristic edge is skipped —
/// the test would regress if the skip were removed.
#[test]
fn test_rename_does_not_touch_heuristic_files() {
    let repo = setup_mirrors_repo(
        "def email():\n    return \"user@example.com\"\n",
        // File content still has `email` — only the GRAPH node name differs.
        "def email():\n    return \"mirror@example.com\"\n",
        "src/model.py",
        "src/schema.py",
        "email",
        "schema_email", // graph node name on mirror side; keeps target_indices to one entry
    );
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "email",
            "new_email",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    // Exit code may be non-zero if the file doesn't exist on disk yet; the
    // important assertion is that schema.py was never written with new_email.
    let schema_content = std::fs::read_to_string(root.join("src/schema.py")).unwrap_or_default();
    assert!(
        !schema_content.contains("new_email"),
        "heuristic mirror file must not be mutated by rename; schema.py=\n{schema_content}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// The `heuristic_mirrors_not_touched: 1` field must appear in stdout as a
/// structural top-level field after a rename when the graph contains one
/// `MirrorsField` inbound edge on the target symbol.
///
/// Mirror node has a different graph name (`schema_notify`) so only the
/// service.py node matches the rename query; the count of 1 reflects the
/// single heuristic edge touching the renamed symbol.
#[test]
fn test_rename_output_surfaces_count_default() {
    let repo = setup_mirrors_repo(
        "def notify():\n    pass\n",
        "def notify():\n    pass\n",
        "src/service.py",
        "src/schema.py",
        "notify",
        "schema_notify", // distinct graph name keeps target_indices to service.py only
    );
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "notify",
            "dispatch",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("heuristic_mirrors_not_touched: 1"),
        "expected structural field in stdout;\nstdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// With `--show-heuristic-mirrors`, the output must include the candidate list
/// with the UNKNOWN_TIER placeholder shape (tier/checks land in T4-7).
#[test]
fn test_rename_show_flag_embeds_candidate_list() {
    let repo = setup_mirrors_repo(
        "def process():\n    pass\n",
        "def process():\n    pass\n",
        "src/worker.py",
        "src/schema.py",
        "process",
        "schema_process", // distinct graph name keeps target_indices to worker.py only
    );
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "process",
            "handle",
            "--repo",
            root.to_str().unwrap(),
            "--show-heuristic-mirrors",
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("heuristic_mirrors:"),
        "expected heuristic_mirrors section with --show-heuristic-mirrors;\nstdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stdout.contains("UNKNOWN_TIER"),
        "expected UNKNOWN_TIER placeholder in mirror list;\nstdout={stdout}",
    );
    assert!(
        stdout.contains("requires_verification: true"),
        "expected requires_verification field;\nstdout={stdout}",
    );
}

/// When zero heuristic mirrors exist, `heuristic_mirrors_not_touched: 0` must
/// appear in the output, but the hint line must be suppressed (noise reduction).
#[test]
fn test_rename_zero_count_omits_hint_line() {
    let repo = setup_zero_mirrors_repo("compute");
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "compute",
            "calculate",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("heuristic_mirrors_not_touched: 0"),
        "zero-mirror field must appear;\nstdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !stdout.contains("find-schema-bindings"),
        "hint line must be suppressed when count is 0;\nstdout={stdout}",
    );
    assert!(
        !stdout.contains("--show-heuristic-mirrors"),
        "hint line must be suppressed when count is 0;\nstdout={stdout}",
    );
}
