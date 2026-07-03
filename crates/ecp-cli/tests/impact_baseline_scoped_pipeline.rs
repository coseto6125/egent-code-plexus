//! Behavior-equivalence guard for the scoped-pipeline optimization in
//! `impact_with_baseline` (FU-2026-07-04-08b838a64f1f).
//!
//! `impact --baseline` used to build the full 20-provider `make_pipeline()`
//! regardless of which languages the diff touched, paying ~0.65s of
//! tree-sitter `Query` compilation for a diff that might parse in ~8ms. The
//! fix scopes the pipeline to `provider_name_for_path` names derived from
//! `parsed_paths`. These tests assert the JSON payload is unchanged by that
//! swap — a diff still yields its `changed_symbols`, and a markdown file in
//! the diff behaves exactly as it did under the old full pipeline (which
//! could never route `.md` through `provider_name_for_path` either, so it
//! contributed zero symbols before and must still contribute zero now).

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_commit(repo: &Path, msg: &str) {
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
            msg,
        ],
    );
}

fn ecp_index(repo: &Path) {
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

fn run_impact_baseline(repo: &Path) -> Value {
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
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("impact --baseline failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "impact --baseline failed\nstderr={stderr}\nstdout={stdout}"
    );
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout:\n{stdout}\nstderr={stderr}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"))
}

const PY_V1: &str = r#"
def helper():
    return 1
"#;

const PY_V2: &str = r#"
def helper():
    return 2
"#;

/// A single-language (Python-only) diff must still produce `changed_symbols`
/// for `helper` — the scoped pipeline must register the `python` provider
/// derived from `parsed_paths`, not silently drop it.
#[test]
fn python_only_diff_still_yields_changed_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();

    run_git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".ecp/\n").unwrap();
    std::fs::write(repo.join("src/lib.py"), PY_V1).unwrap();
    git_commit(repo, "init");
    ecp_index(repo);

    std::fs::write(repo.join("src/lib.py"), PY_V2).unwrap();
    git_commit(repo, "tweak helper");
    ecp_index(repo);

    let val = run_impact_baseline(repo);
    let symbols = val["changed_symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("`changed_symbols` missing or not array:\n{val}"));
    assert!(
        symbols.iter().any(|s| s["name"].as_str() == Some("helper")),
        "python-only diff should report `helper` as changed; got {symbols:?}"
    );
}

/// A diff spanning two languages (Python + Go) must yield `changed_symbols`
/// for both — proves `needed` collects every touched provider, not just the
/// first file's.
#[test]
fn multi_language_diff_yields_symbols_for_every_touched_language() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();

    const GO_V1: &str = "package main\n\nfunc goHelper() int {\n\treturn 1\n}\n";
    const GO_V2: &str = "package main\n\nfunc goHelper() int {\n\treturn 2\n}\n";

    run_git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".ecp/\n").unwrap();
    std::fs::write(repo.join("src/lib.py"), PY_V1).unwrap();
    std::fs::write(repo.join("src/lib.go"), GO_V1).unwrap();
    git_commit(repo, "init");
    ecp_index(repo);

    std::fs::write(repo.join("src/lib.py"), PY_V2).unwrap();
    std::fs::write(repo.join("src/lib.go"), GO_V2).unwrap();
    git_commit(repo, "tweak both");
    ecp_index(repo);

    let val = run_impact_baseline(repo);
    let symbols = val["changed_symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("`changed_symbols` missing or not array:\n{val}"));
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains(&"helper"),
        "multi-language diff missing python `helper`; got {names:?}"
    );
    assert!(
        names.contains(&"goHelper"),
        "multi-language diff missing go `goHelper`; got {names:?}"
    );
}

/// Guards the markdown/yaml caveat: a markdown file changing alongside code
/// must still surface in `changed_paths` (unfiltered git-diff list), and
/// must NOT contribute to `changed_symbols` — matching the pre-fix full
/// pipeline, which also never dispatches `.md` through `provider_name_for_path`
/// (no extension entry exists for markdown; confirmed dead in `make_pipeline()`
/// too). If a future fix wires real markdown dispatch, this assertion is the
/// one to update alongside it.
#[test]
fn markdown_file_in_diff_reports_path_but_contributes_no_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    std::fs::create_dir_all(repo.join("src")).unwrap();

    run_git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".ecp/\n").unwrap();
    std::fs::write(repo.join("src/lib.py"), PY_V1).unwrap();
    std::fs::write(repo.join("NOTES.md"), "# Notes\n").unwrap();
    git_commit(repo, "init");
    ecp_index(repo);

    std::fs::write(repo.join("src/lib.py"), PY_V2).unwrap();
    std::fs::write(repo.join("NOTES.md"), "# Notes\n\nMore detail.\n").unwrap();
    git_commit(repo, "tweak helper + notes");
    ecp_index(repo);

    let val = run_impact_baseline(repo);

    let paths = val["changed_paths"]
        .as_array()
        .unwrap_or_else(|| panic!("`changed_paths` missing:\n{val}"));
    let path_strs: Vec<&str> = paths.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        path_strs.contains(&"NOTES.md"),
        "NOTES.md should still be reported in changed_paths; got {path_strs:?}"
    );

    let symbols = val["changed_symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("`changed_symbols` missing:\n{val}"));
    assert!(
        symbols.iter().any(|s| s["name"].as_str() == Some("helper")),
        "python helper should still be reported changed; got {symbols:?}"
    );
    assert!(
        symbols
            .iter()
            .all(|s| s["filePath"].as_str() != Some("NOTES.md")),
        "markdown must not contribute changed_symbols (matches pre-fix dead-dispatch behavior); got {symbols:?}"
    );
}
