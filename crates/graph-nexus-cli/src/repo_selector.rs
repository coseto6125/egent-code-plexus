//! `--repo` selector grammar parser.
//!
//! selector := atom | atom,atom,...
//! atom     := <path> | <name> | @<group> | @all
//!
//! Resolution to actual repo paths is done by `resolve()` (Task 0.4),
//! which needs registry access; this module only handles syntax.

use std::path::PathBuf;

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
