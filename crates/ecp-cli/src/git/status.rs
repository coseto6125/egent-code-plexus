//! `git status --porcelain -z --untracked-files=all`, the dirty-set query the
//! freshness gate runs on every graph query.
//!
//! On Linux the answer may come from a private copy of the index that keeps
//! git's untracked cache (see `private_index`). Everywhere else, and wherever
//! that copy cannot serve exactly, it comes from the plain command.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Command, Output};

use crate::git::safe_exec;

#[cfg(target_os = "linux")]
mod private_index;
#[cfg(target_os = "linux")]
pub use private_index::sweep_orphans;

/// `-z` (NUL-terminated) avoids git's C-quoting of paths with spaces or
/// non-ASCII bytes, which the human-readable format would otherwise wrap in
/// double quotes and escape — producing a path that doesn't exist on disk and
/// silently dropping that file from the incremental refresh.
///
/// `--untracked-files=all`: untracked files must enter the dirty set or a
/// brand-new file (the most common agent edit: Write, then query) is invisible
/// to the L1 overlay and `found:false` reads as a definitive "does not exist"
/// (FU-2026-06-10-8b98d5e991a6). `all` rather than `normal` because `normal`
/// collapses a new directory to one `dir/` entry, hiding the files inside it
/// from `reanalyze_files`. Gitignored files stay excluded — porcelain honours
/// .gitignore, so scratch dirs cost nothing.
const STATUS_ARGS: [&str; 4] = ["status", "--porcelain", "-z", "--untracked-files=all"];

const PRIVATE_INDEX_DIR: &str = "git-index";

/// Variables set (`Some`) or removed (`None`) for every git this module runs,
/// in order, and read in their place. Production passes none; tests pass a
/// set that keeps the developer's config away from git.
type EnvOverrides = [(OsString, Option<OsString>)];

/// The output of `git status --porcelain -z --untracked-files=all` in
/// `worktree`, byte for byte.
pub fn porcelain_all(worktree: &Path, home_ecp: &Path) -> io::Result<Output> {
    porcelain_all_in(worktree, &home_ecp.join(PRIVATE_INDEX_DIR), &[])
}

#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn porcelain_all_in(worktree: &Path, cache_root: &Path, env: &EnvOverrides) -> io::Result<Output> {
    #[cfg(target_os = "linux")]
    if let Some(out) = private_index::status(worktree, cache_root, env) {
        return Ok(out);
    }
    plain_in(worktree, env)
}

/// [`porcelain_all`] from the plain command alone.
pub fn plain(worktree: &Path) -> io::Result<Output> {
    plain_in(worktree, &[])
}

fn plain_in(worktree: &Path, env: &EnvOverrides) -> io::Result<Output> {
    with_env(&mut safe_exec::git(), env)
        .args(STATUS_ARGS)
        .current_dir(worktree)
        .output()
}

fn with_env<'a>(cmd: &'a mut Command, env: &EnvOverrides) -> &'a mut Command {
    for (key, value) in env {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    cmd
}
