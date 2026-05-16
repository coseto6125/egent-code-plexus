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

// ── Resolver (Task 0.4) ──────────────────────────────────────────────────────

use graph_nexus_core::registry::{BranchEntry, RegistryFile, RepoEntry};
use std::collections::HashSet;

/// A repo resolved from a selector atom — points into the registry,
/// retaining the name + paths the caller needs to load the graph.
#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    pub name: String,
    pub worktree_path: String,
    pub index_dir_root: String,
    pub branches: Vec<BranchEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("repo not found in registry: {0}")]
    NotFound(String),
    #[error("group not found: {0}")]
    GroupNotFound(String),
    #[error("path not in registry: {0}")]
    PathNotRegistered(String),
}

/// Resolve a selector to a deduplicated list of repos. Preserves the first
/// occurrence order across the union to give the caller a stable iteration.
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
                let repo = find_by_path(registry, cwd)
                    .ok_or_else(|| ResolveError::PathNotRegistered(cwd.into()))?;
                push_unique(&mut seen, &mut out, repo);
            }
            Atom::Path(p) => {
                let p_str = p.to_string_lossy();
                let repo = find_by_path(registry, &p_str)
                    .ok_or_else(|| ResolveError::PathNotRegistered(p_str.into_owned()))?;
                push_unique(&mut seen, &mut out, repo);
            }
            Atom::Name(n) => {
                let repo = registry
                    .repos
                    .iter()
                    .find(|r| r.name == *n)
                    .ok_or_else(|| ResolveError::NotFound(n.clone()))?;
                push_unique(&mut seen, &mut out, repo);
            }
            Atom::Group(g) => {
                let group = registry
                    .groups
                    .iter()
                    .find(|gr| gr.name == *g)
                    .ok_or_else(|| ResolveError::GroupNotFound(g.clone()))?;
                for member_name in &group.members {
                    if let Some(repo) = registry.repos.iter().find(|r| r.name == *member_name) {
                        push_unique(&mut seen, &mut out, repo);
                    }
                }
            }
            Atom::All => {
                for repo in &registry.repos {
                    push_unique(&mut seen, &mut out, repo);
                }
            }
        }
    }
    Ok(out)
}

fn find_by_path<'a>(registry: &'a RegistryFile, p: &str) -> Option<&'a RepoEntry> {
    let target = Path::new(p).canonicalize().unwrap_or_else(|_| p.into());
    registry.repos.iter().find(|r| {
        Path::new(&r.worktree_path)
            .canonicalize()
            .map(|c| c == target)
            .unwrap_or(false)
    })
}

fn push_unique(seen: &mut HashSet<String>, out: &mut Vec<ResolvedRepo>, repo: &RepoEntry) {
    if seen.insert(repo.name.clone()) {
        out.push(ResolvedRepo {
            name: repo.name.clone(),
            worktree_path: repo.worktree_path.clone(),
            index_dir_root: repo.index_dir_root.clone(),
            branches: repo.branches.clone(),
        });
    }
}
