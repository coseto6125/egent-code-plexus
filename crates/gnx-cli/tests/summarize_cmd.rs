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
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());
    let out = Command::new(gnx_bin())
        .args(["summarize", "--format", "toml"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("spawn failed");
    assert!(!out.status.success(), "expected nonzero exit on bad format");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unsupported --format"),
        "stderr should mention unsupported format: {err}"
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
