//! `git status --porcelain -z --untracked-files=all`, the dirty-set query the
//! freshness gate runs on every graph query.
//!
//! On Linux the answer may come from a private copy of the index that keeps
//! git's untracked cache (see `private_index`). Everywhere else, and wherever
//! that copy cannot serve exactly, it comes from the plain command.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
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
pub(crate) type EnvOverrides = [(OsString, Option<OsString>)];

/// `key` as the git this module spawns sees it.
fn env_var(env: &EnvOverrides, key: &str) -> Option<OsString> {
    match env.iter().rev().find(|(k, _)| k == key) {
        Some((_, value)) => value.clone(),
        None => std::env::var_os(key),
    }
}

/// Variables that redirect git away from the on-disk layout
/// `git_layout_unchecked` reads, the index file included.
const LAYOUT_ENV: [&str; 5] = [
    "GIT_INDEX_FILE",
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_CEILING_DIRECTORIES",
];

/// The output of `git status --porcelain -z --untracked-files=all` in
/// `worktree`, byte for byte.
pub fn porcelain_all(worktree: &Path, home_ecp: &Path) -> io::Result<Output> {
    porcelain_all_in(worktree, &home_ecp.join(PRIVATE_INDEX_DIR), &[])
}

/// Every git status this runs, the private index's fallback included, carries
/// one set of filter overrides; when they cannot be built, no status runs.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn porcelain_all_in(worktree: &Path, cache_root: &Path, env: &EnvOverrides) -> io::Result<Output> {
    let index = top_index(worktree, env);
    let overrides = overrides_with(worktree, env, index.as_ref())?;
    #[cfg(target_os = "linux")]
    if let Some(out) = private_index::status(worktree, cache_root, env, &overrides, index.as_ref())
    {
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
/// `-c` settings on. A `git diff` that reads the worktree does the same.
pub(crate) fn status_overrides(worktree: &Path, env: &EnvOverrides) -> io::Result<FilterOverrides> {
    overrides_with(worktree, env, top_index(worktree, env).as_ref())
}

/// [`status_overrides`] with the top level's index already read.
///
/// Each directory costs a `git config`. It costs a `git ls-files` as well
/// only when its index file does not show it free of gitlinks; that spawn
/// was most of the cost in a repository without submodules.
fn overrides_with(
    worktree: &Path,
    env: &EnvOverrides,
    top: Option<&IndexRead>,
) -> io::Result<FilterOverrides> {
    let mut overrides = FilterOverrides::default();
    // A gitlink reached again through a symlink would otherwise loop.
    let mut seen: HashSet<PathBuf> = fs::canonicalize(worktree).into_iter().collect();
    let mut level = vec![(worktree.to_path_buf(), false)];
    while !level.is_empty() {
        // Every child is reaped before any result is judged, so an early error
        // leaves no git running unwaited.
        let spawned: Vec<_> = level
            .iter()
            .map(|(dir, submodule)| {
                let config = probe(dir, env, *submodule, &safe_exec::FILTER_KEYS_ARGS);
                let no_gitlink = if *submodule {
                    submodule_index(dir).is_some_and(|index| index.lists_no_gitlink())
                } else {
                    top.is_some_and(IndexRead::lists_no_gitlink)
                };
                let files = (!no_gitlink).then(|| probe(dir, env, *submodule, &LS_FILES_ARGS));
                (config, files)
            })
            .collect();
        let finished: Vec<_> = spawned
            .into_iter()
            .map(|(config, files)| (finish(config), files.map(finish)))
            .collect();
        let mut next = Vec::new();
        for ((dir, _), (config, files)) in level.iter().zip(finished) {
            overrides.merge(FilterOverrides::from_config_output(&config?)?);
            let Some(files) = files else {
                continue;
            };
            next.extend(
                initialized_gitlinks(dir, &files?)?
                    .into_iter()
                    .filter_map(|sub| fs::canonicalize(sub).ok())
                    .filter(|sub| seen.insert(sub.clone()))
                    .map(|sub| (sub, true)),
            );
        }
        level = next;
    }
    Ok(overrides)
}

/// One repository's index file, read once per status operation: the gitlink
/// scan reads it, and the private index copies the same bytes.
struct IndexRead {
    gitdir: PathBuf,
    common: PathBuf,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    meta: fs::Metadata,
    body: Vec<u8>,
}

impl IndexRead {
    fn read(gitdir: PathBuf, common: PathBuf) -> Option<Self> {
        let mut file = File::open(gitdir.join("index")).ok()?;
        let meta = file.metadata().ok()?;
        let mut body = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut body).ok()?;
        Some(Self {
            gitdir,
            common,
            meta,
            body,
        })
    }

    /// True when this file alone shows that git sees no gitlink. A split
    /// index keeps entries in a `sharedindex.*` file the scan does not read,
    /// and a SHA-256 repository widens the object names the scan steps over;
    /// git reads `extensions.objectFormat` from the common config file alone.
    fn lists_no_gitlink(&self) -> bool {
        !split_index_present(&self.gitdir)
            && fs::read(self.common.join("config"))
                .is_ok_and(|config| !config_mentions(&config, &["objectformat"]))
            && gitlink_free(&self.body)
    }
}

/// The index a status in `worktree` reads, or `None` when the environment
/// moves git off the on-disk layout or the file cannot be read.
fn top_index(worktree: &Path, env: &EnvOverrides) -> Option<IndexRead> {
    if LAYOUT_ENV.iter().any(|key| env_var(env, key).is_some()) {
        return None;
    }
    let (_, gitdir, common) = crate::git_cache::git_layout_unchecked(worktree)?;
    IndexRead::read(gitdir, common)
}

/// The index a status in the submodule at `dir` (canonical) reads. git runs
/// it with `GIT_DIR=.git`, so a layout found in an ancestor is not that one.
fn submodule_index(dir: &Path) -> Option<IndexRead> {
    let (top, gitdir, common) = crate::git_cache::git_layout_unchecked(dir)?;
    if top != dir {
        return None;
    }
    IndexRead::read(gitdir, common)
}

/// True when `gitdir` may hold a split index's `sharedindex.*` file, an
/// unreadable directory included.
fn split_index_present(gitdir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(gitdir) else {
        return true;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .as_encoded_bytes()
            .starts_with(b"sharedindex.")
    })
}

/// True when the index's entries were walked, none is a gitlink, and the
/// extensions after them end exactly at the 20-byte checksum. Entries follow
/// a 12-byte header; each holds stat data with the mode at byte 24, a 20-byte
/// object name, 16 bits of flags (32 with the extended bit), and the path.
/// Versions 2 and 3 NUL-pad the entry to a multiple of eight bytes; version 4
/// prefixes the path with a varint and ends it with one NUL. The end check
/// rejects a layout the walk does not model, such as the 32-byte object
/// names of a SHA-256 repository, which would otherwise step past a gitlink.
fn gitlink_free(index: &[u8]) -> bool {
    const GITLINK: u32 = 0o160_000;
    const EXTENDED: u16 = 0x4000;
    const FLAGS_AT: usize = 40 + 20;
    const CHECKSUM: usize = 20;
    let u32_at = |at: usize| {
        let bytes = index.get(at..at.checked_add(4)?)?;
        Some(u32::from_be_bytes(bytes.try_into().ok()?))
    };
    let (Some(b"DIRC"), Some(version @ 2..=4), Some(count)) =
        (index.get(..4), u32_at(4), u32_at(8))
    else {
        return false;
    };
    let mut at = 12;
    for _ in 0..count {
        let (Some(mode), Some(&[hi, lo])) =
            (u32_at(at + 24), index.get(at + FLAGS_AT..at + FLAGS_AT + 2))
        else {
            return false;
        };
        if mode & 0o170_000 == GITLINK {
            return false;
        }
        let mut path = at + FLAGS_AT + 2;
        if u16::from_be_bytes([hi, lo]) & EXTENDED != 0 {
            path += 2;
        }
        if version == 4 {
            while index.get(path).is_some_and(|b| b & 0x80 != 0) {
                path += 1;
            }
            path += 1;
        }
        let Some(len) = index
            .get(path..)
            .and_then(|rest| rest.iter().position(|&b| b == 0))
        else {
            return false;
        };
        at = match version {
            4 => path + len + 1,
            _ => at + ((path - at + len + 8) & !7),
        };
    }
    // Each extension is a 4-byte signature, a 32-bit size and its data.
    let Some(end) = index.len().checked_sub(CHECKSUM) else {
        return false;
    };
    while at < end {
        let Some(size) = u32_at(at + 4) else {
            return false;
        };
        at = at.saturating_add(8).saturating_add(size as usize);
    }
    at == end
}

/// True when `config` opens an include section or sets one of `keys`
/// (lowercase). Keys are matched by name in any section, which only ever
/// over-matches. git skips a UTF-8 byte order mark at the start of the file.
fn config_mentions(config: &[u8], keys: &[&str]) -> bool {
    let text = String::from_utf8_lossy(config);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.lines().any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        let rest = match line.strip_prefix('[') {
            Some(header) if header.starts_with("include") => return true,
            Some(header) => header.split_once(']').map_or("", |(_, rest)| rest),
            None => line.as_str(),
        };
        let key = rest
            .trim_start()
            .split(|c: char| c == '=' || c.is_ascii_whitespace())
            .next()
            .unwrap_or("");
        keys.contains(&key)
    })
}

fn probe(dir: &Path, env: &EnvOverrides, submodule: bool, args: &[&str]) -> io::Result<Child> {
    let mut cmd = safe_exec::git();
    with_env(&mut cmd, env);
    // The config read must see what the status sees; see
    // `safe_exec::filter_overrides`. No other probe reads the variable.
    cmd.env_remove("GIT_CONFIG");
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
