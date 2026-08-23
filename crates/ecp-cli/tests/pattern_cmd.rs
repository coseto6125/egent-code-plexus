//! Integration test for `ecp pattern`.
//!
//! Pipeline coverage:
//! - Stage 1 (graph): every indexed file by default, one symbol's callers
//!   under `--callers-of`
//! - Stage 2 (AST): tree-sitter parse + ast-grep pattern match
//! - Metavariable capture across a multi-line block
//! - A pattern every supported language rejects is an error carrying the
//!   parser's diagnostic, while a scan that meets no supported language at
//!   all is an ordinary empty result
//! - The scan mutates nothing

mod common;

use common::run_git;
use std::path::Path;
use std::process::Command;

const SWALLOWED_EXCEPT: &str = "try:\n    $$$BODY\nexcept $$$E:\n    pass";

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn build_index(root: &Path) {
    let home = root.join(".home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", root.to_str().unwrap()])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("ecp admin index failed to spawn");
    assert!(
        out.status.success(),
        "ecp admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Three Python files, each swallowing one exception. Only `pattern_def` and
/// `pattern_user` are connected through `load_config`, so `--callers-of`
/// must drop `pattern_unrelated`.
fn setup_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();
    for (name, body) in [
        ("pattern_def.py", include_str!("fixtures/pattern_def.py")),
        ("pattern_user.py", include_str!("fixtures/pattern_user.py")),
        (
            "pattern_unrelated.py",
            include_str!("fixtures/pattern_unrelated.py"),
        ),
    ] {
        std::fs::write(root.join(name), body).unwrap();
    }
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);
    repo
}

fn run_pattern(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ecp_bin())
        .arg("pattern")
        .args(args)
        .args(["--format", "json"])
        .env("HOME", root.join(".home"))
        .current_dir(root)
        .output()
        .expect("ecp pattern failed to spawn")
}

fn payload(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "ecp pattern failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json payload")
}

#[test]
fn pattern_multiline_metavar_matches_every_indexed_file() {
    let repo = setup_repo();
    let out = run_pattern(repo.path(), &["-p", SWALLOWED_EXCEPT, "--lang", "py"]);
    let v = payload(&out);
    assert_eq!(v["total"], 3, "{v}");
    assert_eq!(v["matches"].as_array().unwrap().len(), 3);
}

#[test]
fn pattern_callers_of_drops_unconnected_files() {
    let repo = setup_repo();
    let all = payload(&run_pattern(
        repo.path(),
        &["-p", SWALLOWED_EXCEPT, "--lang", "py"],
    ));
    let scoped = payload(&run_pattern(
        repo.path(),
        &[
            "-p",
            SWALLOWED_EXCEPT,
            "--lang",
            "py",
            "--callers-of",
            "load_config",
        ],
    ));

    assert_eq!(scoped["total"], 2, "{scoped}");
    let files: Vec<&str> = scoped["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["file"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["pattern_def.py", "pattern_user.py"]);
    assert!(!files.contains(&"pattern_unrelated.py"), "{files:?}");
    assert!(
        scoped["scanned_files"].as_u64() < all["scanned_files"].as_u64(),
        "scoping must narrow the file set: {scoped} vs {all}"
    );
}

#[test]
fn pattern_callers_of_excludes_override_edges() {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();
    std::fs::write(root.join("Base.java"), "class Base { void foo() {} }\n").unwrap();
    std::fs::write(
        root.join("Child.java"),
        "class Child extends Base { @Override void foo() { int marker = 7; } }\n",
    )
    .unwrap();
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    let v = payload(&run_pattern(
        root,
        &[
            "-p",
            "int marker = $X;",
            "--lang",
            "java",
            "--callers-of",
            "Base.foo",
        ],
    ));
    assert_eq!(v["scanned_files"], 1, "{v}");
    assert_eq!(v["total"], 0, "{v}");
}

#[test]
fn pattern_lang_counts_only_files_actually_scanned() {
    let repo = setup_repo();
    let root = repo.path();
    std::fs::write(root.join("unrelated.rs"), "fn unrelated() {}\n").unwrap();
    run_git(root, &["add", "unrelated.rs"]);
    run_git(root, &["commit", "-q", "-m", "add rust file"]);
    build_index(root);

    let v = payload(&run_pattern(root, &["-p", "pass", "--lang", "py"]));
    assert_eq!(v["scanned_files"], 3, "{v}");
}

#[test]
fn pattern_single_metavar_binds_call_argument() {
    let repo = setup_repo();
    let v = payload(&run_pattern(
        repo.path(),
        &["-p", "json.loads($X)", "--lang", "py"],
    ));
    assert_eq!(v["total"], 1, "{v}");
    assert_eq!(v["matches"][0]["file"], "pattern_def.py");
}

#[test]
fn pattern_unparseable_pattern_reports_parser_diagnostic() {
    let repo = setup_repo();
    // Two sibling statements cannot form one pattern node.
    let out = run_pattern(repo.path(), &["-p", "a = 1\nb = 2", "--lang", "py"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not parse"), "stderr={stderr}");
    // The parser's own reason, not just "it failed somewhere".
    assert!(stderr.contains("Multiple AST nodes"), "stderr={stderr}");
}

/// A repo whose indexed files are all unsupported must not be reported as a
/// pattern-syntax failure: nothing ever tried to parse the pattern.
#[test]
fn pattern_repo_without_supported_language_returns_empty_result() {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();
    std::fs::write(root.join("README.md"), "# docs\n").unwrap();
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    let v = payload(&run_pattern(root, &["-p", "hello"]));
    assert_eq!(v["total"], 0, "{v}");
    assert!(v["languages"].as_array().unwrap().is_empty(), "{v}");
}

/// `--lang` compiles the pattern up front, so a pattern that language rejects
/// is an error even when the scan would have met no file of that language.
/// Leaving it to the per-extension cache would never parse the pattern at all.
#[test]
fn pattern_lang_flag_rejects_bad_pattern_with_no_file_of_that_language() {
    let repo = setup_repo();
    // The fixture repo is Python-only; `--lang js` matches no candidate.
    let out = run_pattern(repo.path(), &["-p", "a = 1\nb = 2", "--lang", "js"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not parse as js"), "stderr={stderr}");
}

/// The same scan with a pattern that language accepts is an empty result, not
/// an error: the pattern is fine, there is simply no file to match it against.
#[test]
fn pattern_lang_flag_with_no_file_of_that_language_is_empty_not_an_error() {
    let repo = setup_repo();
    let v = payload(&run_pattern(repo.path(), &["-p", "f($X)", "--lang", "js"]));
    assert_eq!(v["total"], 0, "{v}");
}

/// A `--callers-of` symbol the graph does not carry is reported, the way
/// `ecp impact` and `ecp inspect` report it. `caller_files` seeds the target's
/// own file, so a symbol that exists always yields at least one candidate.
#[test]
fn pattern_callers_of_unknown_symbol_reports_the_miss() {
    let repo = setup_repo();
    let out = run_pattern(
        repo.path(),
        &["-p", "pass", "--callers-of", "no_such_symbol"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no symbol named"), "stderr={stderr}");
    assert!(stderr.contains("--mode fuzzy"), "stderr={stderr}");
}

/// A symbol that exists but nobody calls still scans its own file, so the
/// miss report above cannot swallow it.
#[test]
fn pattern_callers_of_uncalled_symbol_scans_its_own_file() {
    let repo = setup_repo();
    let v = payload(&run_pattern(
        repo.path(),
        &[
            "-p",
            "pass",
            "--lang",
            "py",
            "--callers-of",
            "read_settings",
        ],
    ));
    assert_eq!(v["scanned_files"], 1, "{v}");
}

#[test]
fn pattern_unsupported_lang_flag_is_rejected() {
    let repo = setup_repo();
    let out = run_pattern(repo.path(), &["-p", "hello", "--lang", "md"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no pattern support"), "stderr={stderr}");
}

#[test]
fn pattern_leaves_every_file_untouched() {
    let repo = setup_repo();
    let before = std::fs::read_to_string(repo.path().join("pattern_def.py")).unwrap();
    run_pattern(repo.path(), &["-p", SWALLOWED_EXCEPT, "--lang", "py"]);
    let after = std::fs::read_to_string(repo.path().join("pattern_def.py")).unwrap();
    assert_eq!(before, after);
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let dirty = String::from_utf8_lossy(&status.stdout);
    assert!(
        dirty.lines().all(|l| l.contains(".home")),
        "scan dirtied the tree: {dirty}"
    );
}
