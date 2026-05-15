//! SessionStart handler: render rules template (in repo or
//! `~/.claude/hooks/gnx/rules.md`), substitute placeholders from
//! `.gitnexus-rs/meta.json`, surface a worktree-needs-index hint when
//! `cwd` is a git worktree without an index.

use super::common::{emit_additional_context, gitnexus_dir, HookInput};
use graph_nexus_core::GnxError;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    if input.cwd.is_empty() {
        return Ok(());
    }
    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => {
            if let Some(hint) = detect_worktree_needing_index(Path::new(&input.cwd)) {
                emit_additional_context("SessionStart", &hint);
            }
            return Ok(());
        }
    };

    let rendered = render_rules(Path::new(&input.cwd), &gnx_dir);
    if !rendered.trim().is_empty() {
        emit_additional_context("SessionStart", &rendered);
    }
    Ok(())
}

fn render_rules(repo_root: &Path, gnx_dir: &Path) -> String {
    let template = match load_template(repo_root) {
        Some(t) => t,
        None => return String::new(),
    };
    let (nodes, edges, head) = read_stats(gnx_dir, repo_root);
    let has_graphify = repo_root.join("graphify-out").exists();
    let has_wiki = has_graphify
        && repo_root
            .join("graphify-out")
            .join("wiki")
            .join("index.md")
            .exists();

    let mut out = template
        .replace("{{stats.nodes}}", &nodes)
        .replace("{{stats.edges}}", &edges)
        .replace("{{head}}", &head);
    out = render_conditional(&out, "wiki", has_wiki);
    out = render_conditional(&out, "graphify", has_graphify);
    out.trim().to_string()
}

fn load_template(repo_root: &Path) -> Option<String> {
    let candidates = [
        repo_root.join(".claude").join("gnx-rules.md"),
        home_dir()
            .join(".claude")
            .join("hooks")
            .join("gnx")
            .join("rules.md"),
    ];
    for c in candidates {
        if let Ok(s) = fs::read_to_string(&c) {
            return Some(s);
        }
    }
    None
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn read_stats(gnx_dir: &Path, repo_root: &Path) -> (String, String, String) {
    let mut nodes = "?".to_string();
    let mut edges = "?".to_string();
    if let Ok(raw) = fs::read_to_string(gnx_dir.join("meta.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(n) = v.get("node_count").and_then(|x| x.as_u64()) {
                nodes = n.to_string();
            }
            if let Some(e) = v.get("edge_count").and_then(|x| x.as_u64()) {
                edges = e.to_string();
            }
        }
    }
    let head = git_head_short(repo_root).unwrap_or_else(|| "?".into());
    (nodes, edges, head)
}

fn git_head_short(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn render_conditional(text: &str, key: &str, keep: bool) -> String {
    let open = format!("{{{{#if {}}}}}", key);
    let close = "{{/if}}";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + open.len()..];
        let end = match after_open.find(close) {
            Some(e) => e,
            None => break,
        };
        if keep {
            out.push_str(&after_open[..end]);
        }
        rest = &after_open[end + close.len()..];
    }
    out.push_str(rest);
    out
}

fn detect_worktree_needing_index(cwd: &Path) -> Option<String> {
    let toplevel = git_rev_parse(cwd, &["rev-parse", "--show-toplevel"])?;
    let git_path = Path::new(&toplevel).join(".git");
    if !git_path.is_file() {
        return None;
    }
    if Path::new(&toplevel).join(".gitnexus-rs").exists() {
        return None;
    }
    let branch =
        git_rev_parse(Path::new(&toplevel), &["branch", "--show-current"]).unwrap_or_default();
    let base = Path::new(&toplevel)
        .file_name()?
        .to_string_lossy()
        .to_string();
    Some(format!(
        "gnx index missing in this worktree ({base} @ {branch}). Run `gnx admin index` to index it."
    ))
}

fn git_rev_parse(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
