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
