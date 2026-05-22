//! Integration tests for `ecp find-schema-bindings` (T4-8).
//!
//! Each test builds a synthetic `graph.bin` with `SchemaField` nodes and
//! `MirrorsField` edges, injects it into an indexed repo, and asserts the
//! JSON output of the command.

use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// ── Graph builder helpers ─────────────────────────────────────────────────────

/// Spec for a single SchemaField node.
struct SfSpec {
    file: &'static str, // e.g. "models/user.py"
    owner: &'static str,
    name: &'static str,
    line: u32,
}

/// Build a synthetic graph with SchemaField nodes and MirrorsField edges.
///
/// `sfs` lists the SchemaField specs; each spec gets:
///   - a File node (deduplicated by path)
///   - a Class node (one per unique (file, owner) pair)
///   - a SchemaField node
///   - a HasProperty edge from the Class → SchemaField
///
/// `mirrors` is a list of `(src_sf_idx, tgt_sf_idx, confidence)` index pairs
/// into the `sfs` slice.
fn build_graph(sfs: &[SfSpec], mirrors: &[(usize, usize, f32)]) -> Vec<u8> {
    let mut pool = StringPool::new();

    // Collect unique files.
    let mut file_paths: Vec<&str> = sfs.iter().map(|s| s.file).collect();
    file_paths.dedup();
    file_paths.sort_unstable();
    file_paths.dedup();

    let file_refs: Vec<StrRef> = file_paths.iter().map(|p| pool.add(p)).collect();
    let files: Vec<File> = file_refs
        .iter()
        .map(|r| File {
            path: *r,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        })
        .collect();

    let file_idx_for = |path: &str| -> u32 {
        file_paths
            .iter()
            .position(|&p| p == path)
            .expect("file not found") as u32
    };

    // Build nodes: one Class per (file, owner), one SchemaField per SfSpec.
    // Layout: class nodes first (0..n_classes), then SchemaField nodes.
    struct ClassKey {
        file: &'static str,
        owner: &'static str,
    }
    let mut class_keys: Vec<ClassKey> = Vec::new();
    for sf in sfs {
        if !class_keys
            .iter()
            .any(|k| k.file == sf.file && k.owner == sf.owner)
        {
            class_keys.push(ClassKey {
                file: sf.file,
                owner: sf.owner,
            });
        }
    }
    let n_classes = class_keys.len();

    let mut nodes: Vec<Node> = Vec::new();
    // Class nodes.
    for ck in &class_keys {
        let uid = ecp_core::uid::compute(NodeKind::Class, ck.file, None, ck.owner);
        let name_ref = pool.add(ck.owner);
        nodes.push(Node {
            uid,
            name: name_ref,
            file_idx: file_idx_for(ck.file),
            kind: NodeKind::Class,
            span: (1, 0, 50, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        });
    }

    // SchemaField nodes (indices: n_classes..n_classes+sfs.len()).
    let sf_node_base = n_classes;
    for sf in sfs {
        let uid = ecp_core::uid::compute(NodeKind::SchemaField, sf.file, Some(sf.owner), sf.name);
        let name_ref = pool.add(sf.name);
        let owner_ref = pool.add(sf.owner);
        nodes.push(Node {
            uid,
            name: name_ref,
            file_idx: file_idx_for(sf.file),
            kind: NodeKind::SchemaField,
            span: (sf.line, 0, sf.line, 0),
            community_id: 0,
            owner_class: owner_ref,
            content_hash: 0,
        });
    }

    let n = nodes.len();

    // Build edges: HasProperty (class → sf) + MirrorsField (sf → sf).
    let reason_has_property = pool.add("post_process:schema_field:has_property");
    let reason_mirror = pool.add("post_process:schema_field:mirrors_field");

    let mut edges: Vec<Edge> = Vec::new();

    // HasProperty edges.
    for (sf_local_idx, sf) in sfs.iter().enumerate() {
        let class_idx = class_keys
            .iter()
            .position(|k| k.file == sf.file && k.owner == sf.owner)
            .expect("class not found") as u32;
        let sf_node_idx = (sf_node_base + sf_local_idx) as u32;
        edges.push(Edge {
            source: class_idx,
            target: sf_node_idx,
            rel_type: RelType::HasProperty,
            confidence: 1.0,
            reason: reason_has_property,
        });
    }

    // MirrorsField edges.
    for &(src_sf, tgt_sf, conf) in mirrors {
        edges.push(Edge {
            source: (sf_node_base + src_sf) as u32,
            target: (sf_node_base + tgt_sf) as u32,
            rel_type: RelType::MirrorsField,
            confidence: conf,
            reason: reason_mirror,
        });
    }

    // Build CSR offsets. out_offsets[i] = cumulative out-degree for node i.
    let mut out_counts = vec![0u32; n];
    let mut in_counts = vec![0u32; n];
    for e in &edges {
        out_counts[e.source as usize] += 1;
        in_counts[e.target as usize] += 1;
    }
    let mut out_offsets = vec![0u32; n + 1];
    let mut in_offsets = vec![0u32; n + 1];
    for i in 0..n {
        out_offsets[i + 1] = out_offsets[i] + out_counts[i];
        in_offsets[i + 1] = in_offsets[i] + in_counts[i];
    }

    // Reorder edges into CSR order (sorted by source).
    let mut sorted_edges = edges.clone();
    sorted_edges.sort_by_key(|e| e.source);
    // in_edge_idx: for each node (as target), list sorted_edges indices.
    let mut in_edge_idx: Vec<u32> = Vec::new();
    for tgt_node in 0..n {
        for (idx, e) in sorted_edges.iter().enumerate() {
            if e.target as usize == tgt_node {
                in_edge_idx.push(idx as u32);
            }
        }
    }

    let name_index: Vec<ecp_core::graph::NameIndexEntry> = Vec::new();

    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        string_pool: pool.bytes,
        files,
        nodes,
        edges: sorted_edges,
        out_offsets,
        in_offsets,
        in_edge_idx,
        name_index,
        process_start: n as u32,
        ..Default::default()
    };

    rkyv::to_bytes::<Error>(&graph)
        .expect("serialize synthetic graph")
        .into_vec()
}

// ── Repo fixture helpers ─────────────────────────────────────────────────────

fn init_repo_and_index(repo: &Path) {
    std::fs::create_dir_all(repo.join("models")).unwrap();
    std::fs::write(
        repo.join("models/user.py"),
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    email: str\n",
    )
    .unwrap();

    let run_git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&["init", "-q", "-b", "main"]);
    run_git(&["add", "-A"]);
    run_git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index spawn");
    assert!(
        out.status.success(),
        "admin index: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find_graph_bin(repo: &Path) -> std::path::PathBuf {
    fn walk(dir: &Path) -> Option<std::path::PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                return Some(p);
            }
            if p.is_dir() {
                if let Some(f) = walk(&p) {
                    return Some(f);
                }
            }
        }
        None
    }
    walk(&repo.join(".ecp")).expect("graph.bin not found")
}

fn run_find_schema_bindings_json(repo: &Path, field: &str) -> (bool, Value) {
    let out = Command::new(ecp_bin())
        .args(["find-schema-bindings", field, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp find-schema-bindings spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "no JSON in stdout (exit={}): {stdout}",
            out.status.code().unwrap_or(-1)
        )
    });
    let val: Value = serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|e| panic!("JSON parse: {e}\n{stdout}"));
    (out.status.success(), val)
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Pydantic `User.email: str` + SQLAlchemy `User.email = Column(String)` →
/// mirrors list contains the SQLA entry with tier `LIKELY_RELATED`.
#[test]
fn pydantic_to_sqlalchemy_surface() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph(
        &[
            SfSpec {
                file: "models/pyd.py",
                owner: "User",
                name: "email",
                line: 4,
            },
            SfSpec {
                file: "models/sqla.py",
                owner: "User",
                name: "email",
                line: 5,
            },
        ],
        // pyd (sf_idx=0) --MirrorsField--> sqla (sf_idx=1) at confidence 0.9
        &[(0, 1, 0.9)],
    );
    std::fs::write(find_graph_bin(tmp.path()), bytes).unwrap();

    let (ok, val) = run_find_schema_bindings_json(tmp.path(), "User.email");
    assert!(ok, "command must exit 0 when field found");
    assert_eq!(val["field"].as_str(), Some("User.email"));

    let mirrors = val["mirrors"].as_array().expect("mirrors array");
    assert_eq!(mirrors.len(), 1, "exactly one mirror for sqla");

    let m = &mirrors[0];
    assert_eq!(m["name"].as_str(), Some("email"));
    assert_eq!(m["tier"].as_str(), Some("LIKELY_RELATED"));
    assert_eq!(m["requires_verification"].as_bool(), Some(true));
    assert!(m["checks"].is_object(), "checks must be an object");
}

/// Bare `email` query returns matches across both `User` and `Admin` classes.
#[test]
fn bare_field_lists_all_owners() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph(
        &[
            SfSpec {
                file: "models/user_pyd.py",
                owner: "User",
                name: "email",
                line: 4,
            },
            SfSpec {
                file: "models/user_sqla.py",
                owner: "User",
                name: "email",
                line: 5,
            },
            SfSpec {
                file: "schemas/admin.py",
                owner: "Admin",
                name: "email",
                line: 4,
            },
        ],
        // User pair mirrors each other; Admin has no mirror edge.
        &[(0, 1, 0.9)],
    );
    std::fs::write(find_graph_bin(tmp.path()), bytes).unwrap();

    // Bare "email" — no owner filter.
    let (ok, val) = run_find_schema_bindings_json(tmp.path(), "email");
    assert!(ok);

    // mirrors: node 0 (User/user_pyd.py) → node 1 (User/user_sqla.py)
    let mirrors = val["mirrors"].as_array().expect("mirrors");
    assert!(!mirrors.is_empty(), "mirrors must not be empty");

    // blind_spot_candidates: Admin.email has no mirror edge and is not in
    // the query's matching set when bare field is queried from all owners.
    // The Admin node matches "email" by name but has no MirrorsField edge.
    let bsc = val["blind_spot_candidates"].as_array().expect("bsc");
    let admin_in_bsc = bsc.iter().any(|e| e["owner"].as_str() == Some("Admin"));
    assert!(
        admin_in_bsc,
        "Admin.email must appear in blind_spot_candidates"
    );
}

/// A field with no related nodes returns empty arrays — no fabrication.
#[test]
fn no_mirrors_no_blindspot_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph(
        &[SfSpec {
            file: "models/lonely.py",
            owner: "Standalone",
            name: "unique_field",
            line: 3,
        }],
        &[], // no mirror edges
    );
    std::fs::write(find_graph_bin(tmp.path()), bytes).unwrap();

    let (ok, val) = run_find_schema_bindings_json(tmp.path(), "unique_field");
    assert!(ok, "exit 0 when field exists even with no mirrors");
    assert_eq!(
        val["mirrors"].as_array().map(Vec::len),
        Some(0),
        "mirrors must be empty"
    );
    assert_eq!(
        val["blind_spot_candidates"].as_array().map(Vec::len),
        Some(0),
        "blind_spot_candidates must be empty"
    );
    assert_eq!(val["summary"]["mirrors_count"].as_u64(), Some(0));
    assert_eq!(val["summary"]["blind_spot_count"].as_u64(), Some(0));
}

/// Every entry in `mirrors` and `blind_spot_candidates` must carry the
/// `requires_verification` field set to `true` — structural gate for LLM.
#[test]
fn output_carries_requires_verification_field() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    let bytes = build_graph(
        &[
            SfSpec {
                file: "models/pyd.py",
                owner: "User",
                name: "email",
                line: 4,
            },
            SfSpec {
                file: "models/sqla.py",
                owner: "User",
                name: "email",
                line: 5,
            },
            SfSpec {
                file: "schemas/admin.py",
                owner: "Admin",
                name: "email",
                line: 3,
            },
        ],
        &[(0, 1, 0.9)],
    );
    std::fs::write(find_graph_bin(tmp.path()), bytes).unwrap();

    let (ok, val) = run_find_schema_bindings_json(tmp.path(), "email");
    assert!(ok);

    for entry in val["mirrors"].as_array().unwrap_or(&vec![]) {
        assert_eq!(
            entry["requires_verification"].as_bool(),
            Some(true),
            "mirrors entry missing requires_verification: {entry}"
        );
    }
    for entry in val["blind_spot_candidates"].as_array().unwrap_or(&vec![]) {
        assert_eq!(
            entry["requires_verification"].as_bool(),
            Some(true),
            "blind_spot_candidates entry missing requires_verification: {entry}"
        );
    }
}

/// `ecp find-schema-bindings DoesNotExist.email` must exit non-zero and
/// return a structured `not_found` payload (not a panic / crash).
#[test]
fn field_with_no_node_returns_clear_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    // Graph has no SchemaField nodes at all — real indexer output.
    // (admin index of a plain Python file emits no SchemaFields.)
    let out = Command::new(ecp_bin())
        .args([
            "find-schema-bindings",
            "DoesNotExist.email",
            "--format",
            "json",
        ])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("spawn");

    assert!(
        !out.status.success(),
        "must exit non-zero for missing field"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON output:\n{stdout}"));
    let val: Value = serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|e| panic!("JSON parse: {e}\n{stdout}"));

    assert_eq!(
        val["status"].as_str(),
        Some("not_found"),
        "payload.status must be 'not_found'"
    );
    // Must be structured arrays, not missing keys.
    assert!(
        val["mirrors"].is_array(),
        "mirrors must be array in not_found"
    );
    assert!(
        val["blind_spot_candidates"].is_array(),
        "blind_spot_candidates must be array in not_found"
    );
}
