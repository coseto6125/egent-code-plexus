//! T-H1: heuristic edge filter integration tests.
//!
//! Each test builds a minimal synthetic `ZeroCopyGraph`, injects it as
//! `graph.bin` after `admin index`, then drives `ecp impact` via `Command`.

use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Minimal TypeScript fixture so `admin index` succeeds and creates `.ecp/`.
const SOURCE_A: &str = "export function alpha() { return 1; }\n";
const SOURCE_B: &str =
    "import { alpha } from \"./a\";\nexport function beta() { return alpha(); }\n";

fn init_repo(repo: &Path) {
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

/// Two `Function` nodes: `source` (idx 0) and `target` (idx 1), linked by a
/// single directed edge of the given `rel_type` (source → target).
fn synthetic_graph_two_nodes(rel_type: RelType, reason_str: &str) -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let source = fx.func("src/a.ts", "source");
    fx.span(source, (1, 0, 3, 0));
    let target = fx.func("src/b.ts", "target");
    fx.span(target, (2, 0, 4, 0));
    // source (0) → target (1)
    fx.edge_with(source, target, rel_type, 0.6, reason_str);
    fx.into_bytes()
}

/// Clean graph: `source` and `target` connected by a deterministic `Calls`
/// edge (no heuristic edges at all).
fn synthetic_graph_clean() -> Vec<u8> {
    synthetic_graph_two_nodes(RelType::Calls, "call")
}

fn run_ecp_impact(repo: &Path, extra_args: &[&str]) -> serde_json::Value {
    let mut cmd_args = vec!["impact", "source", "--format", "json", "--repo", "."];
    cmd_args.extend_from_slice(extra_args);
    let out = Command::new(ecp_bin())
        .args(&cmd_args)
        .current_dir(repo)
        .env("HOME", repo)
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
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"))
}

/// Default `ecp impact` MUST traverse a `MirrorsField` heuristic edge and
/// expose it in `heuristic_callers`. `hidden_heuristic_edges` must be 0.
/// Each entry in `heuristic_callers` must carry `requires_verification: true`.
#[test]
fn test_default_includes_heuristic_edges() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(
        &graph_bin,
        synthetic_graph_two_nodes(RelType::MirrorsField, "schema-mirror-heuristic"),
    )
    .unwrap();

    // direction=down so we traverse from source outward.
    let val = run_ecp_impact(tmp.path(), &["--direction", "down"]);

    // `target` must appear in `heuristic_callers` (traversed by default).
    let callers = val["heuristic_callers"]
        .as_array()
        .unwrap_or_else(|| panic!("`heuristic_callers` key missing from output:\n{val}"));
    let in_heuristic = callers.iter().any(|e| e["name"].as_str() == Some("target"));
    assert!(
        in_heuristic,
        "`target` must appear in `heuristic_callers` by default: {val}"
    );

    // Each entry must be tagged requires_verification: true.
    for entry in callers {
        assert_eq!(
            entry["requires_verification"].as_bool(),
            Some(true),
            "heuristic_callers entry missing requires_verification: {entry}"
        );
    }

    // hidden_heuristic_edges must be 0 (edge was traversed, not hidden).
    let hidden = val["hidden_heuristic_edges"]
        .as_u64()
        .unwrap_or_else(|| panic!("hidden_heuristic_edges missing from output:\n{val}"));
    assert_eq!(
        hidden, 0,
        "expected 0 hidden heuristic edges by default, got {hidden}"
    );
}

/// With `--no-heuristic`, the BFS skips the heuristic edge and counts it as
/// hidden. `hidden_heuristic_edges: 1` must appear in the output.
#[test]
fn test_no_heuristic_flag_suppresses() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(
        &graph_bin,
        synthetic_graph_two_nodes(RelType::MirrorsField, "schema-mirror-heuristic"),
    )
    .unwrap();

    let val = run_ecp_impact(tmp.path(), &["--direction", "down", "--no-heuristic"]);

    // `target` must NOT appear in `impact` or `heuristic_callers` (edge suppressed).
    let in_impact = val["impact"]
        .as_array()
        .map(|arr| arr.iter().any(|e| e["name"].as_str() == Some("target")))
        .unwrap_or(false);
    assert!(
        !in_impact,
        "`target` must not appear in `impact` with --no-heuristic: {val}"
    );

    // hidden_heuristic_edges must be 1.
    let hidden = val["hidden_heuristic_edges"]
        .as_u64()
        .unwrap_or_else(|| panic!("hidden_heuristic_edges missing from output:\n{val}"));
    assert_eq!(
        hidden, 1,
        "expected 1 hidden heuristic edge with --no-heuristic, got {hidden}"
    );
}

/// Clean graph with no heuristic edges: `hidden_heuristic_edges` is present
/// and equals 0 (noise-reduction parity with `hidden_edges` being omitted when
/// 0, but heuristic count is always written so callers can branch on the field).
#[test]
fn test_zero_heuristic_edges_renders_zero() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(&graph_bin, synthetic_graph_clean()).unwrap();

    let val = run_ecp_impact(tmp.path(), &["--direction", "down"]);

    let hidden = val["hidden_heuristic_edges"]
        .as_u64()
        .unwrap_or_else(|| panic!("hidden_heuristic_edges missing from output:\n{val}"));
    assert_eq!(
        hidden, 0,
        "expected 0 hidden heuristic edges on clean graph, got {hidden}"
    );
}

/// `--explain-confidence` emits the `explain_confidence` block with
/// `threshold: 0.85` and `edges_filtered_by_tier`.
#[test]
fn test_explain_confidence_flag_emits_block() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(
        &graph_bin,
        synthetic_graph_two_nodes(RelType::MirrorsField, "schema-mirror-heuristic"),
    )
    .unwrap();

    let val = run_ecp_impact(tmp.path(), &["--direction", "down", "--explain-confidence"]);

    let ec = &val["explain_confidence"];
    assert!(
        !ec.is_null(),
        "`explain_confidence` block missing from output:\n{val}"
    );
    let threshold = ec["threshold"]
        .as_f64()
        .unwrap_or_else(|| panic!("`explain_confidence.threshold` missing:\n{val}"));
    assert!(
        (threshold - 0.85).abs() < 1e-5,
        "expected threshold 0.85, got {threshold}"
    );
    assert!(
        ec["edges_filtered_by_tier"].is_object(),
        "`edges_filtered_by_tier` must be an object:\n{val}"
    );
}

/// A deterministic `Calls` edge below 0.7 confidence stays in `impact` (no
/// row moves) and its row carries `requires_verification: true`. Before the
/// fix the key was absent from `impact` rows: `as_bool()` is `None`.
#[test]
fn test_low_confidence_calls_row_requires_verification() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(&graph_bin, synthetic_graph_clean()).unwrap();

    let val = run_ecp_impact(tmp.path(), &["--direction", "down"]);

    let rows = val["impact"].as_array().expect("impact array");
    let row = |name: &str| {
        rows.iter()
            .find(|e| e["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("no `{name}` row in impact: {val}"))
    };
    assert_eq!(
        row("target")["requires_verification"].as_bool(),
        Some(true),
        "0.6 Calls row must be tagged: {val}"
    );
    assert_eq!(
        row("source")["requires_verification"].as_bool(),
        Some(false),
        "the start row is never tagged: {val}"
    );
    assert!(
        val["heuristic_callers"]
            .as_array()
            .is_none_or(|h| h.is_empty()),
        "a Calls row must not move into heuristic_callers: {val}"
    );
}

/// Stands for: a test caller reached only through a heuristic edge is an
/// unverified test caller in every direction, never a missing caller, so
/// the symbol is `Uncovered`, not `Orphan`. Before the fix the upstream
/// coverage read only the deterministic bucket (`up`) or reran with
/// heuristics off (`both`): `unverified_test_callers` was 0 and the symbol
/// an orphan.
#[test]
fn test_test_coverage_heuristic_test_caller_counts_unverified_up_and_both() {
    use ecp_core::graph::FunctionMeta;
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    let mut fx = GraphFixture::new();
    let source = fx.func("src/a.ts", "source");
    fx.span(source, (1, 0, 3, 0));
    let probe = fx.func("src/probe.ts", "probe");
    fx.span(probe, (1, 0, 3, 0));
    fx.function_meta(probe, FunctionMeta::FLAG_TEST, &[]);
    fx.edge_with(probe, source, RelType::MirrorsField, 0.9, "schema-mirror");
    std::fs::write(&graph_bin, fx.into_bytes()).unwrap();

    for direction in ["up", "both"] {
        let val = run_ecp_impact(tmp.path(), &["--direction", direction, "--test-coverage"]);
        let coverage = &val["coverage"];
        let uncovered = coverage["uncovered_symbols"]
            .as_array()
            .unwrap_or_else(|| panic!("{direction}: no uncovered_symbols: {val}"));
        assert_eq!(uncovered.len(), 1, "{direction}: {val}");
        assert_eq!(uncovered[0]["name"], "source", "{direction}: {val}");
        assert_eq!(
            uncovered[0]["unverified_test_callers"], 1,
            "{direction}: {val}"
        );
        assert_eq!(uncovered[0]["test_caller_count"], 0, "{direction}: {val}");
        assert_eq!(coverage["summary"]["orphan"], 0, "{direction}: {val}");
    }
}

/// `ecp path` tags a deterministic hop below 0.7 as impact does, and its
/// caveat names the hop as low-confidence. Contract: with heuristic relations
/// off (the default) a sub-0.7 `Calls` step still walks, so the tag is the
/// only thing that keeps the route from reading as resolved. On origin/main
/// the step was untagged and the payload had no caveat.
#[test]
fn test_path_low_confidence_calls_hop_requires_verification() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let graph_bin = find_graph_bin(tmp.path());
    std::fs::write(&graph_bin, synthetic_graph_clean()).unwrap();

    let out = Command::new(ecp_bin())
        .args(["path", "source", "target", "--direction", "down"])
        .args(["--format", "json", "--repo", "."])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp path failed to spawn");
    assert!(
        out.status.success(),
        "ecp path exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let val: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let steps = val["path"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a path: {val}"));
    let tags: Vec<(&str, Option<bool>)> = steps
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap(),
                s["requiresVerification"].as_bool(),
            )
        })
        .collect();
    assert_eq!(
        tags,
        [("source", Some(false)), ("target", Some(true))],
        "{val}"
    );
    let caveat = val["result"].as_str().unwrap_or_default();
    assert!(
        caveat.contains("1 of 1 hops") && caveat.contains("low-confidence (< 0.7)"),
        "the caveat names the low-confidence hop: {val}"
    );
}
