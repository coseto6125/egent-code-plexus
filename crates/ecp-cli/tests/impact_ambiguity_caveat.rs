//! FU-2026-05-29-011: `ecp impact` on a high-collision target must self-flag
//! that its caller set may be incomplete. Same-named definitions mean bare
//! calls (no import/qualifier context) were ambiguity-suppressed at index
//! time (Tier-3 `AmbiguousGlobal`) — the upstream BFS can't see them, and a
//! silent low caller count reads as "safe to refactor" when it isn't.

mod common;

use common::{ecp_bin, init_and_analyze, run_git, write};
use std::path::Path;
use std::process::Command;

/// Two same-named `process` defs in different files, a bare caller that the
/// Tier-3 defence suppresses, and one collision-free symbol as control.
fn setup_collision_repo(repo: &Path, home: &Path) {
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/alpha.py"), "def process():\n    return 1\n").unwrap();
    std::fs::write(repo.join("src/beta.py"), "def process():\n    return 2\n").unwrap();
    std::fs::write(
        repo.join("src/caller.py"),
        "def run_all():\n    process()\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/unique.py"),
        "def unique_entry():\n    return 3\n\ndef call_unique():\n    unique_entry()\n",
    )
    .unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/ambiguity-test.git",
        ],
    );
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_impact(repo: &Path, home: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["impact"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--repo", ".", "--format", "json"]);
    let out = Command::new(ecp_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("impact failed to spawn");
    assert!(
        out.status.success(),
        "impact {extra:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn impact_on_collision_name_flags_incomplete_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    // --file narrows past the ambiguous-target error; the graph still holds
    // 2 same-named defs, so the upstream caller set must self-flag.
    let json = run_impact(
        repo,
        home_tmp.path(),
        &["process", "--file", "src/alpha.py", "--direction", "up"],
    );
    let caveat = json
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("collision target must carry a `result` caveat: {json}"));
    assert!(
        caveat.contains("incomplete"),
        "caveat must say the caller set may be incomplete: {caveat}"
    );
    assert!(
        caveat.contains("2 same-named"),
        "caveat must count the same-named definitions: {caveat}"
    );
    // The contract: the caveat names a command the agent can run as-is,
    // carrying the --file that made the colliding name resolvable.
    assert!(
        caveat.contains("ecp impact --target process --file src/alpha.py --ambiguous-callers"),
        "caveat must name the runnable command: {caveat}"
    );
    assert!(
        json.get("ambiguous_callers").is_none(),
        "without the flag the payload must not carry the sites: {json}"
    );
}

#[test]
fn test_ambiguous_callers_json_format_parses_and_lists_suppressed_bare_call() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    // run_impact parses stdout as JSON, so a parse failure fails this test.
    let json = run_impact(
        repo,
        home_tmp.path(),
        &["process", "--file", "src/alpha.py", "--ambiguous-callers"],
    );
    let field = &json["ambiguous_callers"];
    let sites = field["sites"]
        .as_array()
        .unwrap_or_else(|| panic!("ambiguous_callers.sites expected: {json}"));
    assert!(
        sites.iter().any(|s| s["file"] == "src/caller.py"
            && s["line"] == 2
            && s["enclosing"] == "run_all"
            && s["form"] == "bare"),
        "the suppressed bare call in run_all must be listed: {json}"
    );
    assert!(
        sites
            .iter()
            .all(|s| s["file"] != "src/alpha.py" && s["file"] != "src/beta.py"),
        "definition lines must be dropped: {json}"
    );
    assert_eq!(field["shown"].as_u64(), Some(sites.len() as u64));
    let caveat = json["result"].as_str().unwrap_or_default();
    assert!(
        caveat.contains("`ambiguous_callers` field"),
        "with the flag set the caveat points at the field: {caveat}"
    );
}

#[test]
fn test_ambiguous_callers_batch_targets_each_get_own_field() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    let mut child = Command::new(ecp_bin())
        .args([
            "impact",
            "--batch",
            "--kind",
            "function",
            "--ambiguous-callers",
            "--repo",
            ".",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", home_tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("impact --batch failed to spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"process\nunique_entry\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let blocks: Vec<(String, serde_json::Value)> = stdout
        .split("=== target: ")
        .filter(|b| !b.trim().is_empty())
        .map(|b| {
            let (name, body) = b.split_once(" ===\n").expect("block header");
            (name.to_string(), serde_json::from_str(body.trim()).unwrap())
        })
        .collect();
    assert_eq!(blocks.len(), 2, "one block per target: {stdout}");
    // Batch lines carry no per-target --file, so a name with two definitions
    // is refused with its candidates instead of merging two blast radii.
    assert_eq!(blocks[0].1["status"], "ambiguous", "{stdout}");
    let error = blocks[0].1["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("src/alpha.py") && error.contains("src/beta.py"),
        "the refusal lists both candidates: {stdout}"
    );
    assert!(
        blocks[1].1.get("ambiguous_callers").is_none(),
        "collision-free target carries no field: {stdout}"
    );
}

#[test]
fn impact_on_unique_name_stays_caveat_free() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    let json = run_impact(
        repo,
        home_tmp.path(),
        &["unique_entry", "--direction", "up"],
    );
    assert!(
        json.get("result").is_none(),
        "collision-free target must not pay the caveat token cost: {json}"
    );
}

#[test]
fn downstream_impact_on_collision_name_stays_caveat_free() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    // The suppressed edges are *incoming* bare calls; a downstream (callee)
    // walk of the target is unaffected, so no caveat.
    let json = run_impact(
        repo,
        home_tmp.path(),
        &["process", "--file", "src/alpha.py", "--direction", "down"],
    );
    assert!(
        json.get("result").is_none(),
        "downstream walk must not carry the caller-set caveat: {json}"
    );
}

/// cli F4: a narrowing flag that still leaves two definitions must refuse with
/// the candidates. Merging the two blast radii reads as one symbol's callers.
#[test]
fn test_impact_by_name_kind_leaving_two_matches_refuses_with_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    let out = Command::new(ecp_bin())
        .args([
            "impact", "process", "--kind", "function", "--repo", ".", "--format", "json",
        ])
        .current_dir(repo)
        .env("HOME", home_tmp.path())
        .output()
        .expect("impact failed to spawn");
    assert!(!out.status.success(), "two matches must not resolve");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ambiguous")
            && stderr.contains("src/alpha.py")
            && stderr.contains("src/beta.py"),
        "the refusal lists both candidates: {stderr}"
    );
}

fn caveat_of(json: &serde_json::Value) -> &str {
    json.get("result").and_then(|v| v.as_str()).unwrap_or("")
}

/// A same-named class and function in two files: the function target weighs
/// callables only, so the class is no competitor; the class target weighs the
/// function a type name can construct, so it keeps the caveat. Before the fix
/// both targets carried "2 same-named definitions".
#[test]
fn test_impact_caveat_counts_only_competing_kinds_ts_class_and_function() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(repo, "q1.ts", "export class Q {}\n");
    write(
        repo,
        "q2.ts",
        "export function Q() { return 1; }\nexport function callq() { return Q(); }\n",
    );
    init_and_analyze(repo);

    let function = run_impact(
        repo,
        repo,
        &["Q", "--kind", "function", "--direction", "up"],
    );
    assert!(
        !caveat_of(&function).contains("same-named"),
        "a function target is not competed by a class: {function}"
    );

    let class = run_impact(repo, repo, &["Q", "--kind", "class", "--direction", "up"]);
    assert!(
        caveat_of(&class).contains("2 same-named"),
        "a class target is still competed by the function Q: {class}"
    );
}

/// A Rust struct and its `impl` block share a name; an `Impl` is no
/// declaration a type lookup binds. Before the fix: "2 same-named".
#[test]
fn test_impact_caveat_struct_and_impl_block_do_not_compete_rust() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "src/lib.rs",
        "pub struct DirtySet { n: u32 }\nimpl DirtySet { pub fn len(&self) -> u32 { self.n } }\n",
    );
    init_and_analyze(repo);

    let json = run_impact(
        repo,
        repo,
        &["DirtySet", "--kind", "struct", "--direction", "up"],
    );
    assert!(
        !caveat_of(&json).contains("same-named"),
        "struct + impl is one type, not a collision: {json}"
    );
}

/// Java names a constructor after its class: the class and its own
/// constructor are one type, not two definitions a call could pick between.
/// Before the fix the class target counted its constructor: "2 same-named".
#[test]
fn test_impact_caveat_class_and_own_constructor_do_not_compete_java() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(repo, "Foo.java", "class Foo {\n    Foo() {}\n}\n");
    write(
        repo,
        "Main.java",
        "class Main {\n    void run() {\n        new Foo();\n    }\n}\n",
    );
    init_and_analyze(repo);

    let json = run_impact(repo, repo, &["Foo", "--kind", "class", "--direction", "up"]);
    assert!(
        !caveat_of(&json).contains("same-named"),
        "a class and its own constructor are not a collision: {json}"
    );
}

/// Python `def Q` beside `class Q(Base):`. The function target weighs
/// callables only, so it carries no `ambiguous_callers`; the class target is
/// competed by the function, and the class declaration line `Q(Base)` is a
/// definition, never a call site.
#[test]
fn test_ambiguous_callers_function_and_class_namesake_python() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "q1.py",
        "class Base:\n    pass\n\n\nclass Q(Base):\n    pass\n",
    );
    write(repo, "q2.py", "def Q():\n    return 1\n");
    write(
        repo,
        "q3.py",
        "from q2 import *\n\n\ndef callq():\n    return Q()\n",
    );
    init_and_analyze(repo);

    let function = run_impact(
        repo,
        repo,
        &[
            "Q",
            "--kind",
            "function",
            "--direction",
            "up",
            "--ambiguous-callers",
        ],
    );
    assert!(
        function.get("ambiguous_callers").is_none(),
        "a function target is not competed by a class: {function}"
    );

    let class = run_impact(
        repo,
        repo,
        &[
            "Q",
            "--kind",
            "class",
            "--direction",
            "up",
            "--ambiguous-callers",
        ],
    );
    let sites = class["ambiguous_callers"]["sites"]
        .as_array()
        .unwrap_or_else(|| panic!("a class target competed by `def Q` lists sites: {class}"));
    assert!(
        sites.iter().all(|s| s["file"] != "q1.py"),
        "the class declaration line is no call site: {class}"
    );
}
