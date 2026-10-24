//! `git status --porcelain -z --untracked-files=all`, the dirty-set query the
//! freshness gate runs on every graph query.
//!
//! On Linux the answer may come from a private copy of the index that keeps
//! git's untracked cache (see `private_index`). Everywhere else, and wherever
//! that copy cannot serve exactly, it comes from the plain command.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use crate::git::safe_exec::{self, FilterOverrides};

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

/// Every git status this runs, the private index's fallback included, carries
/// one set of filter overrides; when they cannot be built, no status runs.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn porcelain_all_in(worktree: &Path, cache_root: &Path, env: &EnvOverrides) -> io::Result<Output> {
    let overrides = status_overrides(worktree, env)?;
    #[cfg(target_os = "linux")]
    if let Some(out) = private_index::status(worktree, cache_root, env, &overrides) {
        return Ok(out);
    }
    plain_with(worktree, env, &overrides)
}

/// [`porcelain_all`] from the plain command alone.
pub fn plain(worktree: &Path) -> io::Result<Output> {
    plain_in(worktree, &[])
}

fn plain_in(worktree: &Path, env: &EnvOverrides) -> io::Result<Output> {
    plain_with(worktree, env, &status_overrides(worktree, env)?)
}

fn plain_with(
    worktree: &Path,
    env: &EnvOverrides,
    overrides: &FilterOverrides,
) -> io::Result<Output> {
    with_env(&mut safe_exec::git_with_overrides(worktree, overrides), env)
        .args(STATUS_ARGS)
        .output()
}

/// Lists the index entries, gitlinks among them, across the whole tree: git
/// status reports every path whichever subdirectory it runs in.
const LS_FILES_ARGS: [&str; 5] = ["ls-files", "-s", "-z", "--", ":/"];

/// What git unsets before it runs a status inside a submodule
/// (`prepare_submodule_repo_env`, `local_repo_env` in git 2.43). It keeps
/// `GIT_CONFIG_PARAMETERS` and `GIT_CONFIG_COUNT`, so the `-c` overrides of
/// the parent reach the submodule, and sets `GIT_DIR=.git`.
const SUBMODULE_CLEARED_ENV: [&str; 13] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// The filter overrides a status in `worktree` needs: the drivers git
/// resolves there, and in every initialized submodule below it, recursively.
/// The parent status runs a status in each submodule whose `.git` resolves,
/// which reads that submodule's own config and attributes, and passes its own
/// `-c` settings on.
///
/// Each directory costs a `git config` and a `git ls-files`, spawned together
/// so a repository without submodules waits for one process, not two.
fn status_overrides(worktree: &Path, env: &EnvOverrides) -> io::Result<FilterOverrides> {
    let mut overrides = FilterOverrides::default();
    // A gitlink reached again through a symlink would otherwise loop.
    let mut seen: HashSet<PathBuf> = std::fs::canonicalize(worktree).into_iter().collect();
    let mut level = vec![(worktree.to_path_buf(), false)];
    while !level.is_empty() {
        // Every child is reaped before any result is judged, so an early error
        // leaves no git running unwaited.
        let spawned: Vec<_> = level
            .iter()
            .map(|(dir, submodule)| {
                let config = probe(dir, env, *submodule, &safe_exec::FILTER_KEYS_ARGS);
                let files = probe(dir, env, *submodule, &LS_FILES_ARGS);
                (config, files)
            })
            .collect();
        let finished: Vec<_> = spawned
            .into_iter()
            .map(|(config, files)| (finish(config), finish(files)))
            .collect();
        let mut next = Vec::new();
        for ((dir, _), (config, files)) in level.iter().zip(finished) {
            overrides.merge(FilterOverrides::from_config_output(&config?)?);
            next.extend(
                initialized_gitlinks(dir, &files?)?
                    .into_iter()
                    .filter(|sub| std::fs::canonicalize(sub).is_ok_and(|c| seen.insert(c)))
                    .map(|sub| (sub, true)),
            );
        }
        level = next;
    }
    Ok(overrides)
}

fn probe(dir: &Path, env: &EnvOverrides, submodule: bool, args: &[&str]) -> io::Result<Child> {
    let mut cmd = safe_exec::git();
    with_env(&mut cmd, env);
    if submodule {
        for key in SUBMODULE_CLEARED_ENV {
            cmd.env_remove(key);
        }
        cmd.env("GIT_DIR", ".git");
    }
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

fn finish(child: io::Result<Child>) -> io::Result<Output> {
    child?.wait_with_output()
}

/// The gitlinks in `ls-files -s -z` output, under `dir`, whose `.git` exists.
/// git skips a gitlink without one as not checked out, and stops the status
/// on a `.git` that does not resolve, so the config read there fails too.
fn initialized_gitlinks(dir: &Path, ls_files: &Output) -> io::Result<Vec<PathBuf>> {
    if !ls_files.status.success() {
        return Err(io::Error::other(format!(
            "git ls-files could not list the gitlinks ({}): {}",
            ls_files.status,
            String::from_utf8_lossy(&ls_files.stderr).trim()
        )));
    }
    let mut paths = ls_files
        .stdout
        .split(|&b| b == 0)
        .filter(|entry| entry.starts_with(b"160000 "))
        .filter_map(|entry| {
            let tab = entry.iter().position(|&b| b == b'\t')?;
            Some(entry[tab + 1..].to_vec())
        })
        .map(|rel| safe_exec::os_string_from_git(rel).map(|rel| dir.join(rel)))
        .collect::<io::Result<Vec<_>>>()?;
    // A conflicted gitlink is listed once per stage.
    paths.sort_unstable();
    paths.dedup();
    paths.retain(|path| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
            && std::fs::symlink_metadata(path.join(".git")).is_ok()
    });
    Ok(paths)
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
