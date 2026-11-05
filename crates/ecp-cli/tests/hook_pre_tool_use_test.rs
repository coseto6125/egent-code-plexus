//! PreToolUse hook: pattern extraction + in-process graph augmentation.
//! Covers no-op branches and the with-index → emit-hits branch (which
//! was deferred in PR #17 and is now reachable thanks to the
//! `TantivyEngine` wireup + 1-hop expansion in `compute_hits`).

use std::io::Write;
use std::process::{Command, Stdio};

use ecp_cli::search::TantivyEngine;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run(envelope: &str) -> std::process::Output {
    run_with_home(envelope, None)
}

/// Run the hook with an optional HOME override so a fake registry can
/// be planted at `<home>/.ecp/registry.json`. Each subprocess inherits
/// the env we set on the child only — parent's env is untouched.
fn run_with_home(envelope: &str, home: Option<&std::path::Path>) -> std::process::Output {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(["hook", "pre-tool-use", "--claude-code"]);
    if let Some(h) = home {
        cmd.env("HOME", h);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn short_pattern_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"ab"}}"#);
    assert!(out.stdout.is_empty(), "<3 char pattern should no-op");
}

#[test]
fn missing_graph_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"validateUser"}}"#);
    assert!(out.stdout.is_empty(), "no registry entry for /tmp → no-op");
}

#[test]
fn bash_grep_no_index_no_op() {
    let out = run(
        r#"{"cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"rg -n 'validateUser' src/"}}"#,
    );
    assert!(
        out.stdout.is_empty(),
        "no index → no-op even with valid pattern"
    );
    assert!(out.status.success(), "hook must never fail on no-op");
}

#[test]
fn non_search_tool_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Read","tool_input":{"file_path":"foo"}}"#);
    assert!(out.stdout.is_empty());
}

#[test]
fn glob_pattern_with_no_index_no_op() {
    let out = run(
        r#"{"cwd":"/tmp","tool_name":"Glob","tool_input":{"pattern":"src/**/validateUser.rs"}}"#,
    );
    assert!(out.stdout.is_empty());
}

/// Build a minimal 3-node graph with one CALLS edge so the hook has
/// enough fixture to surface a hit + a `Called by:` line.
fn make_graph() -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();
    // node 0 = parseConfig, 1 = loadConfig, 2 = tokenize.
    // edges: parseConfig→tokenize, loadConfig→parseConfig.
    let parse = fx.func("src/lib.rs", "parseConfig");
    fx.span(parse, (10, 0, 11, 0));
    let load = fx.func("src/lib.rs", "loadConfig");
    fx.span(load, (20, 0, 21, 0));
    let tok = fx.func("src/lib.rs", "tokenize");
    fx.span(tok, (30, 0, 31, 0));
    fx.edge_with(parse, tok, RelType::Calls, 1.0, "call");
    fx.edge_with(load, parse, RelType::Calls, 1.0, "call");
    fx.build()
}

#[test]
#[ignore = "fixture mocks v1 registry + <repo>/<branch>/ layout; needs full rewrite to v2 (<repo>__<hash>/commits/<dirname>/ + BTreeMap registry)"]
fn with_index_emits_legacy_block_via_subprocess() {
    // The hook resolves cwd → index_dir via `~/.ecp/registry.json`.
    // We plant both the registry and the per-branch index dir under a
    // tempdir, then point HOME at it for the subprocess.
    let tmp = tempdir().unwrap();
    let fake_home = tmp.path().join("home");
    let home_ecp = fake_home.join(".ecp");
    let repo = tmp.path().join("repo");
    let index_dir = home_ecp.join("alpha").join("main");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&index_dir).unwrap();

    let graph = make_graph();
    fs::write(
        index_dir.join("graph.bin"),
        rkyv::to_bytes::<Error>(&graph).unwrap().as_slice(),
    )
    .unwrap();
    TantivyEngine::build_index(&index_dir, &graph).expect("tantivy build");

    let registry = serde_json::json!({
        "version": 1,
        "repos": [{
            "name": "alpha",
            "remote_url": "",
            "worktree_path": repo.to_string_lossy(),
            "index_dir_root": home_ecp.join("alpha").to_string_lossy(),
            "branches": [{
                "name": "main",
                "index_dir": index_dir.to_string_lossy(),
                "indexed_at": "2026-05-16T00:00:00Z",
                "node_count": 3u32,
                "delta_size": 0u64
            }],
            "groups": []
        }],
        "groups": []
    });
    fs::write(
        home_ecp.join("registry.json"),
        serde_json::to_string(&registry).unwrap(),
    )
    .unwrap();

    let envelope = format!(
        r#"{{"cwd":"{}","tool_name":"Grep","tool_input":{{"pattern":"parseConfig"}}}}"#,
        repo.display()
    );
    let out = run_with_home(&envelope, Some(&fake_home));
    assert!(
        out.status.success(),
        "hook must not error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("parseConfig"),
        "stdout should mention the matched symbol; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Called by: loadConfig"),
        "stdout should expose 1-hop callers; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Calls: tokenize"),
        "stdout should expose 1-hop callees; got:\n{stdout}"
    );
}

fn run_edit_event(
    event: &str,
    envelope: &serde_json::Value,
    home: &std::path::Path,
) -> std::process::Output {
    let mut child = Command::new(ecp_bin())
        .args(["hook", event, "--claude-code"])
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(envelope.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn test_edit_hook_actual_envelope_reports_before_and_after_consumers() {
    let repo = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = repo.path().join("x.js");
    fs::write(&path, "let x = 1 + 2;\nconsume(x);\n").unwrap();
    let mut input = serde_json::json!({"session_id":"session-1","tool_use_id":"edit-1","cwd":repo.path(),"tool_name":"Edit","tool_input":{"file_path":path,"old_string":"1 + 2","new_string":"1 * 2"}});
    let before = run_edit_event("pre-tool-use", &input, home.path());
    assert!(
        before.status.success(),
        "{}",
        String::from_utf8_lossy(&before.stderr)
    );
    let before: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    let context = before["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("before edit"), "{context}");
    assert!(context.contains("x.js:2"), "{context}");
    fs::write(&path, "let x = 1 * 2;\nconsume(x);\n").unwrap();
    input["tool_response"] = serde_json::json!({"success":true});
    let after = run_edit_event("post-tool-use", &input, home.path());
    assert!(after.status.success());
    let after: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    let context = after["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("after edit"), "{context}");
    assert!(context.contains("x.js:2"), "{context}");
}

#[test]
fn test_edit_hook_failed_tool_response_emits_no_current_claim() {
    let repo = tempdir().unwrap();
    let home = tempdir().unwrap();
    let input = serde_json::json!({"cwd":repo.path(),"tool_name":"Write","tool_input":{"file_path":repo.path().join("x.js"),"content":"let x = 1;"},"tool_response":{"is_error":true}});
    let output = run_edit_event("post-tool-use", &input, home.path());
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Contract: the Edit hook reads the edited file only. A sibling source the
/// process cannot read neither slows the hook down nor turns its evidence into
/// "unresolved"; cross-file consumers are left to `ecp review --include flow`.
#[cfg(unix)]
#[test]
fn test_edit_hook_ignores_unreadable_sibling_sources() {
    use std::os::unix::fs::PermissionsExt;
    let repo = tempdir().unwrap();
    let home = tempdir().unwrap();
    let path = repo.path().join("x.js");
    fs::write(&path, "let x = 1;\nconsume(x);\n").unwrap();
    let sibling = repo.path().join("y.js");
    fs::write(&sibling, "import { x } from './x.js';\nconsume(x);\n").unwrap();
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o000)).unwrap();
    let input = serde_json::json!({"session_id":"session-2","tool_use_id":"edit-2","cwd":repo.path(),"tool_name":"Edit","tool_input":{"file_path":path,"old_string":"1","new_string":"2"}});
    let output = run_edit_event("pre-tool-use", &input, home.path());
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let context = payload["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("x.js:2"), "{context}");
    assert!(!context.contains("unresolved"), "{context}");
    assert!(
        !repo.path().join(".ecp").exists(),
        "edit snapshots must not be written into the repository"
    );
}

fn git_in(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Contract: with a published graph, the edit hook's evidence covers the
/// edited file plus its direct importers, and the header names that scope.
/// Without one it stays single-file (`test_edit_hook_ignores_unreadable_sibling_sources`).
#[test]
fn test_edit_hook_reports_consumers_in_direct_importers_from_the_graph() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("x.js"), "export function x() { return 1; }\n").unwrap();
    fs::write(
        repo.join("y.js"),
        "import { x } from './x.js';\nconsume(x());\n",
    )
    .unwrap();
    fs::write(repo.join("z.js"), "consume(2);\n").unwrap();
    git_in(&repo, &["init", "-q"]);
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-qm", "init"]);
    let indexed = Command::new(ecp_bin())
        .args(["admin", "index", "--repo"])
        .arg(&repo)
        .env("HOME", &home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let input = serde_json::json!({"session_id":"session-3","tool_use_id":"edit-3","cwd":repo,"tool_name":"Edit","tool_input":{"file_path":repo.join("x.js"),"old_string":"return 1","new_string":"return 2"}});
    let output = run_edit_event("pre-tool-use", &input, &home);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let context = payload["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        context.contains("Scope: edited file and 1 direct importers from the graph (0 omitted)"),
        "{context}"
    );
    assert!(context.contains("y.js:2"), "{context}");
    assert!(!context.contains("z.js"), "{context}");
}

fn indexed_repo(
    tmp: &std::path::Path,
    files: &[(&str, String)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    for (name, source) in files {
        let path = repo.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    git_in(&repo, &["init", "-q"]);
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-qm", "init"]);
    let indexed = Command::new(ecp_bin())
        .args(["admin", "index", "--repo"])
        .arg(&repo)
        .env("HOME", &home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env_remove("ECP_HOME")
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    (home, repo)
}

fn edit_context(
    home: &std::path::Path,
    cwd: &std::path::Path,
    file: &std::path::Path,
    old: &str,
    new: &str,
) -> String {
    let input = serde_json::json!({"session_id":"session-4","tool_use_id":"edit-4","cwd":cwd,"tool_name":"Edit","tool_input":{"file_path":file,"old_string":old,"new_string":new}});
    let output = run_edit_event("pre-tool-use", &input, home);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    payload["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Contract (codex c1 / R1 F1): an edit to a barrel scopes the consumers
/// that import a name through it. Their symbol edge lands in the module the
/// barrel re-exports from, so only the File → barrel edge kept beside it
/// makes them the barrel's direct importers: a TS `export *` barrel and a
/// Python package `__init__.py`. Before the fix both edits reported 0.
#[test]
fn test_edit_hook_barrel_edit_finds_importers_through_the_barrel() {
    let tmp = tempdir().unwrap();
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[
            ("ts/index.ts", "export * from './impl';\n".into()),
            (
                "ts/impl.ts",
                "export function render() { return 1; }\n".into(),
            ),
            (
                "ts/use.ts",
                "import { render } from './index';\nconsume(render());\n".into(),
            ),
            ("pkg/__init__.py", "from .core import compute\n".into()),
            ("pkg/core.py", "def compute():\n    return 1\n".into()),
            (
                "use1.py",
                "from pkg import compute\n\nconsume(compute())\n".into(),
            ),
        ],
    );
    for (barrel, old, new) in [
        ("ts/index.ts", "./impl", "./impl.ts"),
        (
            "pkg/__init__.py",
            "import compute",
            "import compute as compute",
        ),
    ] {
        let context = edit_context(&home, &repo, &repo.join(barrel), old, new);
        assert!(
            context
                .contains("Scope: edited file and 1 direct importers from the graph (0 omitted)"),
            "{barrel}: {context}"
        );
    }
}

/// Contract: importers never cost the edited file its own evidence. When the
/// cross-file corpus exhausts the analysis budget, the hook reports the
/// single-file result and says why the scope shrank.
#[test]
fn test_edit_hook_falls_back_to_the_edited_file_when_importers_exhaust_the_budget() {
    let tmp = tempdir().unwrap();
    let filler = "0;\n".repeat(40_000);
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[
            (
                "x.js",
                "export function f() {\n  let x = 1;\n  consume(x);\n}\n".into(),
            ),
            ("y1.js", format!("import {{ f }} from './x.js';\n{filler}")),
            ("y2.js", format!("import {{ f }} from './x.js';\n{filler}")),
            ("y3.js", format!("import {{ f }} from './x.js';\n{filler}")),
        ],
    );
    let context = edit_context(&home, &repo, &repo.join("x.js"), "let x = 1", "let x = 2");
    assert!(
        context.contains(
            "Scope: edited file only (its direct importers exceeded the analysis budget)"
        ),
        "{context}"
    );
    assert!(context.contains("x.js:3"), "{context}");
    assert!(!context.contains("truncated=true"), "{context}");
}

/// Contract: a hook fired from a subdirectory keys paths at the worktree
/// root, so the graph lookup and the importers' import specifiers agree.
#[test]
fn test_edit_hook_from_a_subdirectory_keys_paths_at_the_worktree_root() {
    let tmp = tempdir().unwrap();
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[
            ("lib.js", "export function x() { return 1; }\n".into()),
            (
                "use.js",
                "import { x } from './lib.js';\nconsume(x());\n".into(),
            ),
            ("sub/lib.js", "export function x() { return 1; }\n".into()),
            (
                "sub/real.js",
                "import { x } from './lib.js';\nconsume(x());\n".into(),
            ),
        ],
    );
    let context = edit_context(
        &home,
        &repo.join("sub"),
        &repo.join("sub").join("lib.js"),
        "return 1",
        "return 2",
    );
    assert!(context.contains("sub/real.js:2"), "{context}");
    assert!(!context.contains("use.js"), "{context}");
    assert!(context.contains("1 direct importers"), "{context}");
}

/// Contract: a cwd that is a symlink to a subdirectory still resolves the
/// worktree root through the physical path, so importers are found.
#[cfg(unix)]
#[test]
fn test_edit_hook_symlinked_subdirectory_cwd_finds_importers() {
    let tmp = tempdir().unwrap();
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[
            ("sub/lib.js", "export function x() { return 1; }\n".into()),
            (
                "sub/real.js",
                "import { x } from './lib.js';\nconsume(x());\n".into(),
            ),
        ],
    );
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(repo.join("sub"), &alias).unwrap();
    let context = edit_context(&home, &alias, &alias.join("lib.js"), "return 1", "return 2");
    assert!(context.contains("sub/real.js:2"), "{context}");
    assert!(context.contains("1 direct importers"), "{context}");
}

#[test]
fn test_bash_cd_prefix_unindexed_repo_no_op() {
    // cd resolves to a real but unregistered dir: still a clean no-op,
    // never a failure or a stray payload.
    let tmp = tempdir().unwrap();
    let cmd = format!(
        "cd '{}' && grep -rn CacheKeyQueue app",
        tmp.path().display()
    );
    let env = serde_json::json!({
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
    })
    .to_string();
    let out = run(&env);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "unindexed cd target → no-op");
}

fn bash_hook_stdout(home: &std::path::Path, cwd: &std::path::Path, cmd: &str) -> String {
    let env = serde_json::json!({
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
    })
    .to_string();
    let out = run_with_home(&env, Some(home));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A leading `cd` into an indexed repo searches that repo, and the header
/// names it: the agent's own cwd is elsewhere.
#[test]
fn test_bash_cd_into_indexed_repo_names_the_repo_in_the_header() {
    let tmp = tempdir().unwrap();
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[(
            "lib.js",
            "export function CacheKeyQueue() { return 1; }\n".into(),
        )],
    );
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let repo_dir = dunce::canonicalize(&repo).unwrap();
    // The second command continues its line, as agents lay out long ones.
    for cmd in [
        format!("cd '{}' && grep -rn CacheKeyQueue .", repo.display()),
        format!("cd '{}' && \\\ngrep -rn CacheKeyQueue .", repo.display()),
    ] {
        let out: serde_json::Value =
            serde_json::from_str(&bash_hook_stdout(&home, &elsewhere, &cmd)).unwrap();
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_default();
        assert!(
            context.starts_with(&format!("ecp graph hits in {}:\n", repo_dir.display())),
            "{cmd:?}: {context}"
        );
        assert!(
            context.contains("CacheKeyQueue (lib.js:1)"),
            "{cmd:?}: {context}"
        );
    }
}

/// No cd keeps the plain header; a cd the hook cannot place prints no hits
/// rather than the envelope cwd's.
#[test]
fn test_bash_no_cd_plain_header_unknown_cd_no_hits() {
    let tmp = tempdir().unwrap();
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[(
            "lib.js",
            "export function CacheKeyQueue() { return 1; }\n".into(),
        )],
    );
    let out = bash_hook_stdout(&home, &repo, "grep -rn CacheKeyQueue .");
    // The payload is JSON, so the header's newline is the escape `\n`.
    assert!(out.contains(r"ecp graph hits:\n"), "{out}");
    for cmd in [
        "cd \"$OTHER\" && grep -rn CacheKeyQueue .",
        "make || cd sub; grep -rn CacheKeyQueue .",
    ] {
        assert_eq!(bash_hook_stdout(&home, &repo, cmd), "", "{cmd}");
    }
}

#[test]
fn test_bash_cd_nonexistent_dir_no_op() {
    let out = run(
        r#"{"cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"cd /no/such/dir_zz && grep -rn CacheKeyQueue app"}}"#,
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

// ── Dirty-worktree overlay in the hit block (FU-2026-09-05-b7c8975c323b) ─────
//
// Contract: a hit from a file edited since the build reports where the symbol
// is in the working tree, a symbol deleted there is not offered, and one added
// there is. The hook reads the overlay the last `ecp` query wrote for the
// session and does not refresh it itself. A hit prints a file:line only when
// the working-tree file holds the text that line came from; otherwise the
// hook drops the hit, names the file in an `omitted:` line, and starts a
// background refresh.
//
// Pass/fail criterion for every test below: a hit line may name a file:line
// only when that line is the line in the working tree. A wrong line is a fail
// even when the output looks plausible.

const OVERLAY_SID: &str = "hook-overlay-session";

fn session_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(ecp_bin());
    cmd.env("HOME", home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env_remove("ECP_HOME")
        .env_remove("ECP_SESSION_ID")
        .env_remove("CODEX_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("GEMINI_CLI_SESSION_ID")
        .env("CLAUDE_CODE_SESSION_ID", OVERLAY_SID);
    cmd
}

/// The session dir a query in `repo` writes under `OVERLAY_SID`:
/// `sessions/<sid>~<repo's gitdir key>` of the fixture's one repo dir.
fn overlay_session_dir(
    home: &std::path::Path,
    repo: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let name = ecp_cli::session::resolver::session_dir_name(OVERLAY_SID, repo);
    fs::read_dir(home.join(".ecp"))
        .ok()?
        .flatten()
        .map(|e| e.path().join("sessions").join(&name))
        .find(|p| p.is_dir())
}

/// Indexed repo whose `tools.rs` holds `relocated` (line 1) and `vanished`
/// (line 2) at HEAD.
fn overlay_hook_repo(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    indexed_repo(
        tmp,
        &[("tools.rs", "fn relocated() {}\nfn vanished() {}\n".into())],
    )
}

/// One `ecp find` writes the session's overlay fragments, as any earlier query
/// of the agent's session does.
fn warm_session_overlay(home: &std::path::Path, repo: &std::path::Path) {
    let out = session_cmd(home)
        .args(["find", "relocated", "--repo"])
        .arg(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Move `relocated` down 24 lines, delete `vanished`, add `newcomer`.
fn dirty_tools_rs(repo: &std::path::Path) {
    std::thread::sleep(std::time::Duration::from_millis(50));
    let padding = "// pad\n".repeat(24);
    fs::write(
        repo.join("tools.rs"),
        format!("{padding}fn relocated() {{}}\nfn newcomer() {{}}\n"),
    )
    .unwrap();
}

fn grep_envelope(cwd: &std::path::Path, pattern: serde_json::Value) -> String {
    serde_json::json!({"cwd": cwd, "tool_name": "Grep", "tool_input": {"pattern": pattern}})
        .to_string()
}

fn spawn_hook(home: &std::path::Path, envelope: &str) -> std::process::Child {
    spawn_hook_with(session_cmd(home), envelope)
}

fn spawn_hook_with(mut cmd: Command, envelope: &str) -> std::process::Child {
    let mut child = cmd
        .args(["hook", "pre-tool-use", "--claude-code"])
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
fn hook_context(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        return String::new();
    }
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    payload["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn hook_hits(home: &std::path::Path, repo: &std::path::Path, pattern: &str) -> String {
    let envelope = grep_envelope(repo, pattern.into());
    hook_context(spawn_hook(home, &envelope).wait_with_output().unwrap())
}

#[test]
fn test_hook_hits_moved_function_in_dirty_file_reports_working_tree_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);

    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(ctx.contains("relocated (tools.rs:25)"), "{ctx}");
    assert!(!ctx.contains("relocated (tools.rs:1)"), "{ctx}");
}

#[test]
fn test_hook_hits_function_deleted_in_dirty_file_is_not_offered() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    assert!(
        hook_hits(&home, &repo, "vanished").contains("vanished (tools.rs:2)"),
        "control: the clean worktree offers the symbol"
    );
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);

    assert_eq!(hook_hits(&home, &repo, "vanished"), "");
}

#[test]
fn test_hook_hits_function_added_in_dirty_file_is_offered() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);

    let ctx = hook_hits(&home, &repo, "newcomer");
    assert!(ctx.contains("newcomer (tools.rs:26)"), "{ctx}");
}

/// A session that exists but holds no dirty file adds nothing: the block is
/// byte-identical to the one printed before the session existed. The session
/// is made real by a dirty edit that is then restored to HEAD and re-queried.
#[test]
fn test_hook_hits_clean_worktree_output_unchanged_by_session() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    let before = hook_hits(&home, &repo, "relocated");
    assert!(
        before.starts_with("ecp graph hits:\n") && before.contains("relocated (tools.rs:1)"),
        "{before}"
    );
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    git_in(&repo, &["checkout", "--", "tools.rs"]);
    warm_session_overlay(&home, &repo);
    assert!(
        overlay_session_dir(&home, &repo).is_some(),
        "setup: the session dir exists"
    );
    assert_eq!(hook_hits(&home, &repo, "relocated"), before);
}

/// The hook process carries no host session env var: the `session_id` of its
/// JSON input names the session whose overlay it reads. Fails on a hook that
/// resolves the id from the environment alone (`resolve_session_id(None)`):
/// it finds no session dir and reports the stale line 1.
#[test]
fn test_hook_hits_session_id_from_input_when_env_absent_reports_working_tree_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);

    let mut envelope: serde_json::Value =
        serde_json::from_str(&grep_envelope(&repo, "relocated".into())).unwrap();
    envelope["session_id"] = OVERLAY_SID.into();
    let mut cmd = session_cmd(&home);
    cmd.env_remove("CLAUDE_CODE_SESSION_ID");
    let out = spawn_hook_with(cmd, &envelope.to_string())
        .wait_with_output()
        .unwrap();
    let ctx = hook_context(out);
    assert!(ctx.contains("relocated (tools.rs:25)"), "{ctx}");
}

/// The session was warmed on commit 1; HEAD has moved to an indexed commit 2.
/// Commit 1's overlay must not attach to commit 2's graph. Commit 2 is made
/// with plumbing so the dirty file, and so commit 1's fragment, stay valid on
/// disk: only the `l2_dirname` guard in `with_dirty_overlay` keeps the
/// fragment off, and deleting it turns this red (line 25 is printed).
#[test]
fn test_hook_hits_session_on_another_commit_does_not_attach_its_overlay() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);

    let committed = tmp.path().join("committed_tools.rs");
    fs::write(
        &committed,
        "// b\n// b\nfn relocated() {}\nfn vanished() {}\n",
    )
    .unwrap();
    let blob = Command::new("git")
        .current_dir(&repo)
        .args(["hash-object", "-w"])
        .arg(&committed)
        .output()
        .unwrap();
    assert!(blob.status.success());
    let blob = String::from_utf8(blob.stdout).unwrap();
    git_in(
        &repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{},tools.rs", blob.trim()),
        ],
    );
    git_in(&repo, &["commit", "-qm", "second"]);
    let indexed = Command::new(ecp_bin())
        .args(["admin", "index", "--repo"])
        .arg(&repo)
        .env("HOME", &home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env_remove("ECP_HOME")
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );

    // Commit 2's graph holds `relocated` at line 3; the working tree holds it
    // at line 25. Neither line is the claim: the graph's text is not on disk.
    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(!ctx.contains("tools.rs:25"), "{ctx}");
    assert!(!ctx.contains("tools.rs:3"), "{ctx}");
    assert!(
        ctx.contains("omitted:") && ctx.contains("tools.rs"),
        "{ctx}"
    );
}

/// Two hooks fired at once on one dirty worktree (parallel tool calls) both
/// read the same overlay and agree.
#[test]
fn test_hook_hits_concurrent_duplicate_calls_agree_on_working_tree_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);

    let envelope = grep_envelope(&repo, "relocated".into());
    let (a, b) = (spawn_hook(&home, &envelope), spawn_hook(&home, &envelope));
    let (a, b) = (
        hook_context(a.wait_with_output().unwrap()),
        hook_context(b.wait_with_output().unwrap()),
    );
    assert!(a.contains("relocated (tools.rs:25)"), "{a}");
    assert_eq!(a, b);
}

/// A dirty file the parser cannot read must not make the hook fail or print
/// anything but one valid envelope (or nothing). Which hit it then offers is
/// the overlay's contract for unreadable files, not this hook's.
#[test]
fn test_hook_hits_dirty_file_that_fails_to_parse_still_emits_valid_payload() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(
        repo.join("tools.rs"),
        [0xff_u8, 0xfe, 0x00, b'f', b'n', b' ', 0xc3],
    )
    .unwrap();
    warm_session_overlay(&home, &repo);

    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(
        ctx.is_empty() || ctx.starts_with("ecp graph hits:\n"),
        "{ctx}"
    );
}

#[test]
fn test_hook_hits_index_missing_in_dirty_repo_emits_nothing() {
    let tmp = tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(repo.join("tools.rs"), "fn relocated() {}\n").unwrap();
    git_in(&repo, &["init", "-q"]);
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-qm", "init"]);
    fs::write(repo.join("tools.rs"), "// pad\nfn relocated() {}\n").unwrap();

    assert_eq!(hook_hits(&home, &repo, "relocated"), "");
}

#[test]
fn test_hook_hits_empty_input_and_unusable_patterns_emit_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);

    let blank = grep_envelope(&repo, "   ".into());
    let list = grep_envelope(&repo, serde_json::json!(["relocated"]));
    let absent =
        serde_json::json!({"cwd": repo, "tool_name": "Grep", "tool_input": {}}).to_string();
    for envelope in ["", "{}", blank.as_str(), list.as_str(), absent.as_str()] {
        let ctx = hook_context(spawn_hook(&home, envelope).wait_with_output().unwrap());
        assert_eq!(ctx, "", "envelope {envelope:?}");
    }
}

// ── Per-hit proof: a hit names a line only when the file still holds its text ──

/// Rewrite `tools.rs` after the last query: `relocated` moves to line 3.
fn edit_tools_rs_again(repo: &std::path::Path) {
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(
        repo.join("tools.rs"),
        "// a\n// b\nfn relocated() {}\nfn other_fn() {}\n",
    )
    .unwrap();
}

fn force_reindex(home: &std::path::Path, repo: &std::path::Path) {
    let out = session_cmd(home)
        .args(["admin", "index", "--force", "--repo"])
        .arg(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn assert_omits_tools_rs(ctx: &str) {
    assert!(
        ctx.contains("omitted:") && ctx.contains("tools.rs"),
        "{ctx}"
    );
}

/// `<home>/.ecp/<repo>/.hook-refresh/<OVERLAY_SID>~<worktree key>.<ext>` for
/// the one repo dir.
fn hook_refresh_file(home: &std::path::Path, ext: &str) -> Option<std::path::PathBuf> {
    let prefix = format!("{OVERLAY_SID}~");
    let suffix = format!(".{ext}");
    fs::read_dir(home.join(".ecp"))
        .ok()?
        .flatten()
        .filter_map(|e| fs::read_dir(e.path().join(".hook-refresh")).ok())
        .flatten()
        .flatten()
        .map(|f| f.path())
        .find(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && n.ends_with(&suffix)
            })
        })
}

/// Bounded wait (at most 30 s) for the refresh job's outcome marker.
fn wait_for_marker(home: &std::path::Path, ext: &str) -> std::path::PathBuf {
    for _ in 0..300 {
        if let Some(marker) = hook_refresh_file(home, ext) {
            return marker;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let log = hook_refresh_file(home, "log")
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();
    panic!("no .{ext} marker within 30 s; refresh log: {log}");
}

/// Stands for: an edit after the last query makes the reader reject the
/// fragment, and the base graph's HEAD line must not stand in for it. Red on
/// the code before the proof (it printed `relocated (tools.rs:1)`).
#[test]
fn test_hook_hits_edit_after_last_query_moved_function_offers_no_stale_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    edit_tools_rs_again(&repo);

    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(!ctx.contains("relocated (tools.rs:1)"), "{ctx}");
    assert!(!ctx.contains("(tools.rs:25)"), "{ctx}");
    assert_omits_tools_rs(&ctx);
}

/// Stands for: `admin index --force` deletes the overlay, and the rebuilt
/// graph holds HEAD's `vanished` that the working tree no longer has. Red on
/// the code before the proof (it printed `vanished (tools.rs:2)`).
#[test]
fn test_hook_hits_deleted_function_after_force_reindex_is_not_offered() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    force_reindex(&home, &repo);

    let ctx = hook_hits(&home, &repo, "vanished");
    assert!(!ctx.contains("vanished (tools.rs:2)"), "{ctx}");
}

/// Stands for: no ecp query ran since the edit, so there is no session dir.
/// Red on the code before the proof (it printed `relocated (tools.rs:1)`).
#[test]
fn test_hook_hits_no_session_dir_dirty_file_offers_no_committed_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);

    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(!ctx.contains("relocated (tools.rs:1)"), "{ctx}");
    assert_omits_tools_rs(&ctx);
}

#[test]
fn test_hook_hits_no_session_id_anywhere_offers_no_committed_line_and_spawns_nothing() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    let session_dirs = || {
        let mut names: Vec<_> = fs::read_dir(home.join(".ecp"))
            .unwrap()
            .flatten()
            .flat_map(|e| {
                fs::read_dir(e.path().join("sessions"))
                    .into_iter()
                    .flatten()
            })
            .flatten()
            .map(|e| e.file_name())
            .collect();
        names.sort();
        names
    };
    let before = session_dirs();

    // The skip variable would hide a spawn, so this hook runs without it.
    let mut cmd = session_cmd(&home);
    cmd.env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("ECP_SKIP_BG_REBUILD");
    let envelope = grep_envelope(&repo, "relocated".into());
    let ctx = hook_context(spawn_hook_with(cmd, &envelope).wait_with_output().unwrap());

    assert!(!ctx.contains("relocated (tools.rs:1)"), "{ctx}");
    assert_omits_tools_rs(&ctx);
    assert!(hook_refresh_file(&home, "lock").is_none());
    assert!(
        !home
            .join(".ecp")
            .read_dir()
            .unwrap()
            .flatten()
            .any(|e| e.path().join(".hook-refresh").exists()),
        "no session id: no refresh job state"
    );
    assert_eq!(session_dirs(), before, "no new session dir");
}

/// Guards against over-dropping: only the other file is dirty, so the hit
/// from the clean file keeps its committed line.
#[test]
fn test_hook_hits_clean_file_in_dirty_worktree_keeps_committed_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[
            ("tools.rs", "fn relocated() {}\nfn vanished() {}\n".into()),
            ("other.rs", "fn steady() {}\n".into()),
        ],
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(repo.join("other.rs"), "// pad\nfn steady() {}\n").unwrap();
    warm_session_overlay(&home, &repo);

    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(ctx.contains("relocated (tools.rs:1)"), "{ctx}");
    assert!(!ctx.contains("omitted:"), "{ctx}");
}

/// The dirty file is restored to HEAD with no query after: the working tree
/// equals the graph's text, so the committed line is the working-tree line.
#[test]
fn test_hook_hits_dirty_set_became_clean_reports_committed_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    std::thread::sleep(std::time::Duration::from_millis(50));
    git_in(&repo, &["checkout", "--", "tools.rs"]);

    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(ctx.contains("relocated (tools.rs:1)"), "{ctx}");
    assert!(!ctx.contains("omitted:"), "{ctx}");
}

/// HEAD (commit 2) is not indexed, so the hook falls back to commit 1's
/// graph. `tools.rs` changed in commit 2 and is clean in git, yet differs
/// from that graph; `other.rs` did not change. Red on the code before the
/// proof (it printed `relocated (tools.rs:1)`).
#[test]
fn test_hook_hits_head_unindexed_sibling_graph_changed_file_offers_no_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = indexed_repo(
        tmp.path(),
        &[
            ("tools.rs", "fn relocated() {}\nfn vanished() {}\n".into()),
            ("other.rs", "fn steady() {}\n".into()),
        ],
    );
    fs::write(
        repo.join("tools.rs"),
        "// b\n// b\nfn relocated() {}\nfn vanished() {}\n",
    )
    .unwrap();
    git_in(&repo, &["commit", "-qam", "second"]);

    let moved = hook_hits(&home, &repo, "relocated");
    assert!(!moved.contains("relocated (tools.rs:1)"), "{moved}");
    assert_omits_tools_rs(&moved);
    let steady = hook_hits(&home, &repo, "steady");
    assert!(steady.contains("steady (other.rs:1)"), "{steady}");
}

/// An overlay written before a forced rebuild (a newer `.gen.*` graph dir),
/// then an edit: no stale line from the overlay, the old graph or the new one.
#[test]
fn test_hook_hits_overlay_written_by_older_graph_generation_offers_no_stale_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    force_reindex(&home, &repo);
    edit_tools_rs_again(&repo);

    let ctx = hook_hits(&home, &repo, "relocated");
    assert!(!ctx.contains("(tools.rs:1)"), "{ctx}");
    assert!(!ctx.contains("(tools.rs:25)"), "{ctx}");
    assert_omits_tools_rs(&ctx);
}

#[cfg(unix)]
#[test]
fn test_hook_hits_overlay_dir_unreadable_emits_valid_payload_without_stale_line() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    let session_dir = overlay_session_dir(&home, &repo).expect("setup: the session dir exists");
    fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o000)).unwrap();
    let readable_anyway = fs::read_dir(&session_dir).is_ok();

    let out = spawn_hook(&home, &grep_envelope(&repo, "relocated".into()))
        .wait_with_output()
        .unwrap();
    fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o755)).unwrap();
    if readable_anyway {
        return; // root ignores the mode: the directory is not unreadable
    }
    let ctx = hook_context(out);
    assert!(!ctx.contains("relocated (tools.rs:1)"), "{ctx}");
}

/// The hook that drops a hit starts one background `ecp find`; once it
/// finishes, the next hook prints the working-tree line.
#[cfg(unix)]
#[test]
fn test_hook_hits_stale_hit_spawns_refresh_then_next_hook_reports_working_tree_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    edit_tools_rs_again(&repo);
    let refreshing = || {
        let mut cmd = session_cmd(&home);
        cmd.env_remove("ECP_SKIP_BG_REBUILD");
        cmd
    };
    let envelope = grep_envelope(&repo, "relocated".into());

    let first = hook_context(
        spawn_hook_with(refreshing(), &envelope)
            .wait_with_output()
            .unwrap(),
    );
    assert_omits_tools_rs(&first);
    wait_for_marker(&home, "complete");

    let second = hook_context(
        spawn_hook_with(refreshing(), &envelope)
            .wait_with_output()
            .unwrap(),
    );
    assert!(second.contains("relocated (tools.rs:3)"), "{second}");
    assert!(!second.contains("omitted:"), "{second}");
}

/// Two hooks at once on the stale state: neither prints a stale line, and
/// the flock lets one refresh run. (The two outputs may differ if the refresh
/// lands between them, so they are not compared.)
#[cfg(unix)]
#[test]
fn test_hook_hits_concurrent_stale_hooks_spawn_at_most_one_refresh() {
    let tmp = tempdir().unwrap();
    let (home, repo) = overlay_hook_repo(tmp.path());
    dirty_tools_rs(&repo);
    warm_session_overlay(&home, &repo);
    edit_tools_rs_again(&repo);
    let refreshing = || {
        let mut cmd = session_cmd(&home);
        cmd.env_remove("ECP_SKIP_BG_REBUILD");
        cmd
    };
    let envelope = grep_envelope(&repo, "relocated".into());

    let (a, b) = (
        spawn_hook_with(refreshing(), &envelope),
        spawn_hook_with(refreshing(), &envelope),
    );
    for ctx in [
        hook_context(a.wait_with_output().unwrap()),
        hook_context(b.wait_with_output().unwrap()),
    ] {
        assert!(!ctx.contains("(tools.rs:1)"), "{ctx}");
        assert!(!ctx.contains("(tools.rs:25)"), "{ctx}");
    }
    let log = fs::read_to_string(wait_for_marker(&home, "log")).unwrap_or_default();
    wait_for_marker(&home, "complete");
    let log_after = fs::read_to_string(hook_refresh_file(&home, "log").unwrap()).unwrap();
    assert!(
        log_after.matches("=== attempt").count() <= 1,
        "{log}\n{log_after}"
    );
}

/// One file per mainstream language, each edited after indexing: the proof is
/// byte-level, so no language's committed line survives an edit.
#[test]
fn test_hook_hits_dirty_files_in_14_languages_offer_no_committed_line() {
    let langs: [(&str, &str); 14] = [
        ("l_ts.ts", "export function langts() { return 1; }\n"),
        ("l_js.js", "function langjs() { return 1; }\n"),
        ("l_py.py", "def langpy():\n    return 1\n"),
        ("L_Java.java", "class LJava {\n    void langjava() {}\n}\n"),
        ("l_kt.kt", "fun langkt() {}\n"),
        ("l_cs.cs", "class LCs {\n    void langcs() {}\n}\n"),
        ("l_go.go", "package main\n\nfunc langgo() {}\n"),
        ("l_rs.rs", "fn langrs() {}\n"),
        ("l_php.php", "<?php\nfunction langphp() {}\n"),
        ("l_rb.rb", "def langrb\n  1\nend\n"),
        ("l_sw.swift", "func langswift() {}\n"),
        ("l_c.c", "int langc(void) { return 1; }\n"),
        ("l_cpp.cpp", "int langcpp() { return 1; }\n"),
        ("l_dart.dart", "void langdart() {}\n"),
    ];
    let tmp = tempdir().unwrap();
    let files: Vec<(&str, String)> = langs.iter().map(|(f, src)| (*f, src.to_string())).collect();
    let (home, repo) = indexed_repo(tmp.path(), &files);
    let names = [
        "langts",
        "langjs",
        "langpy",
        "langjava",
        "langkt",
        "langcs",
        "langgo",
        "langrs",
        "langphp",
        "langrb",
        "langswift",
        "langc",
        "langcpp",
        "langdart",
    ];
    let offered_before = names
        .iter()
        .filter(|name| hook_hits(&home, &repo, name).contains(&format!("{name} (")))
        .count();
    assert!(
        offered_before >= 7,
        "control: the clean worktree offers most symbols, got {offered_before}"
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    for (file, source) in langs {
        fs::write(repo.join(file), format!("// edited\n// again\n{source}")).unwrap();
    }
    for name in names {
        let ctx = hook_hits(&home, &repo, name);
        assert!(!ctx.contains(&format!("{name} (")), "{name}: {ctx}");
    }
}

// ── Sibling worktrees under one session id ──────────────────────────────────
//
// Contract: each (session, worktree) pair has its own overlay dir, so a query
// in one worktree never retires, rebases or serves another worktree's
// overlay. The pass/fail criterion above holds: a hit names the line the
// hook's own working tree holds.

const APP_PY: &str = "def keep_me():\n    return 1\n\n\ndef doomed_fn():\n    return 2\n";
const LIB_PY: &str = "def helper_b():\n    return 3\n";

fn sibling_hook_repo(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    indexed_repo(tmp, &[("app.py", APP_PY.into()), ("lib.py", LIB_PY.into())])
}

/// `git worktree add` of a new branch `name` beside `repo`.
fn linked_worktree(repo: &std::path::Path, name: &str) -> std::path::PathBuf {
    let worktree = repo.parent().unwrap().join(name);
    git_in(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            worktree.to_str().unwrap(),
        ],
    );
    worktree
}

/// One `ecp find` in `worktree` under the shared session; returns its stderr.
fn query_in(home: &std::path::Path, worktree: &std::path::Path, pattern: &str) -> String {
    let out = session_cmd(home)
        .args(["find", pattern, "--repo"])
        .arg(worktree)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn pad(lines: usize, source: &str) -> String {
    format!("{}{source}", "# pad\n".repeat(lines))
}

#[test]
fn test_hook_hits_two_worktrees_one_session_each_reports_own_working_tree_line() {
    let tmp = tempdir().unwrap();
    let (home, repo) = sibling_hook_repo(tmp.path());
    let wt_b = linked_worktree(&repo, "wt-b");
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(
        repo.join("app.py"),
        pad(3, "def keep_me():\n    return 1\n"),
    )
    .unwrap();
    fs::write(wt_b.join("lib.py"), pad(2, LIB_PY)).unwrap();
    query_in(&home, &repo, "keep_me");
    query_in(&home, &wt_b, "helper_b");

    let in_a = hook_hits(&home, &repo, "keep_me");
    assert!(in_a.contains("keep_me (app.py:4)"), "{in_a}");
    let deleted_in_a = hook_hits(&home, &repo, "doomed_fn");
    assert!(
        !deleted_in_a.contains("doomed_fn (app.py:5)"),
        "{deleted_in_a}"
    );
    let in_b = hook_hits(&home, &wt_b, "helper_b");
    assert!(in_b.contains("helper_b (lib.py:3)"), "{in_b}");
    let a_sees_b_clean_file = hook_hits(&home, &repo, "helper_b");
    assert!(
        a_sees_b_clean_file.contains("helper_b (lib.py:1)"),
        "A's lib.py is clean: {a_sees_b_clean_file}"
    );
    let b_sees_a_clean_file = hook_hits(&home, &wt_b, "keep_me");
    assert!(
        b_sees_a_clean_file.contains("keep_me (app.py:1)"),
        "B's app.py is clean: {b_sees_a_clean_file}"
    );
}

#[test]
fn test_hook_hits_sibling_worktree_on_other_commit_keeps_own_overlay() {
    let tmp = tempdir().unwrap();
    let (home, repo) = sibling_hook_repo(tmp.path());
    let wt_b = linked_worktree(&repo, "wt-b");
    fs::write(wt_b.join("lib.py"), pad(2, LIB_PY)).unwrap();
    git_in(&wt_b, &["commit", "-qam", "b1"]);
    let indexed = Command::new(ecp_bin())
        .args(["admin", "index", "--repo"])
        .arg(&wt_b)
        .env("HOME", &home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env_remove("ECP_HOME")
        .output()
        .unwrap();
    assert!(indexed.status.success());
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(
        repo.join("app.py"),
        pad(3, "def keep_me():\n    return 1\n"),
    )
    .unwrap();
    let a_first = query_in(&home, &repo, "keep_me");
    fs::write(wt_b.join("lib.py"), pad(4, LIB_PY)).unwrap();
    let b_first = query_in(&home, &wt_b, "helper_b");

    let in_a = hook_hits(&home, &repo, "keep_me");
    assert!(in_a.contains("keep_me (app.py:4)"), "{in_a}");
    let a_again = query_in(&home, &repo, "keep_me");
    let in_b = hook_hits(&home, &wt_b, "helper_b");
    assert!(in_b.contains("helper_b (lib.py:5)"), "{in_b}");
    let in_a_after = hook_hits(&home, &repo, "keep_me");
    assert!(in_a_after.contains("keep_me (app.py:4)"), "{in_a_after}");
    for stderr in [&a_first, &b_first, &a_again] {
        assert!(
            !stderr.contains("session.promoted") && !stderr.contains("session.rebased"),
            "a query promoted or rebased a sibling worktree's overlay: {stderr}"
        );
    }
}

#[test]
fn test_hook_hits_equal_mtime_sibling_file_does_not_read_other_worktree_fragment() {
    let tmp = tempdir().unwrap();
    let (home, repo) = sibling_hook_repo(tmp.path());
    let wt_b = linked_worktree(&repo, "wt-b");
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(
        repo.join("app.py"),
        pad(3, "def keep_me():\n    return 1\n"),
    )
    .unwrap();
    query_in(&home, &repo, "keep_me");
    // `cp -p`, `rsync -a` and a coarse-mtime filesystem give B's file A's mtime.
    let mtime = fs::metadata(repo.join("app.py"))
        .unwrap()
        .modified()
        .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(wt_b.join("app.py"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    let kept = hook_hits(&home, &wt_b, "keep_me");
    assert!(kept.contains("keep_me (app.py:1)"), "{kept}");
    let deleted_in_a_only = hook_hits(&home, &wt_b, "doomed_fn");
    assert!(
        deleted_in_a_only.contains("doomed_fn (app.py:5)"),
        "B's app.py still holds doomed_fn: {deleted_in_a_only}"
    );
}
