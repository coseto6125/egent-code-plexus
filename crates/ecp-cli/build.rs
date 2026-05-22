//! Build script for `ecp-cli`.
//!
//! Embeds the current git short-SHA into the binary at compile time as
//! `ECP_GIT_SHA`. The runtime reads it via `env!()` / `option_env!()` to:
//!   * Display the version string (`ecp --version`) as `<semver>+<sha>`.
//!   * Persist the SHA into `CommitBuildMeta.binary_commit_sha` so callers
//!     can detect when the graph was built by a different binary revision.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Resolve the actual git common directory. In a worktree (e.g.
    // `.claude/worktrees/<topic>/`) `.git` at the worktree root is a text
    // file pointing into the main repo's `.git/worktrees/<topic>/`, NOT
    // a directory — so a hard-coded `../../.git/HEAD` path is missing,
    // and Cargo treats missing rerun-if-changed paths as "always rerun",
    // making this build script execute on every incremental build.
    if let Some(common_dir) = git_output(&["rev-parse", "--git-common-dir"]) {
        println!("cargo:rerun-if-changed={common_dir}/HEAD");
        println!("cargo:rerun-if-changed={common_dir}/refs/heads");
    }

    let sha =
        git_output(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ECP_GIT_SHA={sha}");
}

fn git_output(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
