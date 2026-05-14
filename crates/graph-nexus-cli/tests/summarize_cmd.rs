use std::process::Command;

const SOURCE: &str = r#"
function handleLogin(username: string, password: string) {
    const user = lookupUser(username);
    if (!verifyPassword(user, password)) return null;
    return createSession(user);
}

function lookupUser(name: string) {
    return dbQuery(name);
}

function verifyPassword(user: any, password: string) {
    return hashPassword(password) === user.passwordHash;
}

function hashPassword(password: string) {
    return password;
}

function createSession(user: any) {
    return { id: generateSessionId(), user };
}

function generateSessionId() {
    return Math.random().toString(36);
}

function dbQuery(q: string) {
    return { name: q, passwordHash: "" };
}
"#;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn init_repo_and_analyze(repo: &std::path::Path) {
    let _ = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/auth.ts"), SOURCE).unwrap();
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

fn run_summarize(repo: &std::path::Path, extra: &[&str]) -> String {
    let mut args = vec!["summarize"];
    args.extend_from_slice(extra);
    let out = Command::new(gnx_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("summarize failed to spawn");
    assert!(
        out.status.success(),
        "summarize failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("non-utf8 output")
}

#[test]
fn summarize_markdown_has_three_sections() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let out = run_summarize(tmp.path(), &[]);

    assert!(
        out.starts_with("# Project Summary"),
        "missing header: {out}"
    );
    assert!(
        out.contains("## Top hot files"),
        "missing top files section"
    );
    assert!(
        out.contains("## Architecture"),
        "missing architecture section"
    );
    assert!(
        out.contains("## Per-file detail"),
        "missing per-file section"
    );
    // 7 functions in 1 file → expect `src/auth.ts` in detail.
    assert!(
        out.contains("src/auth.ts"),
        "expected source file mentioned"
    );
}

#[test]
fn summarize_json_format_parses() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let out = run_summarize(tmp.path(), &["--format", "json"]);

    let v: serde_json::Value = serde_json::from_str(&out).expect("output must be valid JSON");
    assert!(v["files_total"].as_u64().unwrap() >= 1);
    assert!(v["symbols_total"].as_u64().unwrap() >= 5);
    assert!(v["top_files"].is_array());
    assert!(v["top_communities"].is_array());
}

#[test]
fn summarize_unsupported_format_errors() {
    // clap ValueEnum 在 argument parse 階段就拒絕無效 --format，repo 不需 analyze。
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(gnx_bin())
        .args(["summarize", "--format", "toml"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("spawn failed");
    assert!(!out.status.success(), "expected nonzero exit on bad format");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("invalid value") && err.contains("--format"),
        "stderr should mention invalid --format value: {err}"
    );
    // 應該明確列出可選值
    assert!(
        err.contains("md") && err.contains("json"),
        "stderr should list valid formats md/json: {err}"
    );
}

// 兩檔皆呼叫對方的 `format` (跨檔 call) 確保 in_deg>0、不被 orphan filter 濾掉。
const SHARED_NAME_SOURCE_A: &str = r#"
import { format as fmtB } from "./b";
export function format(x: number) { return x.toString(); }
export function entryA() { return format(1) + fmtB("x"); }
"#;

const SHARED_NAME_SOURCE_B: &str = r#"
import { format as fmtA } from "./a";
export function format(s: string) { return s.toUpperCase(); }
export function entryB() { return format("y") + fmtA(2); }
"#;

fn init_repo_with_shared_names(repo: &std::path::Path) {
    let _ = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.ts"), SHARED_NAME_SOURCE_A).unwrap();
    std::fs::write(repo.join("src/b.ts"), SHARED_NAME_SOURCE_B).unwrap();
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
        .expect("analyze failed");
    assert!(out.status.success());
}

#[test]
fn summarize_detects_shadowing_for_duplicate_symbol_names() {
    // src/a.ts 與 src/b.ts 各定義 `format` → 應觸發 shadowed_by 標註
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_shared_names(tmp.path());
    let out = run_summarize(tmp.path(), &["--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("must be JSON");

    let mut format_entries = Vec::new();
    for file in v["top_files"].as_array().unwrap() {
        for sym in file["top_symbols"].as_array().unwrap() {
            if sym["name"] == "format" {
                format_entries.push(sym["shadowed_by"].as_u64().unwrap());
            }
        }
    }
    assert!(
        !format_entries.is_empty(),
        "expected `format` symbol in top_symbols"
    );
    for shadowed_by in format_entries {
        assert_eq!(
            shadowed_by, 1,
            "`format` exists in 2 files → shadowed_by should be 1"
        );
    }
}

#[test]
fn summarize_include_orphans_flag_keeps_zero_degree_symbols() {
    // 預設過濾 in_deg=0 && out_deg=0 孤兒；--include-orphans 應保留。
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let default_json = run_summarize(tmp.path(), &["--format", "json"]);
    let with_orphans = run_summarize(tmp.path(), &["--format", "json", "--include-orphans"]);

    fn count_total_top_symbols(s: &str) -> usize {
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        v["top_files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["top_symbols"].as_array().unwrap().len())
            .sum()
    }
    let n_default = count_total_top_symbols(&default_json);
    let n_orphans = count_total_top_symbols(&with_orphans);
    assert!(
        n_orphans >= n_default,
        "--include-orphans should not drop symbols (default={n_default}, with_orphans={n_orphans})"
    );
}

#[test]
fn summarize_writes_output_file() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let out_path = tmp.path().join("summary.md");
    let out = run_summarize(tmp.path(), &["--output", out_path.to_str().unwrap()]);
    assert!(out.is_empty(), "stdout should be empty when --output used");
    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(content.contains("# Project Summary"));
}

// Python sources that emit blind spots (eval/exec/dynamic-import/cross-getattr).
// Distribution chosen so the per-kind table has >1 row and at least 2 distinct
// files contribute, exercising the top-files breakdown.
const BLIND_SPOTS_MIDDLEWARE: &str = r#"
def authenticate(user_input):
    return eval(user_input)

def authorize(code):
    exec(code)

def reauthenticate(more):
    return eval(more)
"#;

const BLIND_SPOTS_DISPATCH: &str = r#"
import importlib

def dispatch(name):
    return importlib.import_module(name)

class Router:
    def call(self, other, name):
        return getattr(other, name)()
"#;

fn init_repo_with_blind_spots(repo: &std::path::Path) {
    let _ = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/middleware.py"), BLIND_SPOTS_MIDDLEWARE).unwrap();
    std::fs::write(repo.join("src/dispatch.py"), BLIND_SPOTS_DISPATCH).unwrap();
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
        .expect("analyze failed");
    assert!(
        out.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn summarize_blind_spots_groups_by_kind_with_top_files() {
    // Repo has 3 eval, 1 exec, 1 dynamic-import, 1 cross-getattr ⇒ 6 sites
    // across 2 files. Markdown section should match the spec format:
    // a `Total: N sites across M files.` line, a `By kind:` bullet list,
    // and a numbered `Top files:` list with descending counts.
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_blind_spots(tmp.path());
    let out = run_summarize(tmp.path(), &[]);

    assert!(
        out.contains("## Blind Spots"),
        "expected Blind Spots heading in markdown: {out}"
    );
    // Spec-shaped Total line. middleware (3 eval) + dispatch (1 exec or
    // dispatch-level: import + getattr) → ≥4 sites across 2 files. We assert
    // the "across N files." suffix shape without pinning a brittle count.
    assert!(
        out.contains("Total: ") && out.contains(" across "),
        "expected `Total: N sites across M files.` line: {out}"
    );
    assert!(
        out.contains("By kind:"),
        "expected `By kind:` subsection header: {out}"
    );
    // kind entries are formatted as ``- `kind`: count``
    assert!(
        out.contains("- `python-eval`:"),
        "expected python-eval kind bullet: {out}"
    );
    assert!(
        out.contains("Top files:"),
        "expected `Top files:` subsection: {out}"
    );
    // middleware.py owns 3 evals → it should win and appear as item 1.
    let top_files_idx = out.find("Top files:").expect("top files missing");
    let after = &out[top_files_idx..];
    let first_item_idx = after.find("1. ").expect("expected ranked item 1");
    let first_line = after[first_item_idx..]
        .lines()
        .next()
        .expect("first item line");
    assert!(
        first_line.contains("src/middleware.py"),
        "expected middleware.py to be the top blind-spot file: {first_line}"
    );

    // The Blind Spots section must sit between Architecture and Per-file detail.
    let arch_idx = out.find("## Architecture").expect("architecture missing");
    let bs_idx = out.find("## Blind Spots").expect("blind spots missing");
    let per_file_idx = out.find("## Per-file detail").expect("per-file missing");
    assert!(
        arch_idx < bs_idx && bs_idx < per_file_idx,
        "Blind Spots must render after Architecture and before Per-file detail"
    );
}

#[test]
fn summarize_json_includes_blind_spots_aggregate() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_blind_spots(tmp.path());
    let out = run_summarize(tmp.path(), &["--format", "json"]);

    let v: serde_json::Value = serde_json::from_str(&out).expect("output must be valid JSON");
    let bs = &v["blind_spots"];
    assert!(bs.is_object(), "expected `blind_spots` object: {v}");
    let total = bs["total"].as_u64().expect("total must be number");
    assert!(total >= 4, "expected ≥4 blind spots, got {total}");
    let by_kind = bs["by_kind"].as_object().expect("by_kind must be object");
    assert!(
        by_kind.contains_key("python-eval"),
        "expected python-eval kind, got: {by_kind:?}"
    );
    let top_files = bs["top_files"].as_array().expect("top_files must be array");
    assert!(!top_files.is_empty(), "top_files should not be empty");
    let middleware = top_files
        .iter()
        .find(|f| f["path"].as_str() == Some("src/middleware.py"))
        .expect("middleware.py expected in top_files");
    assert!(
        middleware["count"].as_u64().unwrap() >= 3,
        "middleware.py should account for ≥3 sites (3 eval)"
    );
}

#[test]
fn summarize_json_blind_spots_shape_when_empty() {
    // Repo without reflection: `blind_spots` key must still exist with
    // total=0, empty by_kind, empty top_files (shape stability for tooling).
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let out = run_summarize(tmp.path(), &["--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    let bs = &v["blind_spots"];
    assert_eq!(bs["total"].as_u64(), Some(0));
    assert!(bs["by_kind"].as_object().unwrap().is_empty());
    assert!(bs["top_files"].as_array().unwrap().is_empty());
}

#[test]
fn summarize_blind_spots_section_appears_with_zero_count_when_repo_clean() {
    // Spec: when the graph carries zero blind-spot records, the section MUST
    // still render with an explicit `Total: 0 sites.` line. Its presence
    // signals the LLM that gnx looked for reflection sites and confirmed
    // none — versus the section being absent for an unrelated reason.
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let out = run_summarize(tmp.path(), &[]);
    assert!(
        out.contains("## Blind Spots"),
        "Blind Spots section must always render, even when empty: {out}"
    );
    assert!(
        out.contains("Total: 0 sites."),
        "expected explicit `Total: 0 sites.` line: {out}"
    );
    // When zero, no per-kind or top-files breakdown should follow.
    assert!(
        !out.contains("By kind:"),
        "By kind: subsection should be suppressed on zero total: {out}"
    );
    assert!(
        !out.contains("Top files:"),
        "Top files: subsection should be suppressed on zero total: {out}"
    );
}
