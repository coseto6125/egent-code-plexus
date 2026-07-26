//! Integration tests for `ecp find-schema-bindings` (T4-8).
//!
//! Each test builds a synthetic `graph.bin` with `SchemaField` nodes and
//! `MirrorsField` edges, injects it into an indexed repo, and asserts the
//! JSON output of the command.

use ecp_core::graph::{NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
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
    let mut fx = GraphFixture::new();

    // Build nodes: one Class per (file, owner), one SchemaField per SfSpec.
    // Layout: class nodes first, then SchemaField nodes.
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

    let class_ids: Vec<u32> = class_keys
        .iter()
        .map(|ck| {
            let id = fx.node(NodeKind::Class, ck.file, ck.owner);
            fx.span(id, (1, 0, 50, 0));
            id
        })
        .collect();

    let sf_ids: Vec<u32> = sfs
        .iter()
        .map(|sf| {
            let id = fx.node_owned(NodeKind::SchemaField, sf.file, sf.owner, sf.name);
            fx.span(id, (sf.line, 0, sf.line, 0));
            id
        })
        .collect();

    // HasProperty edges: Class → its SchemaField.
    for (sf_local_idx, sf) in sfs.iter().enumerate() {
        let class_idx = class_keys
            .iter()
            .position(|k| k.file == sf.file && k.owner == sf.owner)
            .expect("class not found");
        fx.edge_with(
            class_ids[class_idx],
            sf_ids[sf_local_idx],
            RelType::HasProperty,
            1.0,
            "post_process:schema_field:has_property",
        );
    }

    // MirrorsField edges.
    for &(src_sf, tgt_sf, conf) in mirrors {
        fx.edge_with(
            sf_ids[src_sf],
            sf_ids[tgt_sf],
            RelType::MirrorsField,
            conf,
            "post_process:schema_field:mirrors_field",
        );
    }

    fx.into_bytes()
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

fn run_verb_json(repo: &Path, verb: &[&str], field: &str) -> (bool, Value) {
    let mut args = verb.to_vec();
    args.extend_from_slice(&[field, "--format", "json"]);
    let out = Command::new(ecp_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .unwrap_or_else(|e| panic!("{args:?} failed to spawn: {e}"));
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

fn run_find_schema_bindings_json(repo: &Path, field: &str) -> (bool, Value) {
    run_verb_json(repo, &["find-schema-bindings"], field)
}

fn run_heuristics_schema_bindings_json(repo: &Path, field: &str) -> (bool, Value) {
    run_verb_json(repo, &["heuristics", "schema-bindings"], field)
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

// ── New-path coverage (`ecp heuristics schema-bindings`) ─────────────────────

/// `ecp heuristics schema-bindings` must return the same mirrors as the
/// deprecated `ecp find-schema-bindings` for the same graph.
#[test]
fn heuristics_schema_bindings_new_path_matches_deprecated_path() {
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
        &[(0, 1, 0.9)],
    );
    std::fs::write(find_graph_bin(tmp.path()), bytes).unwrap();

    let (ok_old, val_old) = run_find_schema_bindings_json(tmp.path(), "User.email");
    let (ok_new, val_new) = run_heuristics_schema_bindings_json(tmp.path(), "User.email");
    assert!(ok_old);
    assert!(ok_new, "new path must exit 0 for found field");
    assert_eq!(
        val_new["mirrors"].as_array().map(|a| a.len()),
        val_old["mirrors"].as_array().map(|a| a.len()),
        "new `heuristics schema-bindings` path must return same mirror count as deprecated path"
    );
}
