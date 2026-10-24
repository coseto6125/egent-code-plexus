//! `ecp impact --file` picks the path that equals the needle (or ends with
//! `/needle`) over a longer path that merely contains it, and an ambiguity
//! error names only the narrowing flags the caller left out.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn init_repo(repo: &Path, files: &[(&str, &str)]) {
    git(repo, &["init", "-q", "-b", "main"]);
    for (rel, src) in files {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "init"]);
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn impact(repo: &Path, extra: &[&str]) -> Output {
    Command::new(ecp_bin())
        .args(["impact", "--repo", ".", "--format", "json"])
        .args(extra)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("impact failed to spawn")
}

fn target_file(repo: &Path, extra: &[&str]) -> String {
    let out = impact(repo, extra);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{extra:?} failed: stderr={}\nstdout={stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_str(&stdout[stdout.find('{').unwrap()..]).unwrap();
    let start = json["impact"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["depth"].as_u64() == Some(0))
        .unwrap_or_else(|| panic!("no depth-0 row: {json}"));
    start["filePath"].as_str().unwrap().to_string()
}

#[test]
fn test_impact_file_flag_naming_shorter_nested_path_picks_it_not_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(
        tmp.path(),
        &[
            ("cmd/main.go", "package main\n\nfunc main() {}\n"),
            ("tools/cmd/main.go", "package main\n\nfunc main() {}\n"),
            (
                "src/index.ts",
                "export function render(): number { return 1; }\n",
            ),
            (
                "packages/a/src/index.ts",
                "export function render(): number { return 2; }\n",
            ),
        ],
    );
    for (name, file, kind, want) in [
        ("main", "cmd/main.go", "function", "cmd/main.go"),
        ("main", "./cmd/main.go", "function", "cmd/main.go"),
        ("main", "tools/cmd/main.go", "function", "tools/cmd/main.go"),
        ("render", "src/index.ts", "function", "src/index.ts"),
        (
            "render",
            "packages/a/src/index.ts",
            "function",
            "packages/a/src/index.ts",
        ),
    ] {
        assert_eq!(
            target_file(tmp.path(), &[name, "--file", file, "--kind", kind]),
            want,
            "{name} --file {file}"
        );
    }
}

#[test]
fn test_impact_ambiguous_with_file_and_kind_given_does_not_suggest_them_again() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(
        tmp.path(),
        &[
            ("a/x.ts", "export function dup(): number { return 1; }\n"),
            ("b/x.ts", "export function dup(): number { return 2; }\n"),
        ],
    );
    let both = impact(tmp.path(), &["dup", "--file", "x.ts", "--kind", "function"]);
    let stderr = String::from_utf8_lossy(&both.stderr);
    assert!(!both.status.success(), "{stderr}");
    assert!(
        stderr.contains("ambiguous")
            && stderr.contains("a/x.ts")
            && stderr.contains("b/x.ts")
            && !stderr.contains("add --file")
            && !stderr.contains("add --kind")
            && !stderr.contains("or --kind"),
        "{stderr}"
    );

    let file_only = impact(tmp.path(), &["dup", "--file", "x.ts"]);
    let stderr = String::from_utf8_lossy(&file_only.stderr);
    assert!(
        stderr.contains("add --kind") && !stderr.contains("add --file"),
        "{stderr}"
    );
}
