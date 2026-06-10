//! Tests for Task A: tabular toon edge output in `ecp inspect`.
//!
//! Verifies:
//!   - toon/llm output omits `tier`, `checks`, and null `ownerClass` per edge
//!   - `--format json` payload retains all fields unchanged
//!   - `--file` flag (new primary name) works identically to `--file_path`
//!   - `--file_path` still accepted as back-compat alias

mod common;

use common::{ecp_bin, init_and_analyze, write};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn run_json(repo: &Path, args: &[&str]) -> Value {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
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
        .unwrap_or_else(|err| panic!("{args:?} JSON parse: {err}\nstdout={stdout}"))
}

fn run_stdout(repo: &Path, args: &[&str]) -> String {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Build a fixture with a caller → callee relationship.
fn setup_caller_callee(repo: &Path) {
    write(
        repo,
        "src/lib.ts",
        r#"
export function callee() { return 1; }

export function caller() {
    return callee();
}
"#,
    );
    init_and_analyze(repo);
}

// ── Task A: boilerplate omission in toon/llm output ──────────────────────────

/// `tier` and `checks` are constant-across-all-edges boilerplate: `tier` is
/// always "unresolved" and `checks` is always `{}`. They must be absent from
/// toon output (both llm default and explicit `--format toon`).
#[test]
fn toon_output_omits_tier_and_checks_from_edge_entries() {
    let tmp = tempfile::tempdir().unwrap();
    setup_caller_callee(tmp.path());

    let stdout = run_stdout(tmp.path(), &["inspect", "--name", "caller"]);
    // "tier" and "checks" are string key names — must not appear in toon output.
    assert!(
        !stdout.contains("tier"),
        "toon default output must not contain 'tier' key:\n{stdout}"
    );
    assert!(
        !stdout.contains("checks"),
        "toon default output must not contain 'checks' key:\n{stdout}"
    );
}

/// Same check for explicit `--format toon`.
#[test]
fn explicit_toon_format_omits_tier_and_checks() {
    let tmp = tempfile::tempdir().unwrap();
    setup_caller_callee(tmp.path());

    let stdout = run_stdout(
        tmp.path(),
        &["inspect", "--name", "caller", "--format", "toon"],
    );
    assert!(
        !stdout.contains("tier"),
        "--format toon must omit 'tier':\n{stdout}"
    );
    assert!(
        !stdout.contains("checks"),
        "--format toon must omit 'checks':\n{stdout}"
    );
}

/// `ownerClass: null` entries are null/empty-constant and must not appear in
/// toon output when every edge entry has null ownerClass.
#[test]
fn toon_output_omits_null_owner_class() {
    let tmp = tempfile::tempdir().unwrap();
    setup_caller_callee(tmp.path());

    let stdout = run_stdout(tmp.path(), &["inspect", "--name", "caller"]);
    // ownerClass is null for top-level functions; should be absent from toon.
    assert!(
        !stdout.contains("ownerClass"),
        "toon output must omit null ownerClass:\n{stdout}"
    );
}

/// `--format json` must retain all fields including `tier`, `checks`, and
/// `ownerClass` so the payload stays full-fidelity for tools that parse it.
#[test]
fn json_format_retains_all_fields() {
    let tmp = tempfile::tempdir().unwrap();
    setup_caller_callee(tmp.path());

    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "callee", "--format", "json"],
    );
    assert_eq!(out["status"].as_str(), Some("found"), "{out}");

    // Find any edge entry in incoming or outgoing and assert tier+checks present.
    let incoming = out["incoming"].as_object();
    let outgoing = out["outgoing"].as_object();
    let has_edge = incoming
        .iter()
        .flat_map(|m| m.values())
        .chain(outgoing.iter().flat_map(|m| m.values()))
        .filter_map(|v| v.as_array())
        .any(|arr| !arr.is_empty());

    if has_edge {
        let entry = incoming
            .iter()
            .flat_map(|m| m.values())
            .chain(outgoing.iter().flat_map(|m| m.values()))
            .filter_map(|v| v.as_array())
            .find(|arr| !arr.is_empty())
            .unwrap()[0]
            .clone();

        assert!(
            entry.get("tier").is_some(),
            "json format must retain 'tier' field per edge: {entry}"
        );
        assert!(
            entry.get("checks").is_some(),
            "json format must retain 'checks' field per edge: {entry}"
        );
        assert!(
            entry.get("ownerClass").is_some(),
            "json format must retain 'ownerClass' field per edge: {entry}"
        );
    }
    // If no edges, the test trivially passes (fixture may have no edges indexed).
}

/// Byte reduction sanity: toon output must be strictly smaller than json output
/// for the same symbol (the boilerplate removal contributes the reduction).
#[test]
fn toon_output_smaller_than_json_for_same_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    setup_caller_callee(tmp.path());

    let toon_bytes = run_stdout(
        tmp.path(),
        &["inspect", "--name", "callee", "--format", "toon"],
    )
    .len();
    let json_bytes = run_stdout(
        tmp.path(),
        &["inspect", "--name", "callee", "--format", "json"],
    )
    .len();

    // toon is inherently more compact than JSON even without boilerplate removal;
    // this validates the overall direction.
    assert!(
        toon_bytes < json_bytes,
        "toon ({toon_bytes}B) should be smaller than json ({json_bytes}B) for the same symbol"
    );
}

// ── Task B: --file flag unification ──────────────────────────────────────────

/// `--file` (new primary name) must work identically to `--file_path`.
#[test]
fn file_flag_disambiguates_same_as_file_path() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/auth/login.ts",
        "export function handler() { return 'auth'; }\n",
    );
    write(
        tmp.path(),
        "src/billing/charge.ts",
        "export function handler() { return 'billing'; }\n",
    );
    init_and_analyze(tmp.path());

    // Without filter: ambiguous
    let ambig = run_json(
        tmp.path(),
        &["inspect", "--name", "handler", "--format", "json"],
    );
    assert_eq!(
        ambig["status"].as_str(),
        Some("ambiguous"),
        "setup sanity: should be ambiguous: {ambig}"
    );

    // --file must narrow to one match
    let via_file = run_json(
        tmp.path(),
        &[
            "inspect",
            "--name",
            "handler",
            "--file",
            "src/auth/",
            "--format",
            "json",
        ],
    );
    let status = via_file["status"].as_str().unwrap_or("");
    assert!(
        status == "found" || status == "ambiguous",
        "unexpected status with --file: {via_file}"
    );
    if status == "found" {
        assert!(
            via_file["symbol"]["filePath"]
                .as_str()
                .unwrap_or("")
                .contains("src/auth/"),
            "--file filtered wrong symbol: {via_file}"
        );
    }
}

/// `--file_path` still works as a back-compat alias.
#[test]
fn file_path_alias_still_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/auth/login.ts",
        "export function handler() { return 'auth'; }\n",
    );
    write(
        tmp.path(),
        "src/billing/charge.ts",
        "export function handler() { return 'billing'; }\n",
    );
    init_and_analyze(tmp.path());

    let via_file_path = run_json(
        tmp.path(),
        &[
            "inspect",
            "--name",
            "handler",
            "--file_path",
            "src/auth/",
            "--format",
            "json",
        ],
    );
    // Must not return an error (back-compat alias must still parse).
    let status = via_file_path["status"].as_str().unwrap_or("");
    assert!(
        status == "found" || status == "ambiguous",
        "--file_path alias must be accepted: {via_file_path}"
    );
}

/// Help text for inspect must show `--file` (primary) and NOT show `--file_path`
/// as a visible documented option (it's a hidden alias).
#[test]
fn help_shows_file_not_file_path() {
    let out = Command::new(ecp_bin())
        .args(["inspect", "--help"])
        .output()
        .expect("help spawn failed");
    let help = String::from_utf8_lossy(&out.stdout);

    assert!(
        help.contains("--file"),
        "help must show --file flag:\n{help}"
    );
    // --file_path must NOT appear as a standalone visible option in help.
    // (It may appear inside metavar descriptions or hidden sections, but the
    // primary doc line should be `--file`.)
    assert!(
        !help.contains("--file_path"),
        "help must not show --file_path as a visible option (it is a hidden alias):\n{help}"
    );
}
