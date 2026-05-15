//! Integration tests for the `gnx impact` command.
//!
//! Tests cover:
//!   - Positional <name> replaces old --target <UID>
//!   - --kind / --file_path / --relation_types filters
//!   - --high-trust-only default true
//!   - --since <ref> for diff-mode
//!   - <name> and --since mutual exclusion
//!   - Empty callers hint when 0 incoming (upstream)

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
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
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

#[allow(dead_code)]
fn run_impact_stderr(repo: &Path, extra: &[&str]) -> String {
    let mut args = vec!["impact", "--repo", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(gnx_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("impact failed to spawn");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Extract non-start (depth > 0) entries.
fn non_start_kinds(json: &Value) -> Vec<String> {
    json["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
        .map(|e| e["kind"].as_str().unwrap_or_default().to_ascii_lowercase())
        .collect()
}

// ── New positional-name tests ─────────────────────────────────────────────────

#[test]
fn impact_accepts_name_positional() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let result = run_impact(tmp.path(), &["caller", "--direction", "up"]);
    assert!(
        result.get("error").is_none(),
        "impact with positional name returned error: {result}"
    );
    assert_eq!(result["status"], "success", "unexpected result: {result}");
}

#[test]
fn impact_accepts_target_flag_as_alias_for_positional() {
    // `--target` is the named alias for the positional <name>. The graph
    // is empty so the symbol won't resolve, but clap must parse the flag
    // (i.e. the failure should come from "symbol not found", not from
    // "unexpected argument"). This pins the alias against regressions.
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(gnx_bin())
        .args([
            "impact",
            "--target",
            "Function:src/core/lib.ts:caller",
            "--direction",
            "up",
        ])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("gnx failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--target must be accepted (alias for positional); got: {stderr}"
    );
}

#[test]
fn impact_high_trust_only_default_true() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(gnx_bin())
        .args(["impact", "--help"])
        .current_dir(tmp.path())
        .output()
        .expect("gnx failed to spawn");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--high-trust-only"),
        "--high-trust-only not in help: {help}"
    );
    // The description explicitly states "Default ON" to signal the default is true.
    assert!(
        help.contains("Default ON")
            || help.contains("default: true")
            || help.contains("high-trust-only=false"),
        "--high-trust-only description should indicate it defaults to on:\n{help}"
    );
}

#[test]
fn impact_since_ref_runs_diff_mode() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Add a new commit so HEAD~1 is valid.
    std::fs::write(
        tmp.path().join("src/core/lib.ts"),
        SOURCE_CORE.to_string() + "\n// tweak\n",
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(tmp.path())
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
            "tweak",
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let out = Command::new(gnx_bin())
        .args([
            "impact", "--since", "HEAD~1", "--repo", ".", "--format", "json",
        ])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("impact --since failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "--since HEAD~1 failed: stderr={stderr}\nstdout={stdout}"
    );
    // Accept "changed" in output, or "0 changes" / empty message.
    assert!(
        stdout.contains("changed") || stdout.contains("since") || stdout.contains("changes"),
        "--since output doesn't mention changes:\nstdout={stdout}"
    );
}

#[test]
fn impact_name_and_since_mutually_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(gnx_bin())
        .args(["impact", "foo", "--since", "HEAD~1"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("gnx failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "foo + --since should be rejected but process succeeded"
    );
    assert!(
        stderr.contains("conflict")
            || stderr.contains("cannot be used")
            || stderr.contains("error"),
        "expected conflict error:\nstderr={stderr}"
    );
}

#[test]
fn impact_empty_callers_includes_explanation() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // `helper` is called BY `caller`, but `extraHelper` in the extra file has
    // no callers — it's a leaf. Use it to trigger the empty-upstream hint.
    let out = Command::new(gnx_bin())
        .args([
            "impact",
            "extraHelper",
            "--direction",
            "up",
            "--repo",
            ".",
            "--format",
            "json",
            "--high-trust-only=false",
        ])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("impact failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Command must succeed.
    assert!(
        out.status.success(),
        "impact extraHelper failed:\nstderr={stderr}\nstdout={stdout}"
    );

    // Parse JSON from stdout.
    let json_start = stdout.find('{');
    if let Some(pos) = json_start {
        let json: Value = serde_json::from_str(&stdout[pos..]).unwrap_or(Value::Null);
        let impact_arr = json["impact"].as_array();
        let non_start = impact_arr
            .map(|arr| {
                arr.iter()
                    .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
                    .count()
            })
            .unwrap_or(0);
        if non_start == 0 {
            assert!(
                stderr.contains("entry point")
                    || stderr.contains("dead")
                    || stderr.contains("direction")
                    || stderr.contains("--direction"),
                "missing empty-result hint in stderr:\n{stderr}\nstdout={stdout}"
            );
        }
    }
}

// ── Updated versions of old filter tests (now using positional name) ─────────

#[test]
fn impact_kind_filter_drops_non_matching_results() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Baseline downstream from `caller` should reach the helper function AND
    // the Greeter class/methods (mix of kinds).
    let baseline = run_impact(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "down",
            "--depth",
            "5",
            "--high-trust-only=false",
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
            "caller",
            "--direction",
            "down",
            "--depth",
            "5",
            "--high-trust-only=false",
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

    let filtered = run_impact(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "down",
            "--depth",
            "5",
            "--high-trust-only=false",
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

    let only_extends = run_impact(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "down",
            "--depth",
            "5",
            "--high-trust-only=false",
            "--relation_types",
            "extends",
        ],
    );
    let extends_count = only_extends["impact"].as_array().unwrap().len();
    assert_eq!(
        extends_count, 1,
        "with --relation_types extends, only the start node should remain: {only_extends}"
    );

    let baseline = run_impact(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "down",
            "--depth",
            "5",
            "--high-trust-only=false",
        ],
    );
    let baseline_count = baseline["impact"].as_array().unwrap().len();
    assert!(
        baseline_count > extends_count,
        "baseline must traverse more than the --relation_types extends path: baseline={baseline}"
    );
}

#[test]
fn impact_snake_case_alias_accepts_underscored_flag_name() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let kebab = run_impact(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "up",
            "--depth",
            "3",
            "--high-trust-only=false",
        ],
    );
    let snake = run_impact(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "up",
            "--depth",
            "3",
            "--high_trust_only=false",
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
