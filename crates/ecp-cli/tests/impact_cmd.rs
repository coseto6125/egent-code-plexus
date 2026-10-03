//! Integration tests for the `ecp impact` command.
//!
//! Tests cover:
//!   - Positional <name> replaces old --target <UID>
//!   - --kind / --file_path / --relation_types filters
//!   - --high-trust-only default false (recall-first)
//!   - hidden_edges counter when min_conf filter drops edges
//!   - --baseline <ref> for diff-mode
//!   - <name> and --baseline mutual exclusion
//!   - Empty callers hint when 0 incoming (upstream)

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
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

export function duplicateTarget(): number {
    return 1;
}
"#;

const SOURCE_EXTRA: &str = r#"
export function extraHelper(): number {
    return 42;
}

export function duplicateTarget(): number {
    return 2;
}
"#;

fn init_repo_and_analyze(repo: &Path) {
    init_repo_with(
        repo,
        &[
            ("src/core/lib.ts", SOURCE_CORE),
            ("src/extra/lib.ts", SOURCE_EXTRA),
        ],
    );
}

fn init_repo_with(repo: &Path, files: &[(&str, &str)]) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    for (rel, src) in files {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }

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

    let out = Command::new(ecp_bin())
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
    let out = Command::new(ecp_bin())
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
    let out = Command::new(ecp_bin())
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
    let out = Command::new(ecp_bin())
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
        .expect("ecp failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--target must be accepted (alias for positional); got: {stderr}"
    );
}

#[test]
fn impact_high_trust_only_default_false() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(ecp_bin())
        .args(["impact", "--help"])
        .current_dir(tmp.path())
        .output()
        .expect("ecp failed to spawn");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--high-trust-only"),
        "--high-trust-only not in help: {help}"
    );
    // After the recall-first flip, the description states "Default OFF" and
    // tells the user how to opt back into the high-trust filter.
    assert!(
        help.contains("Default OFF")
            || help.contains("default: false")
            || help.contains("--high-trust-only=true"),
        "--high-trust-only description should indicate it defaults to off:\n{help}"
    );
}

#[test]
fn impact_hidden_edges_counted_when_min_conf_filters() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Force every edge to fall below threshold (max real confidence is 1.0)
    // so the BFS visits the start node, considers every outgoing edge, and
    // increments the counter for each one it drops.
    let result = run_impact(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "down",
            "--depth",
            "5",
            "--min-confidence",
            "1.5",
        ],
    );

    let impact_len = result["impact"].as_array().unwrap().len();
    assert_eq!(
        impact_len, 1,
        "all edges should be filtered, only start node remains: {result}"
    );
    let hidden = result["hidden_edges"].as_u64();
    assert!(
        hidden.is_some_and(|n| n > 0),
        "hidden_edges should be present and > 0 when filter drops edges: {result}"
    );
}

#[test]
fn impact_hidden_edges_absent_when_no_filtering() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Default flag set (high-trust-only=false → min_conf=0.0) admits every
    // edge, so the counter stays at zero and the field is omitted to keep
    // happy-path output noise-free.
    let result = run_impact(
        tmp.path(),
        &["caller", "--direction", "down", "--depth", "5"],
    );

    assert!(
        result.get("hidden_edges").is_none(),
        "hidden_edges should be absent when nothing was filtered: {result}"
    );
}

#[test]
fn impact_hidden_edges_footer_emitted_to_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let stderr = run_impact_stderr(
        tmp.path(),
        &[
            "caller",
            "--direction",
            "down",
            "--depth",
            "5",
            "--min-confidence",
            "1.5",
        ],
    );

    assert!(
        stderr.contains("edges hidden") && stderr.contains("--high-trust-only"),
        "stderr footer should mention hidden edges and the flag: {stderr}"
    );
}

#[test]
#[ignore = "baseline mode requires L2 build for baseline SHA after git_guard checkout; needs auto_ensure inside the guard scope (Phase 5+ rewire)"]
fn impact_baseline_ref_runs_diff_mode() {
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

    let out = Command::new(ecp_bin())
        .args([
            "impact",
            "--baseline",
            "HEAD~1",
            "--repo",
            ".",
            "--format",
            "json",
        ])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("impact --baseline failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "--baseline HEAD~1 failed: stderr={stderr}\nstdout={stdout}"
    );
    // Accept "changed" in output, or "0 changes" / empty message.
    assert!(
        stdout.contains("changed") || stdout.contains("baseline") || stdout.contains("changes"),
        "--baseline output doesn't mention changes:\nstdout={stdout}"
    );
}

#[test]
fn impact_name_and_baseline_mutually_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(ecp_bin())
        .args(["impact", "foo", "--baseline", "HEAD~1"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "foo + --baseline should be rejected but process succeeded"
    );
    assert!(
        stderr.contains("conflict")
            || stderr.contains("cannot be used")
            || stderr.contains("error"),
        "expected conflict error:\nstderr={stderr}"
    );
}

#[test]
fn impact_ambiguous_symbol_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = Command::new(ecp_bin())
        .args([
            "impact",
            "duplicateTarget",
            "--repo",
            ".",
            "--format",
            "json",
        ])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("impact failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "ambiguous symbol must exit non-zero so MCP marks isError=true"
    );
    assert!(
        stderr.contains("ambiguous")
            && stderr.contains("--file")
            && stderr.contains("--kind")
            && stderr.contains("candidates"),
        "expected actionable ambiguous-symbol error:\n{stderr}"
    );
}

#[test]
fn impact_empty_callers_includes_explanation() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // `helper` is called BY `caller`, but `extraHelper` in the extra file has
    // no callers — it's a leaf. Use it to trigger the empty-upstream hint.
    let out = Command::new(ecp_bin())
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

/// Contract: `--kind` disambiguates the START node (`--help`: "Disambiguate by
/// kind"); it never filters the traversal. The old version asserted every
/// descendant was a function and only held while `new Greeter()` emitted no
/// edge, so a class descendant is now expected, not a leak.
#[test]
fn impact_kind_selects_start_node_not_descendants() {
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
    let start_kind = filtered["impact"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["depth"].as_u64() == Some(0))
        .and_then(|e| e["kind"].as_str())
        .map(str::to_ascii_lowercase);
    assert_eq!(
        start_kind.as_deref(),
        Some("function"),
        "--kind function must pick the function start node: {filtered}"
    );
    assert_eq!(
        non_start_kinds(&filtered),
        baseline_kinds,
        "--kind must not filter descendants: {filtered}"
    );
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

// ── Class targets: instantiators reach the class through its constructor ────

/// `Widget` declares `__init__`, so `Widget(1)` lands on the constructor;
/// `Gadget` declares none, so `Gadget()` lands on the class. `Lonely` has no
/// instantiator, and because it follows `Widget` in the same file its
/// `HasMethod` edge points at `Widget.__init__`. The JS `Widget` is a
/// same-named free function with its own caller.
const PY_WIDGET: &str = "class Widget:
    def __init__(self, x):
        self.x = x


class Gadget:
    pass


class Lonely:
    def __init__(self):
        self.ready = True
";

const PY_APP: &str = "from .widget import Widget, Gadget


def make_widget():
    return Widget(1)


def make_gadget():
    return Gadget()
";

const JS_LEGACY: &str = "function Widget() {
  return 0;
}

function callLegacy() {
  return Widget();
}
";

fn init_python_class_repo(repo: &Path) {
    init_repo_with(
        repo,
        &[
            ("pkg/widget.py", PY_WIDGET),
            ("pkg/app.py", PY_APP),
            ("web/legacy.js", JS_LEGACY),
        ],
    );
}

/// `(name, kind)` of every entry past the start node.
fn reached(json: &Value) -> Vec<(String, String)> {
    json["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
        .map(|e| {
            (
                e["name"].as_str().unwrap_or_default().to_string(),
                e["kind"].as_str().unwrap_or_default().to_ascii_lowercase(),
            )
        })
        .collect()
}

fn reached_names(json: &Value) -> Vec<String> {
    reached(json).into_iter().map(|(name, _)| name).collect()
}

fn class_impact(repo: &Path, name: &str, kind: &str, direction: &str) -> Value {
    run_impact(
        repo,
        &[
            name,
            "--kind",
            kind,
            "--direction",
            direction,
            "--depth",
            "1",
        ],
    )
}

#[test]
fn test_impact_class_with_init_upstream_lists_instantiator() {
    let tmp = tempfile::tempdir().unwrap();
    init_python_class_repo(tmp.path());

    let up = class_impact(tmp.path(), "Widget", "class", "up");
    let hits = reached(&up);
    assert!(
        hits.iter().any(|(name, _)| name == "make_widget"),
        "the instantiator of Widget must be an upstream caller: {up}"
    );
    assert!(
        !hits.iter().any(|(name, _)| name == "callLegacy"),
        "a caller of the same-named JS function is not a Widget caller: {up}"
    );
    assert!(
        !hits.iter().any(|(_, kind)| kind == "constructor"),
        "the seeded constructor is part of the target, not a caller: {up}"
    );
}

#[test]
fn test_impact_class_without_constructor_upstream_lists_instantiator() {
    let tmp = tempfile::tempdir().unwrap();
    init_python_class_repo(tmp.path());

    let up = run_impact(tmp.path(), &["Gadget", "--direction", "up", "--depth", "1"]);
    assert!(
        reached_names(&up).iter().any(|n| n == "make_gadget"),
        "a class with no constructor takes the Calls edge itself: {up}"
    );
}

/// Empty: `Lonely.__init__` has no caller, and the `HasMethod` edge that
/// points at `Widget.__init__` must not borrow Widget's instantiator.
#[test]
fn test_impact_class_never_instantiated_upstream_reaches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    init_python_class_repo(tmp.path());

    let up = run_impact(tmp.path(), &["Lonely", "--direction", "up", "--depth", "1"]);
    assert!(reached(&up).is_empty(), "{up}");
}

#[test]
fn test_impact_class_downstream_does_not_seed_constructor_callers() {
    let tmp = tempfile::tempdir().unwrap();
    init_python_class_repo(tmp.path());

    let down = class_impact(tmp.path(), "Widget", "class", "down");
    assert!(
        !reached_names(&down).iter().any(|n| n == "make_widget"),
        "an instantiator is upstream, never downstream: {down}"
    );
}

#[test]
fn test_impact_class_both_directions_seeds_upstream_part() {
    let tmp = tempfile::tempdir().unwrap();
    init_python_class_repo(tmp.path());

    let both = class_impact(tmp.path(), "Widget", "class", "both");
    assert!(
        reached_names(&both).iter().any(|n| n == "make_widget"),
        "--direction both keeps the upstream instantiator: {both}"
    );
}

/// Java names the constructor like its class, so `--kind` picks the start;
/// either start lists the instantiator.
#[test]
fn test_impact_java_class_and_constructor_share_name_both_list_instantiator() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with(
        tmp.path(),
        &[
            (
                "src/Account.java",
                "public class Account {\n    public Account() {\n    }\n}\n",
            ),
            (
                "src/App.java",
                "public class App {\n    public Account makeAccount() {\n        return new Account();\n    }\n}\n",
            ),
        ],
    );

    for kind in ["class", "constructor"] {
        let up = class_impact(tmp.path(), "Account", kind, "up");
        assert!(
            reached_names(&up).iter().any(|n| n == "makeAccount"),
            "--kind {kind} must list the instantiator: {up}"
        );
    }
}
