//! `ecp rename` names every affected file it could not scan.
//!
//! A `.vue` file has no identifier finder (C5 `Capability::IdentifierFinder`).
//! Before the finder read the route, such a file returned an empty result and
//! the rename dropped it without a word, so an agent read "renamed" while the
//! component still called the old name.
//!
//! Setup follows `rename_excludes_heuristic.rs`: index a real repo, then
//! replace `graph.bin` with a fixture whose `Calls` edge runs from the `.vue`
//! file to the target. The Vue parser emits no calls (C5 open point 13), so
//! only a fixture puts that file in the rename's file set.

mod common;

use common::{ecp_bin, init_and_analyze, write};
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NO_FINDER: &str = "no identifier finder for this file type";
const UTIL_TS: &str = "export function helper() {\n  return 1;\n}\n";
const APP_VUE: &str = "<template><div /></template>\n<script lang=\"ts\">\nimport { helper } from './util';\nexport function setup() {\n  return helper();\n}\n</script>\n";

fn find_graph_bin(repo: &Path) -> PathBuf {
    let mut queue = vec![repo.join(".ecp")];
    while let Some(dir) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
            if path.file_name().is_some_and(|n| n == "graph.bin") {
                return path;
            }
            if path.is_dir() {
                queue.push(path);
            }
        }
    }
    panic!("graph.bin not found under {}", repo.join(".ecp").display())
}

/// `src/App.vue::setup` calls `src/util.ts::helper`, when `vue_calls` is set.
/// Without it, `helper` lives only in `src/App.vue`.
fn inject_graph(repo: &Path, vue_calls: bool) {
    let mut fx = GraphFixture::new();
    if vue_calls {
        let helper = fx.func("src/util.ts", "helper");
        fx.span(helper, (0, 0, 2, 1));
        let setup = fx.func("src/App.vue", "setup");
        fx.span(setup, (3, 0, 5, 1));
        fx.edge(setup, helper, RelType::Calls);
    } else {
        let helper = fx.func("src/App.vue", "helper");
        fx.span(helper, (3, 0, 5, 1));
    }
    std::fs::write(find_graph_bin(repo), fx.into_bytes()).unwrap();
}

fn setup_repo(app_vue: &str, vue_calls: bool) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    write(repo.path(), "src/util.ts", UTIL_TS);
    write(repo.path(), "src/App.vue", app_vue);
    init_and_analyze(repo.path());
    inject_graph(repo.path(), vue_calls);
    repo
}

fn rename(repo: &Path, extra: &[&str]) -> Output {
    Command::new(ecp_bin())
        .args([
            "rename",
            "helper",
            "assist",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .args(extra)
        .env("HOME", repo)
        .current_dir(repo)
        .output()
        .expect("ecp rename spawn failed")
}

/// The notice row: one stdout line that names the file and the reason, so a
/// residual line that merely names `src/App.vue` does not satisfy it.
fn has_notice(stdout: &str, file: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.contains(file) && line.contains(NO_FINDER))
}

#[test]
fn test_rename_vue_component_reports_unsupported() {
    let repo = setup_repo(APP_VUE, true);
    let out = rename(repo.path(), &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("not_scanned"),
        "payload must carry not_scanned;\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        has_notice(&stdout, "src/App.vue"),
        "App.vue must be named as not scanned;\nstdout={stdout}"
    );
    // The scanned file is planned, not reported.
    assert!(
        !has_notice(&stdout, "src/util.ts"),
        "util.ts has a finder;\nstdout={stdout}"
    );
    // Contract: the headline never claims `risk safe` while a caller file was
    // not scanned (ruling D4 option A); K counts the unscanned files.
    assert!(
        stdout.contains("risk partial; files 1; usages 1; not scanned 1"),
        "only util.ts is planned and the headline counts App.vue;\nstdout={stdout}"
    );
    assert!(!stdout.contains("risk safe"), "stdout={stdout}");
}

#[test]
fn test_rename_vue_component_apply_leaves_vue_and_reports_it() {
    let repo = setup_repo(APP_VUE, true);
    let out = rename(repo.path(), &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let util = std::fs::read_to_string(repo.path().join("src/util.ts")).unwrap();
    let vue = std::fs::read_to_string(repo.path().join("src/App.vue")).unwrap();
    assert!(util.contains("function assist()"), "util.ts={util}");
    assert_eq!(vue, APP_VUE, "the unscanned file must stay untouched");
    assert!(has_notice(&stdout, "src/App.vue"), "stdout={stdout}");
}

#[test]
fn test_rename_only_vue_file_zero_hits_still_reports_unsupported() {
    // With no scannable file, the answer is "No occurrences", and the payload
    // must still say why: the one affected file was never scanned.
    let app = "<script>\nexport function helper() {\n  return 1;\n}\n</script>\n";
    let repo = setup_repo(app, false);
    let out = rename(repo.path(), &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Contract: a zero-hit answer says it covers scanned files only (D4 A).
    assert!(
        stdout.contains(
            "No occurrences of \"helper\" found in scanned files; 1 affected file(s) not scanned."
        ),
        "stdout={stdout}"
    );
    assert!(has_notice(&stdout, "src/App.vue"), "stdout={stdout}");
}

#[test]
fn test_rename_missing_affected_file_reports_cannot_read() {
    // The fixture graph names a caller file that is absent from disk.
    let repo = tempfile::tempdir().expect("tempdir");
    write(repo.path(), "src/util.ts", UTIL_TS);
    init_and_analyze(repo.path());
    let mut fx = GraphFixture::new();
    let helper = fx.func("src/util.ts", "helper");
    fx.span(helper, (0, 0, 2, 1));
    let gone = fx.func("src/gone.ts", "caller");
    fx.span(gone, (0, 0, 2, 1));
    fx.edge(gone, helper, RelType::Calls);
    std::fs::write(find_graph_bin(repo.path()), fx.into_bytes()).unwrap();
    let out = rename(repo.path(), &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("not_scanned"), "stdout={stdout}");
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("src/gone.ts") && l.contains("cannot read file")),
        "stdout={stdout}"
    );
}

#[test]
fn test_rename_all_files_scanned_omits_not_scanned() {
    // The field's absence is the "everything was scanned" signal.
    let repo = tempfile::tempdir().expect("tempdir");
    write(repo.path(), "src/util.ts", UTIL_TS);
    init_and_analyze(repo.path());
    let mut fx = GraphFixture::new();
    let helper = fx.func("src/util.ts", "helper");
    fx.span(helper, (0, 0, 2, 1));
    std::fs::write(find_graph_bin(repo.path()), fx.into_bytes()).unwrap();
    let out = rename(repo.path(), &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("risk safe; files 1; usages 1"),
        "stdout={stdout}"
    );
    assert!(!stdout.contains("not_scanned"), "stdout={stdout}");
}
