//! `--repo` selector grammar parser.
//!
//! selector := atom | atom,atom,...
//! atom     := <path> | <name> | @<group> | @all
//!
//! Resolution to actual repo paths is done by `resolve()` (Task 0.4),
//! which needs registry access; this module only handles syntax.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Atom {
    /// No --repo flag given; resolves to current working directory.
    Cwd,
    /// `.` / `./rel` / `/abs/path` — looked up in registry by canonical path.
    Path(PathBuf),
    /// Registry name (no `@` prefix).
    Name(String),
    /// `@<group>` — expand via `RepoEntry.groups` membership.
    Group(String),
    /// `@all` — every registered repo.
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector(pub Vec<Atom>);

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("empty atom in --repo (consecutive commas?)")]
    EmptyAtom,
    #[error("`@` without name in --repo")]
    AtWithoutName,
}

pub fn parse(s: &str) -> Result<Selector, ParseError> {
    if s.is_empty() {
        return Ok(Selector(vec![Atom::Cwd]));
    }
    let atoms: Result<Vec<Atom>, _> = s.split(',').map(parse_atom).collect();
    Ok(Selector(atoms?))
}

fn parse_atom(part: &str) -> Result<Atom, ParseError> {
    if part.is_empty() {
        return Err(ParseError::EmptyAtom);
    }
    if let Some(rest) = part.strip_prefix('@') {
        if rest.is_empty() {
            return Err(ParseError::AtWithoutName);
        }
        if rest == "all" {
            return Ok(Atom::All);
        }
        return Ok(Atom::Group(rest.to_string()));
    }
    // Heuristic: anything containing '/' or starting with '.' is a path;
    // otherwise treat as registry name. Path canonicalization happens in
    // the resolver, not here.
    if part.starts_with('.') || part.starts_with('/') {
        return Ok(Atom::Path(PathBuf::from(part)));
    }
    Ok(Atom::Name(part.to_string()))
}

// ── Resolver ────────────────────────────────────────────────────────────────

use crate::git::safe_exec;
use cgn_core::registry::{RegistryFile, RepoAlias};
use std::collections::HashSet;

/// A repo resolved from a selector atom — derived from a v2 `RepoAlias`,
/// carrying the alias's stable `dir_name` (which is the `<repo>/` segment
/// under `~/.gnx/`) and the canonical git common-dir.
#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    pub dir_name: String,
    pub common_dir: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("repo not found in registry: {0}")]
    NotFound(String),
    #[error("group not found: {0}")]
    GroupNotFound(String),
    #[error("path not in registry: {0}")]
    PathNotRegistered(String),
    #[error("`@{group}` cannot be used at the top level — use `gnx group {hint}` instead")]
    GroupAtTopLevel { group: String, hint: String },
}

/// Thin wrapper around `resolve` that rejects `@<group>` atoms before
/// expansion. Top-level commands (`search` / `find` / `contracts` /
/// `coverage`) call this so users get a clear migration hint pointing at
/// `gnx group <verb>`. `@all` and single-repo selectors pass through
/// unchanged.
pub fn resolve_top_level(
    sel: &Selector,
    registry: &RegistryFile,
    cwd: &str,
    verb_hint: &str,
) -> Result<Vec<ResolvedRepo>, ResolveError> {
    for atom in &sel.0 {
        if let Atom::Group(g) = atom {
            return Err(ResolveError::GroupAtTopLevel {
                group: g.clone(),
                hint: verb_hint.to_string(),
            });
        }
    }
    resolve(sel, registry, cwd)
}

/// Resolve a selector to a deduplicated list of repos. Preserves first
/// occurrence order across the union so the caller has stable iteration.
pub fn resolve(
    sel: &Selector,
    registry: &RegistryFile,
    cwd: &str,
) -> Result<Vec<ResolvedRepo>, ResolveError> {
    let mut seen = HashSet::<String>::new();
    let mut out = Vec::<ResolvedRepo>::new();

    for atom in &sel.0 {
        match atom {
            Atom::Cwd => {
                let alias = find_by_path(registry, cwd)
                    .ok_or_else(|| ResolveError::PathNotRegistered(cwd.into()))?;
                push_unique(&mut seen, &mut out, alias);
            }
            Atom::Path(p) => {
                let s = p.to_string_lossy();
                let alias = find_by_path(registry, &s)
                    .ok_or_else(|| ResolveError::PathNotRegistered(s.into_owned()))?;
                push_unique(&mut seen, &mut out, alias);
            }
            Atom::Name(n) => {
                // Match by user-facing alias OR by storage dir_name.
                let alias = registry
                    .repos
                    .values()
                    .find(|r| r.aliases.iter().any(|a| a == n) || r.dir_name == *n)
                    .ok_or_else(|| ResolveError::NotFound(n.clone()))?;
                push_unique(&mut seen, &mut out, alias);
            }
            Atom::Group(g) => {
                let group = registry
                    .groups
                    .iter()
                    .find(|gr| gr.name == *g)
                    .ok_or_else(|| ResolveError::GroupNotFound(g.clone()))?;
                for member in &group.members {
                    if let Some(a) = registry.repos.get(member) {
                        push_unique(&mut seen, &mut out, a);
                    }
                }
            }
            Atom::All => {
                for alias in registry.repos.values() {
                    push_unique(&mut seen, &mut out, alias);
                }
            }
        }
    }
    Ok(out)
}

/// Locate the registered alias whose stored `common_dir` matches `p`'s
/// canonical git common-dir. This makes any cwd inside a worktree (and
/// every `git worktree add` sibling) resolve to the same alias.
pub fn find_by_path<'a>(registry: &'a RegistryFile, p: &str) -> Option<&'a RepoAlias> {
    let target_common = git_common_dir_canonical(Path::new(p))?;
    registry.repos.values().find(|alias| {
        std::fs::canonicalize(&alias.common_dir).ok().as_deref() == Some(&target_common)
    })
}

fn git_common_dir_canonical(cwd: &Path) -> Option<std::path::PathBuf> {
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = std::str::from_utf8(&out.stdout).ok()?.trim();
    let p = std::path::PathBuf::from(s);
    let resolved = if p.is_absolute() { p } else { cwd.join(p) };
    std::fs::canonicalize(resolved).ok()
}

fn push_unique(seen: &mut HashSet<String>, out: &mut Vec<ResolvedRepo>, alias: &RepoAlias) {
    if seen.insert(alias.dir_name.clone()) {
        out.push(ResolvedRepo {
            dir_name: alias.dir_name.clone(),
            common_dir: alias.common_dir.clone(),
            aliases: alias.aliases.clone(),
        });
    }
}
