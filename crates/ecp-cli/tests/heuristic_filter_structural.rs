use ecp_core::graph::{Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::pool::StringPool;
use rkyv::rancor::Error;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// TypeScript fixture: two functions so admin index emits producer + consumer.
const SOURCE_A: &str = r#"
export function producer(): number {
    return 42;
}
"#;

const SOURCE_B: &str = r#"
import { producer } from "./a";
export function consumer(): number {
    return producer();
}
"#;

fn init_repo_and_index(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.ts"), SOURCE_A).unwrap();
    std::fs::write(repo.join("src/b.ts"), SOURCE_B).unwrap();

    let run_git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
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
        .expect("admin index spawn failed");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Locate the graph.bin written under `.ecp/` (recursive search, up to 4
/// levels deep, to accommodate the `<repo__hash>/commits/<branch__sha>/`
/// directory structure produced by `admin index`).
fn find_graph_bin(repo: &Path) -> std::path::PathBuf {
    fn walk(dir: &Path, depth: usize) -> Option<std::path::PathBuf> {
        if depth == 0 {
            return None;
        }
        let rd = std::fs::read_dir(dir).ok()?;
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                return Some(p);
            }
            if p.is_dir() {
                if let Some(found) = walk(&p, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&repo.join(".ecp"), 5).expect("graph.bin not found after admin index")
}

/// Build a small synthetic `ZeroCopyGraph` with two `Function` nodes linked by
/// a `MirrorsField` edge. Returns serialized bytes ready for `graph.bin`.
fn synthetic_graph_with_mirrors_field() -> Vec<u8> {
    let mut pool = StringPool::new();

    let file_ref = pool.add("src/a.ts");
    let producer_uid = pool.add("Function:src/a.ts:producer");
    let consumer_uid = pool.add("Function:src/b.ts:consumer");
    let producer_name = pool.add("producer");
    let consumer_name = pool.add("consumer");
    let file_b_ref = pool.add("src/b.ts");
    let reason_ref = pool.add("schema-mirror-heuristic");

    let files = vec![
        File {
            path: file_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
        File {
            path: file_b_ref,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
    ];

    let nodes = vec![
        Node {
            uid: producer_uid,
            name: producer_name,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (2, 0, 4, 0),
            community_id: 0,
        },
        Node {
            uid: consumer_uid,
            name: consumer_name,
            file_idx: 1,
            kind: NodeKind::Function,
            span: (3, 0, 5, 0),
            community_id: 0,
        },
    ];

    // MirrorsField edge: producer (0) → consumer (1)
    let edges = vec![Edge {
        source: 0,
        target: 1,
        rel_type: RelType::MirrorsField,
        confidence: 0.6,
        reason: reason_ref,
    }];

    let n = nodes.len();
    // out_offsets: producer has 1 outgoing edge (edge 0), consumer has 0.
    let out_offsets = vec![0u32, 1u32, 1u32];
    // in_offsets: consumer has 1 incoming (edge 0), producer has 0.
    let in_offsets = vec![0u32, 0u32, 1u32];
    let in_edge_idx = vec![0u32];
    let name_index: Vec<u32> = (0..n as u32).collect();

    let graph = ZeroCopyGraph {
        string_pool: pool.bytes,
        files,
        nodes,
        edges,
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

/// Two `Function` nodes connected by a `MirrorsField` edge: default `ecp impact`
/// must NOT surface that edge in its output.
///
/// The actual `is_heuristic()` filter inside `run_bfs` does not land until
/// T-H1. This test is the structural CI gate: the graph serializes and the
/// test body is fully specified; T-H1 only removes the `#[ignore]` attribute.
#[test]
fn test_impact_default_hides_mirrors_field() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_index(tmp.path());

    // Overwrite the indexed graph.bin with our synthetic one that contains
    // a MirrorsField edge.
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(&graph_bin, synthetic_graph_with_mirrors_field()).unwrap();

    // ecp impact producer --format json (default direction=up, depth=5).
    let out = Command::new(ecp_bin())
        .args(["impact", "producer", "--format", "json", "--repo", "."])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp impact failed to spawn");

    assert!(
        out.status.success(),
        "ecp impact exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout:\n{stdout}"));
    let val: serde_json::Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));

    // No impact entry should carry a MirrorsField via-reason.
    if let Some(impact_arr) = val["impact"].as_array() {
        for entry in impact_arr {
            let via = entry["viaReason"].as_str().unwrap_or("");
            assert_ne!(
                via, "schema-mirror-heuristic",
                "MirrorsField edge must be hidden by default: {entry}"
            );
            assert_ne!(
                via, "mirrors_field",
                "MirrorsField edge must be hidden by default: {entry}"
            );
        }
    }

    // Belt-and-suspenders: raw stdout must not mention the injected reason.
    assert!(
        !stdout.contains("schema-mirror-heuristic"),
        "MirrorsField edge reason leaked into impact output:\n{stdout}"
    );
}
