//! PostToolUse handler: detect git ref-changing commands, kick off a
//! detached background reindex when the index is stale.

use super::common::{emit_additional_context, gitnexus_dir, strip_shell_quotes, HookInput};
use crate::auto_ensure::{ensure_index, EnsureResult};
use graph_nexus_core::GnxError;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// Git-mutation matcher. Compiled once per process — PostToolUse fires
/// on every Bash tool call so amortising the regex build matters.
fn git_mutation_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\bgit\s+(commit|merge|rebase|cherry-pick|pull)(\s|$)").unwrap()
    })
}

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    if input.tool_name != "Bash" {
        return Ok(());
    }
    let cmd = input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !is_git_mutation(cmd) {
        return Ok(());
    }
    let exit = input
        .tool_output
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if exit != 0 {
        return Ok(());
    }

    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };
    let repo_root = match gnx_dir.parent() {
        Some(p) => p.to_path_buf(),
        None => return Ok(()),
    };
    let graph_path = gnx_dir.join("graph.bin");

    let result = ensure_index(&graph_path, &repo_root).unwrap_or(EnsureResult::Missing);
    let age = match result {
        EnsureResult::Stale { age_seconds } => age_seconds,
        _ => return Ok(()),
    };

    if !spawn_background_reindex(&repo_root, &gnx_dir) {
        return Ok(());
    }

    emit_additional_context(
        "PostToolUse",
        &format!(
            "gnx reindex started in background (index stale ~{age}s). Subsequent gnx tools may use stale data until completion (~30-120s). If it appears stuck, run `gnx admin index` manually."
        ),
    );
    Ok(())
}

fn is_git_mutation(cmd: &str) -> bool {
    git_mutation_re().is_match(&strip_shell_quotes(cmd))
}

/// Detached background `gnx admin index` under flock at
/// `<gnx_dir>/.analyze.lock`. Writes `.rebuild-complete` on success
/// or `.rebuild-failed` after MAX=3 attempts. Returns true iff the
/// launcher subprocess was spawned (the analyze outcome surfaces
/// asynchronously via marker files consumed by UserPromptSubmit).
fn spawn_background_reindex(repo_root: &Path, gnx_dir: &Path) -> bool {
    let lock = gnx_dir.join(".analyze.lock");
    let complete = gnx_dir.join(".rebuild-complete");
    let failed = gnx_dir.join(".rebuild-failed");
    let log = gnx_dir.join("last-rebuild.log");
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let shell = format!(
        r#"exec 9>{lock} || exit 0
flock -n 9 || exit 0
: > {log}
MAX=3; ATTEMPT=0
while [ $ATTEMPT -lt $MAX ]; do
  ATTEMPT=$((ATTEMPT+1))
  echo "=== attempt $ATTEMPT/$MAX ===" >> {log}
  if {gnx} admin index >> {log} 2>&1; then
    rm -f {failed}
    : > {complete}
    exit 0
  fi
  [ $ATTEMPT -lt $MAX ] && sleep 2
done
rm -f {complete}
: > {failed}
"#,
        lock = shell_quote(&lock),
        log = shell_quote(&log),
        gnx = shell_quote(&self_exe),
        complete = shell_quote(&complete),
        failed = shell_quote(&failed),
    );

    Command::new("sh")
        .arg("-c")
        .arg(&shell)
        .current_dir(repo_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

fn shell_quote<P: AsRef<Path>>(p: P) -> String {
    let s = p.as_ref().to_string_lossy().to_string();
    let escaped = s.replace('\'', r"'\''");
    format!("'{}'", escaped)
}
