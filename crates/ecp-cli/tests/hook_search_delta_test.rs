//! PostToolUse search delta: after a `rg` / `grep` / `git grep` Bash call or
//! a Grep tool call, the hook names only what the graph holds and the search
//! output does not show: a definition file, a non-test caller file, and a
//! mapping when the output mixes two same-name definitions.
//!
//! Pass/fail criterion for every test: a delta line may name a file only when
//! the tool output does not mention it, and the hook prints nothing when the
//! output already shows every definition and caller file.
//!
//! The fixture holds one call graph per language (Python, TypeScript, PHP).
//! A fact survives only while its file holds the indexed text: the tests that
//! rewrite a fixture file after the index prove the gate.

use crate::hook_pre_tool_use_test::{git_in, indexed_repo};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::tempdir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// The fixture's files. `load_user` (Python), `formatPrice` (TypeScript) and
/// `send_welcome` (PHP) each have one definition file and two non-test caller
/// files; `load_user` also has a test caller. `render_row` has one Python and
/// one TypeScript definition.
fn fixture_files() -> Vec<(&'static str, String)> {
    vec![
        ("models.py", "def load_user(uid):\n    return uid\n".into()),
        (
            "service.py",
            "from models import load_user\n\n\ndef fetch(uid):\n    return load_user(uid)\n".into(),
        ),
        (
            "api.py",
            "from models import load_user\n\n\ndef handle(uid):\n    return load_user(uid)\n".into(),
        ),
        (
            "tests/test_models.py",
            "from models import load_user\n\n\ndef test_load_user():\n    assert load_user(1) == 1\n"
                .into(),
        ),
        (
            "ts/format.ts",
            "export function formatPrice(n: number): string {\n  return String(n);\n}\n".into(),
        ),
        (
            "ts/cart.ts",
            "import { formatPrice } from './format';\n\nexport function cartTotal(): string {\n  return formatPrice(1);\n}\n"
                .into(),
        ),
        (
            "ts/checkout.ts",
            "import { formatPrice } from './format';\n\nexport function checkoutTotal(): string {\n  return formatPrice(2);\n}\n"
                .into(),
        ),
        (
            "php/Mailer.php",
            "<?php\nfunction send_welcome($user) {\n    return $user;\n}\n".into(),
        ),
        (
            "php/signup.php",
            "<?php\nrequire_once 'Mailer.php';\nfunction register_user($user) {\n    return send_welcome($user);\n}\n"
                .into(),
        ),
        (
            "php/invite.php",
            "<?php\nrequire_once 'Mailer.php';\nfunction invite_user($user) {\n    return send_welcome($user);\n}\n"
                .into(),
        ),
        ("views_a.py", "def render_row(row):\n    return row\n".into()),
        (
            "views_b.ts",
            "export function render_row(row: string): string {\n  return row;\n}\n".into(),
        ),
    ]
}

fn fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    indexed_repo(tmp, &fixture_files())
}

/// The hook command with only this test's `HOME`: no session of the
/// surrounding agent and no `ECP_HOME` reach it.
fn hook_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(ecp_bin());
    cmd.env("HOME", home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env_remove("ECP_HOME")
        .env_remove("ECP_SESSION_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CODEX_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("GEMINI_CLI_SESSION_ID");
    cmd
}

pub(crate) fn spawn_hook(home: &Path, event: &str, envelope: &str) -> std::process::Child {
    let mut child = hook_cmd(home)
        .args(["hook", event, "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child
}

/// The hook's `additionalContext`, `""` when it printed nothing. The payload
/// is parsed as the harness parses it: one JSON object on stdout.
pub(crate) fn hook_context(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        return String::new();
    }
    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    payload["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn grep_envelope(cwd: &Path, pattern: Value) -> String {
    json!({"cwd": cwd, "tool_name": "Grep", "tool_input": {"pattern": pattern}}).to_string()
}

/// The PostToolUse context for `envelope`.
fn post(home: &Path, envelope: &Value) -> String {
    hook_context(
        spawn_hook(home, "post-tool-use", &envelope.to_string())
            .wait_with_output()
            .unwrap(),
    )
}

/// A Bash call that ran `cmd` in `cwd` and printed `stdout`.
fn bash(cwd: &Path, cmd: &str, stdout: &str) -> Value {
    json!({
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false},
    })
}

/// `rg -n` lines for `files`, one match each.
fn rg_lines(files: &[&str], name: &str) -> String {
    files.iter().map(|f| format!("{f}:1:{name}\n")).collect()
}

const LOAD_USER_FILES: [&str; 4] = ["models.py", "service.py", "api.py", "tests/test_models.py"];

#[test]
fn test_handle_non_search_bash_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let ctx = post(&home, &bash(&repo, "ls -la", "models.py\nservice.py\n"));
    assert_eq!(ctx, "");
}

#[test]
fn test_handle_search_with_no_identifier_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    for cmd in ["rg -n 'for|error' .", r#"grep -rn "\d+" ."#, "rg -n ab ."] {
        assert_eq!(post(&home, &bash(&repo, cmd, "")), "", "{cmd}");
    }
}

#[test]
fn test_handle_single_file_search_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    assert_eq!(
        post(
            &home,
            &bash(
                &repo,
                "rg -n load_user models.py",
                "1:def load_user(uid):\n"
            )
        ),
        ""
    );
    assert_eq!(
        post(
            &home,
            &bash(&repo, "grep -n load_user service.py api.py", "")
        ),
        "",
        "every operand is a regular file"
    );
    let grep_tool = json!({
        "cwd": repo,
        "tool_name": "Grep",
        "tool_input": {"pattern": "load_user", "path": repo.join("models.py")},
        "tool_response": {"filenames": [], "content": ""},
    });
    assert_eq!(post(&home, &grep_tool), "");
}

#[test]
fn test_handle_output_mentions_every_caller_file_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let out = rg_lines(&LOAD_USER_FILES, "load_user");
    assert_eq!(post(&home, &bash(&repo, "rg -n load_user .", &out)), "");
    let without_test = rg_lines(&["models.py", "service.py", "api.py"], "load_user");
    assert_eq!(
        post(&home, &bash(&repo, "rg -n load_user .", &without_test)),
        "",
        "a test caller the output misses is no delta"
    );
}

/// Contract: one missing non-test caller file per language is named, the
/// files the output shows are not, and the impact command follows once.
#[test]
fn test_handle_missing_caller_file_is_emitted_in_python_typescript_php() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    for (name, shown, missing) in [
        ("load_user", ["models.py", "service.py"], "api.py"),
        (
            "formatPrice",
            ["ts/format.ts", "ts/cart.ts"],
            "ts/checkout.ts",
        ),
        (
            "send_welcome",
            ["php/Mailer.php", "php/signup.php"],
            "php/invite.php",
        ),
    ] {
        let cmd = format!("rg -n {name} .");
        let ctx = post(&home, &bash(&repo, &cmd, &rg_lines(&shown, name)));
        assert!(ctx.starts_with("ecp graph delta:\n"), "{name}: {ctx}");
        assert!(ctx.contains(missing), "{name}: {ctx}");
        for file in shown {
            assert!(!ctx.contains(&format!("{file},")), "{name}: {ctx}");
            assert!(!ctx.contains(&format!("{file}\n")), "{name}: {ctx}");
        }
        assert!(!ctx.contains("→"), "the definition file is shown: {ctx}");
        assert_eq!(
            ctx.matches(&format!("ecp impact --target {name} --direction upstream"))
                .count(),
            1,
            "{ctx}"
        );
        assert!(!ctx.contains("test_models.py"), "{ctx}");
    }
}

#[test]
fn test_handle_definition_file_not_in_output_is_emitted() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let out = rg_lines(&["service.py", "api.py"], "load_user");
    let ctx = post(&home, &bash(&repo, "rg -n load_user .", &out));
    assert!(ctx.contains("load_user → models.py:1 [Function]"), "{ctx}");
    assert!(
        !ctx.contains("ecp impact"),
        "no caller file is missing, so no impact hint: {ctx}"
    );
}

#[test]
fn test_handle_two_same_name_definitions_in_output_emits_mapping_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let out = rg_lines(&["views_a.py", "views_b.ts"], "render_row");
    let ctx = post(&home, &bash(&repo, "rg -n render_row .", &out));
    let line = ctx
        .lines()
        .find(|l| l.contains("render_row") && l.contains("definitions in the output"))
        .unwrap_or_else(|| panic!("no mapping line: {ctx}"));
    assert!(
        line.contains("views_a.py:1 [Function] ← views_a.py"),
        "{line}"
    );
    assert!(
        line.contains("views_b.ts:1 [Function] ← views_b.ts"),
        "{line}"
    );
    assert!(!ctx.contains("→"), "both definition files are shown: {ctx}");
}

#[test]
fn test_handle_cd_dir_then_rg_searches_that_repo() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let top = dunce::canonicalize(&repo).unwrap();
    let cmd = format!("cd '{}' && rg -n load_user .", repo.display());
    let out = rg_lines(&["models.py", "service.py"], "load_user");
    let ctx = post(&home, &bash(&elsewhere, &cmd, &out));
    assert!(
        ctx.starts_with(&format!("ecp graph delta in {}:\n", top.display())),
        "{ctx}"
    );
    assert!(ctx.contains("api.py"), "{ctx}");
}

#[test]
fn test_handle_cd_var_then_rg_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let ctx = post(
        &home,
        &bash(&repo, "cd \"$OTHER\" && rg -n load_user .", ""),
    );
    assert_eq!(ctx, "");
}

/// A search that reads a pipe names no repository file, so it has no delta.
#[test]
fn test_handle_search_of_piped_stdin_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let ctx = post(
        &home,
        &bash(&repo, "git log --oneline | grep load_user", ""),
    );
    assert_eq!(ctx, "");
}

#[test]
fn test_handle_egrep_and_git_grep_are_searches() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let out = rg_lines(&["models.py", "service.py"], "load_user");
    for cmd in ["egrep -rn load_user .", "git grep -n load_user"] {
        let ctx = post(&home, &bash(&repo, cmd, &out));
        assert!(ctx.contains("api.py"), "{cmd}: {ctx}");
    }
}

#[test]
fn test_handle_grep_tool_input_reports_missing_caller() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let top = dunce::canonicalize(&repo).unwrap();
    let envelope = json!({
        "cwd": repo,
        "tool_name": "Grep",
        "tool_input": {"pattern": "load_user", "output_mode": "files_with_matches"},
        "tool_response": {
            "mode": "files_with_matches",
            "filenames": [top.join("models.py"), top.join("service.py")],
            "numFiles": 2,
        },
    });
    let ctx = post(&home, &envelope);
    assert!(ctx.contains("api.py"), "{ctx}");
    assert!(!ctx.contains("service.py"), "{ctx}");
}

#[test]
fn test_handle_tool_output_string_is_read() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let mut envelope = bash(&repo, "rg -n load_user .", "");
    envelope["tool_response"] = json!(rg_lines(&LOAD_USER_FILES, "load_user"));
    assert_eq!(post(&home, &envelope), "");
    envelope["tool_response"] = json!(rg_lines(&["models.py", "service.py"], "load_user"));
    assert!(post(&home, &envelope).contains("api.py"));
}

#[test]
fn test_handle_tool_output_stdout_object_is_read() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let out = rg_lines(&LOAD_USER_FILES, "load_user");
    assert_eq!(post(&home, &bash(&repo, "rg -n load_user .", &out)), "");
    let out = rg_lines(&["models.py", "api.py"], "load_user");
    let ctx = post(&home, &bash(&repo, "rg -n load_user .", &out));
    assert!(ctx.contains("service.py"), "{ctx}");
}

#[test]
fn test_handle_tool_output_filenames_and_content_object_is_read() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let top = dunce::canonicalize(&repo).unwrap();
    let envelope = json!({
        "cwd": repo,
        "tool_name": "Grep",
        "tool_input": {"pattern": "load_user", "output_mode": "content"},
        "tool_response": {
            "mode": "content",
            "filenames": [top.join("models.py")],
            "content": format!(
                "{}:5:    return load_user(uid)\n",
                top.join("service.py").display()
            ),
        },
    });
    let ctx = post(&home, &envelope);
    assert!(ctx.contains("api.py"), "{ctx}");
    assert!(!ctx.contains("service.py"), "content names it: {ctx}");
    assert!(!ctx.contains("models.py"), "filenames name it: {ctx}");
}

/// Contract: an empty output is evidence that the search showed nothing, so
/// the definition and every non-test caller file are the delta, while their
/// files still hold the text the graph indexed.
#[test]
fn test_handle_tool_output_empty_reports_definition_and_callers() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let ctx = post(&home, &bash(&repo, "rg -n load_user .", ""));
    assert!(ctx.contains("load_user → models.py:1 [Function]"), "{ctx}");
    assert!(
        ctx.contains("api.py") && ctx.contains("service.py"),
        "{ctx}"
    );
    assert!(!ctx.contains("test_models.py"), "{ctx}");
}

/// Contract (the `doomed_fn` regression, ported from the PreToolUse graph
/// hits): the agent renames `load_user` without committing. `rg load_user`
/// prints nothing, and the delta must not claim the definition or its callers.
#[test]
fn test_handle_definition_renamed_after_the_index_is_not_stated() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    fs::write(
        repo.join("models.py"),
        "def load_account(uid):\n    return uid\n",
    )
    .unwrap();
    assert_eq!(post(&home, &bash(&repo, "rg -n load_user .", "")), "");
    fs::remove_file(repo.join("models.py")).unwrap();
    assert_eq!(post(&home, &bash(&repo, "rg -n load_user .", "")), "");
}

#[test]
fn test_handle_caller_file_edited_after_the_index_is_not_named() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    fs::write(repo.join("api.py"), "def handle(uid):\n    return uid\n").unwrap();
    let ctx = post(&home, &bash(&repo, "rg -n load_user .", ""));
    assert!(ctx.contains("service.py"), "{ctx}");
    assert!(!ctx.contains("api.py"), "{ctx}");
}

/// Contract: a graph that a sibling worktree built (this HEAD has none) never
/// names a file this tree lacks. `load_user`'s file is gone here and `send_welcome`'s
/// is not, so the delta still proves it read the graph.
#[test]
fn test_handle_sibling_graph_does_not_name_a_file_this_tree_lacks() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let wt = tmp.path().join("wt");
    git_in(
        &repo,
        &["worktree", "add", "-q", "-b", "other", wt.to_str().unwrap()],
    );
    git_in(&wt, &["rm", "-q", "models.py"]);
    git_in(&wt, &["commit", "-qm", "drop models"]);
    let ctx = post(&home, &bash(&wt, "rg -n 'load_user|send_welcome' .", ""));
    assert!(ctx.contains("send_welcome → php/Mailer.php"), "{ctx}");
    assert!(
        !ctx.contains("models.py") && !ctx.contains("load_user"),
        "{ctx}"
    );
}

/// Contract: a path with spaces is one path, in `path:line:` text and in the
/// Grep tool's `filenames` field. Both outputs show the definition's file, so
/// the delta is empty.
#[test]
fn test_handle_path_with_spaces_is_mentioned_in_text_and_filenames() {
    let tmp = tempdir().unwrap();
    let files = [(
        "src/my module.py",
        "def spaced_fn(x):\n    return x\n".to_owned(),
    )];
    let (home, repo) = indexed_repo(tmp.path(), &files);
    let text = "src/my module.py:1:def spaced_fn(x):\n";
    assert_eq!(post(&home, &bash(&repo, "rg -n spaced_fn .", text)), "");
    let top = dunce::canonicalize(&repo).unwrap();
    let grep_tool = json!({
        "cwd": repo,
        "tool_name": "Grep",
        "tool_input": {"pattern": "spaced_fn", "output_mode": "files_with_matches"},
        "tool_response": {"filenames": [top.join("src/my module.py")], "numFiles": 1},
    });
    assert_eq!(post(&home, &grep_tool), "");
}

/// Contract: `~/..` is the hook's HOME, so this operand names one regular
/// file, and a search of one file has no delta.
#[test]
fn test_handle_tilde_single_file_operand_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let cmd = "rg -n load_user ~/../repo/models.py";
    assert_eq!(
        post(&home, &bash(&repo, cmd, "1:def load_user(uid):\n")),
        ""
    );
}

/// Contract: a pattern with more than three identifiers states facts for the
/// first three that have a definition.
#[test]
fn test_handle_pattern_with_four_defined_identifiers_states_three() {
    let tmp = tempdir().unwrap();
    let names = ["alpha_fn", "bravo_fn", "charlie_fn", "delta_fn"];
    let files: Vec<(String, String)> = names
        .iter()
        .map(|n| (format!("{n}.py"), format!("def {n}():\n    return 1\n")))
        .collect();
    let borrowed: Vec<(&str, String)> =
        files.iter().map(|(f, s)| (f.as_str(), s.clone())).collect();
    let (home, repo) = indexed_repo(tmp.path(), &borrowed);
    let ctx = post(
        &home,
        &bash(
            &repo,
            "rg -n 'undefined_fn|alpha_fn|bravo_fn|charlie_fn|delta_fn' .",
            "",
        ),
    );
    for stated in &names[..3] {
        assert!(ctx.contains(&format!("{stated} → {stated}.py")), "{ctx}");
    }
    assert!(!ctx.contains("delta_fn"), "{ctx}");
}

#[test]
fn test_handle_tool_output_absent_emits_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = fixture(tmp.path());
    let mut envelope = bash(&repo, "rg -n load_user .", "");
    envelope.as_object_mut().unwrap().remove("tool_response");
    assert_eq!(post(&home, &envelope), "");
    assert_eq!(
        hook_context(
            spawn_hook(
                &home,
                "post-tool-use",
                &grep_envelope(&repo, "load_user".into())
            )
            .wait_with_output()
            .unwrap()
        ),
        ""
    );
}

/// Contract: the block stays within 600 bytes and ends on a whole line.
/// 12 definitions of each of three long names, none shown, overflow the cap.
/// The unit tests of `render_delta` pin which lines survive the cut.
#[test]
fn test_handle_output_over_600_bytes_is_cut_at_a_line_boundary() {
    let tmp = tempdir().unwrap();
    let names = [
        "overflowing_definition_name_alpha",
        "overflowing_definition_name_bravo",
        "overflowing_definition_name_charlie",
    ];
    let mut files: Vec<(String, String)> = Vec::new();
    for (n, name) in names.iter().enumerate() {
        for i in 0..12 {
            files.push((
                format!("deeply/nested/package_{n}/module_{i:02}.py"),
                format!("def {name}():\n    return {i}\n"),
            ));
        }
        files.push((
            format!("callers/use_{n}.py"),
            format!("from deeply.nested.package_{n}.module_00 import {name}\n\n\ndef go():\n    return {name}()\n"),
        ));
    }
    let borrowed: Vec<(&str, String)> =
        files.iter().map(|(f, s)| (f.as_str(), s.clone())).collect();
    let (home, repo) = indexed_repo(tmp.path(), &borrowed);
    let cmd = format!("rg -n '{}' .", names.join("|"));
    let ctx = post(&home, &bash(&repo, &cmd, ""));
    assert!(ctx.starts_with("ecp graph delta:\n"), "{ctx}");
    assert!(ctx.len() <= 600, "{} bytes: {ctx}", ctx.len());
    assert!(ctx.ends_with('\n'), "{ctx:?}");
    for line in ctx.lines().skip(1) {
        assert!(
            line.ends_with(']')
                || line.ends_with("more definitions")
                || line.ends_with(".py")
                || line.ends_with("more")
                || line.ends_with("--direction upstream"),
            "a cut line: {line:?}"
        );
    }
}

#[test]
fn test_handle_graph_absent_emits_nothing_and_exits_zero() {
    let tmp = tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        repo.join("models.py"),
        "def load_user(uid):\n    return uid\n",
    )
    .unwrap();
    git_in(&repo, &["init", "-q"]);
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let out = spawn_hook(
        &home,
        "post-tool-use",
        &bash(&repo, "rg -n load_user .", "").to_string(),
    )
    .wait_with_output()
    .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}
