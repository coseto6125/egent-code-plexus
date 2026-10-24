//! `--format` is a clap-typed value: the argument parser rejects an unknown
//! value before the command runs, and the error lists the valid values.
//! Before this change an unknown value fell back to the default silently.

use std::path::Path;
use std::process::{Command, Output};

const SOURCE: &str = "function foo() { return bar(); }\nfunction bar() { return 1; }\n";

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn ecp(repo: &Path, args: &[&str]) -> Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp spawn failed")
}

fn indexed_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.ts"), SOURCE).unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);
    let out = ecp(repo, &["admin", "index", "--repo", "."]);
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    tmp
}

/// One invocation per command under test.
const COMMANDS: &[&[&str]] = &[
    &["find", "foo"],
    &["impact", "--target", "bar", "--direction", "upstream"],
    &["inspect", "--name", "foo"],
    &["cypher", "MATCH (n:Function) RETURN n.name LIMIT 5"],
    &["routes"],
];

fn with_format(base: &[&str], value: &str) -> Vec<String> {
    let mut v: Vec<String> = base.iter().map(|s| s.to_string()).collect();
    v.push("--format".into());
    v.push(value.into());
    v
}

fn run_with(repo: &Path, base: &[&str], value: &str) -> Output {
    let argv = with_format(base, value);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    ecp(repo, &refs)
}

fn assert_rejected_at_parse(out: &Output, label: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{label}: expected non-zero exit");
    assert!(
        // clap words an empty and an unknown value differently; both list the
        // valid values, which is what an agent needs to retry.
        stderr.contains("possible values") && stderr.contains("json") && stderr.contains("toon"),
        "{label}: stderr must list the valid values: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "{label}: the command ran despite the bad value: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn test_format_flag_unknown_value_rejected_at_parse_with_valid_list() {
    let tmp = tempfile::tempdir().unwrap();
    for base in COMMANDS {
        let out = run_with(tmp.path(), base, "jsn");
        assert_rejected_at_parse(&out, base[0]);
    }
}

#[test]
fn test_format_flag_empty_value_rejected_at_parse() {
    let tmp = tempfile::tempdir().unwrap();
    for base in COMMANDS {
        let out = run_with(tmp.path(), base, "");
        assert_rejected_at_parse(&out, base[0]);
    }
}

/// Today `--format JSON` is not `json`: the old parser matched case-sensitively
/// and fell back to the default silently. The typed flag keeps the
/// case-sensitive match and turns the silent fallback into a parse error.
#[test]
fn test_format_flag_uppercase_value_rejected_at_parse() {
    let tmp = tempfile::tempdir().unwrap();
    for base in COMMANDS {
        let out = run_with(tmp.path(), base, "JSON");
        assert_rejected_at_parse(&out, base[0]);
    }
}

#[test]
fn test_format_flag_valid_values_still_run() {
    let tmp = indexed_repo();
    for base in COMMANDS {
        for value in ["llm", "toon", "json", "text"] {
            let out = run_with(tmp.path(), base, value);
            assert!(
                out.status.success(),
                "{} --format {value}: {}",
                base[0],
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let json = run_with(tmp.path(), base, "json");
        let stdout = String::from_utf8_lossy(&json.stdout);
        assert!(
            serde_json::from_str::<serde_json::Value>(stdout.trim()).is_ok(),
            "{} --format json is not JSON: {stdout}",
            base[0]
        );
    }
}

/// The default with no `--format` stays the LLM-tuned rendering, byte for byte
/// the same as the explicit `--format llm`.
#[test]
fn test_format_flag_absent_default_unchanged_matches_llm() {
    let tmp = indexed_repo();
    for base in COMMANDS {
        let default = ecp(tmp.path(), base);
        let llm = run_with(tmp.path(), base, "llm");
        assert!(default.status.success(), "{}: default run failed", base[0]);
        assert_eq!(
            String::from_utf8_lossy(&default.stdout),
            String::from_utf8_lossy(&llm.stdout),
            "{}: default differs from --format llm",
            base[0]
        );
    }
}

/// `schema` accepts only json and text: a value that is valid for another
/// command (`toon`) still fails there with the command's own error.
#[test]
fn test_format_flag_schema_toon_rejected_by_command() {
    let tmp = tempfile::tempdir().unwrap();
    let out = ecp(tmp.path(), &["schema", "node-kinds", "--format", "toon"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown --format `toon`") && stderr.contains("`json` or `text`"),
        "{stderr}"
    );
    let ok = ecp(tmp.path(), &["schema", "node-kinds", "--format", "text"]);
    assert!(ok.status.success());
}

/// The MCP server builds argv from tool arguments and spawns `ecp`, so a bad
/// format string from an MCP host reaches the same clap parser.
#[test]
fn test_format_flag_mcp_tools_unknown_value_rejected_valid_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = ecp(tmp.path(), &["admin", "mcp", "tools", "--format", "jsn"]);
    assert_rejected_at_parse(&bad, "admin mcp tools");
    let good = ecp(tmp.path(), &["admin", "mcp", "tools", "--format", "json"]);
    assert!(good.status.success());
}
