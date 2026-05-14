//! Integration tests for the new `gnx impact` flags:
//!   --kind            (emission filter on result entries by node kind)
//!   --file_path       (substring filter on result entries by file path)
//!   --relation_types  (BFS edge filter — actually narrows traversal shape)
//!   snake_case alias  (--high_trust_only ↔ --high-trust-only)

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// TypeScript fixture exercising mixed node kinds + multiple rel types.
///
/// Graph (downstream from `caller`):
///   caller --calls--> helper
///   caller --calls--> Greeter (constructor reference / accesses)
///
/// Inheritance (downstream from `Base`):
///   Greeter --extends--> Base
///
/// Both kinds (Function + Class + Method) appear, so `--kind function`
/// must drop class/method emission. Two `helper`-style function callees in
/// extra/ let `--file_path` filter to substring "core".
const SOURCE_CORE: &str = r#"
export class Base {
    baseMethod(): number { return 1; }
}

export class Greeter extends Base {
    greet(): string { return "hi"; }
}

export function helper(): number {
    return 1;
}

export function caller(): number {
    const g = new Greeter();
    return helper();
}
"#;

const SOURCE_EXTRA: &str = r#"
export function extraHelper(): number {
    return 42;
}
"#;

fn init_repo_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir_all(repo.join("src/core")).unwrap();
    std::fs::create_dir_all(repo.join("src/extra")).unwrap();
    std::fs::write(repo.join("src/core/lib.ts"), SOURCE_CORE).unwrap();
    std::fs::write(repo.join("src/extra/lib.ts"), SOURCE_EXTRA).unwrap();

    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(gnx_bin())
        .args(["analyze", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("analyze failed to spawn");
    assert!(
        out.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_impact(repo: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["impact", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(gnx_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("impact failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return JSON: {err}\nstdout={stdout}"))
}

/// Extract non-start (depth > 0) entries — used for filter assertions where
/// the start node is exempt from --kind / --file_path emission filtering.
fn non_start_kinds(json: &Value) -> Vec<String> {
    json["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
        .map(|e| {
            e["kind"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase()
        })
        .collect()
}

#[test]
fn impact_kind_filter_drops_non_matching_results() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Baseline downstream from `caller` should reach the helper function AND
    // the Greeter class/methods (mix of kinds).
    let baseline = run_impact(
        tmp.path(),
        &[
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "downstream",
            "--depth",
            "5",
        ],
    );
    let baseline_kinds = non_start_kinds(&baseline);
    assert!(
        baseline_kinds.iter().any(|k| k == "function"),
        "baseline missing function-kind descendants: {baseline}"
    );

    // --kind function: only function-kind result entries past the start node.
    let filtered = run_impact(
        tmp.path(),
        &[
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "downstream",
            "--depth",
            "5",
            "--kind",
            "function",
        ],
    );
    let filtered_kinds = non_start_kinds(&filtered);
    assert!(
        !filtered_kinds.is_empty(),
        "--kind function should still produce at least one descendant entry: {filtered}"
    );
    for k in &filtered_kinds {
        assert_eq!(
            k, "function",
            "--kind function leaked a non-function entry ({k}): {filtered}"
        );
    }
}

#[test]
fn impact_file_path_filter_keeps_substring_matches() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // We use `extraHelper` as the target so the start node lives in
    // src/extra/lib.ts. With --file_path core we expect every NON-start
    // entry (if any) to have "core" in its filePath, and the start node
    // itself remains (it is exempt from the filter).
    let filtered = run_impact(
        tmp.path(),
        &[
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "downstream",
            "--depth",
            "5",
            "--file_path",
            "src/core",
        ],
    );

    let entries = filtered["impact"].as_array().unwrap();
    assert!(
        !entries.is_empty(),
        "filtered impact unexpectedly empty: {filtered}"
    );
    for entry in entries {
        let depth = entry["depth"].as_u64().unwrap_or(0);
        let path = entry["filePath"].as_str().unwrap_or("");
        if depth > 0 {
            assert!(
                path.contains("src/core"),
                "--file_path src/core leaked non-matching entry ({path}): {filtered}"
            );
        }
    }
}

#[test]
fn impact_relation_types_filter_short_circuits_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Downstream from `caller`, the baseline includes both `calls` (to
    // helper, Greeter constructor, etc.) and indirect edges. Filtering to
    // only `extends` from `caller` should yield NO descendants because
    // `caller` is a function and has no extends-out edges.
    let only_extends = run_impact(
        tmp.path(),
        &[
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "downstream",
            "--depth",
            "5",
            "--relation_types",
            "extends",
        ],
    );
    let extends_count = only_extends["impact"].as_array().unwrap().len();
    assert_eq!(
        extends_count, 1,
        "with --relation_types extends, only the start node should remain (BFS halts immediately): {only_extends}"
    );

    // Sanity baseline: without the filter, traversal reaches more nodes.
    let baseline = run_impact(
        tmp.path(),
        &[
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "downstream",
            "--depth",
            "5",
        ],
    );
    let baseline_count = baseline["impact"].as_array().unwrap().len();
    assert!(
        baseline_count > extends_count,
        "baseline must traverse more than the --relation_types extends path (baseline={baseline_count}, extends={extends_count}): baseline={baseline} filtered={only_extends}"
    );
}

#[test]
fn impact_snake_case_alias_accepts_underscored_flag_name() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Both kebab and snake forms must yield the same result envelope on a
    // graph where neither filter actually drops anything. We compare the
    // `impact` arrays bit-for-bit to prove the alias is wired (not just
    // accepted by clap silently).
    let kebab = run_impact(
        tmp.path(),
        &[
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "upstream",
            "--depth",
            "3",
            "--high-trust-only",
        ],
    );
    let snake = run_impact(
        tmp.path(),
        &[
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "upstream",
            "--depth",
            "3",
            "--high_trust_only",
        ],
    );

    assert_eq!(
        kebab["status"], "success",
        "kebab call did not succeed: {kebab}"
    );
    assert_eq!(
        snake["status"], "success",
        "snake call did not succeed: {snake}"
    );
    assert_eq!(
        kebab["impact"], snake["impact"],
        "--high_trust_only must produce identical impact array to --high-trust-only.\nkebab={kebab}\nsnake={snake}"
    );
}
