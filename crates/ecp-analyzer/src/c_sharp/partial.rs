//! C# `partial` types: one type declared in several parts, often across
//! files (Newtonsoft `JsonWriter.cs` and `JsonWriter.Async.cs`).
//!
//! Each part keeps its own node. The builder records the parts of one type
//! in the symbol table (`SymbolTable::note_partial_type`), and Pass 1.9,
//! Pass 2 and the `Overrides` method index read that one equivalence: the
//! bases any part lists are the type's, a base spelling names the type once,
//! and a member any part declares is the type's own (codex ruling P-d).
//!
//! Two declarations are parts of one type only when every merge-key field
//! is equal: the project, the declaring scope (namespace and enclosing
//! types, [`RawNode::partial_marker`]), the name, the generic arity and the
//! kind. Each carries `partial`; C# rejects a `partial` and a non-`partial`
//! declaration of one type, so a declaration without the marker joins none.
//!
//! The project is the evidence that two files compile together. A file whose
//! project is unknown ([`CsharpProjects::project_of`]) merges with nothing:
//! an unknown project is never a shared one, and directory proximity does
//! not stand in for it.

use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::file_category::Language;
use rustc_hash::FxHashMap;

/// The C# projects of the repo, as the index's `.csproj` scan found them.
/// Empty by default: then every file's project is unknown.
#[derive(Debug, Clone, Default)]
pub struct CsharpProjects {
    /// Repo-relative, `/`-separated project directory (`""` for the repo
    /// root) → whether the project compiles its whole directory subtree.
    dirs: FxHashMap<String, bool>,
    /// The deepest directory level the scan read (the repo root is level
    /// 0). A deeper directory may hold a project the scan never saw.
    scanned_depth: usize,
}

impl CsharpProjects {
    /// No project yet, from a scan that read `scanned_depth` directory
    /// levels below the repo root.
    pub fn new(scanned_depth: usize) -> Self {
        Self {
            dirs: FxHashMap::default(),
            scanned_depth,
        }
    }

    /// Record a project file found in directory `dir`. `subtree`: an
    /// SDK-style project whose default compile items take every `.cs` file
    /// below `dir` and that removes none; any other project lists its files,
    /// which only a parse of its compile items could confirm. A directory
    /// with two projects compiles its subtree only when both do.
    pub fn add(&mut self, dir: &str, subtree: bool) {
        let dir = dir.trim_matches('/');
        self.dirs
            .entry(dir.to_string())
            .and_modify(|all| *all &= subtree)
            .or_insert(subtree);
    }

    /// The project directory that compiles the file at `path`: the nearest
    /// ancestor directory holding a project, which compiles every file
    /// below it except those under a nested project. `None` (unknown) when
    /// no ancestor holds one, when the nearest one lists its files, or when
    /// the file sits deeper than the scan read.
    pub fn project_of(&self, path: &str) -> Option<&str> {
        let mut dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let depth = if dir.is_empty() {
            0
        } else {
            dir.split('/').count()
        };
        if depth > self.scanned_depth {
            return None;
        }
        loop {
            if let Some((project, &subtree)) = self.dirs.get_key_value(dir) {
                return subtree.then_some(project.as_str());
            }
            if dir.is_empty() {
                return None;
            }
            dir = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
}

/// The node ids of every C# `partial` type declared in two parts or more,
/// each group ascending, the groups ordered by their first part. Two parts
/// in one file share a node already (a uid twin binds its survivor), so a
/// group lists each node once.
pub fn partial_groups(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    projects: &CsharpProjects,
) -> Vec<Vec<u32>> {
    type Key<'a> = (&'a str, &'a str, &'a str, u32, u8);
    let mut parts: FxHashMap<Key<'_>, Vec<u32>> = FxHashMap::default();
    for (file_idx, lg) in local_graphs.iter().enumerate() {
        let path = lg.file_path.to_string_lossy();
        if Language::from_path(&path) != Language::CSharp {
            continue;
        }
        let Some(project) = projects.project_of(&path.replace('\\', "/")) else {
            continue;
        };
        for (raw_idx, node) in lg.nodes.iter().enumerate() {
            let Some(scope) = partial_scope(node) else {
                continue;
            };
            let Some(slot) = raw_map.node(FileOrd(file_idx as u32), RawIdx(raw_idx as u32)) else {
                continue;
            };
            parts
                .entry((
                    project,
                    scope,
                    node.name.as_str(),
                    node.generic_arity(),
                    node.kind as u8,
                ))
                .or_default()
                .push(slot);
        }
    }
    let mut groups: Vec<Vec<u32>> = parts
        .into_values()
        .filter_map(|mut ids| {
            ids.sort_unstable();
            ids.dedup();
            (ids.len() >= 2).then_some(ids)
        })
        .collect();
    groups.sort_unstable_by_key(|ids| ids[0]);
    groups
}

/// The declaring scope of a `partial` type node; `None` for any other node.
fn partial_scope(node: &RawNode) -> Option<&str> {
    if node.kind.is_type() && !node.name.is_empty() {
        node.partial_scope()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projects(dirs: &[(&str, bool)], scanned_depth: usize) -> CsharpProjects {
        let mut projects = CsharpProjects::new(scanned_depth);
        for &(dir, subtree) in dirs {
            projects.add(dir, subtree);
        }
        projects
    }

    #[test]
    fn test_project_of_nearest_ancestor_returns_nested_project() {
        let projects = projects(&[("src", true), ("src/Lib", true)], 4);
        assert_eq!(projects.project_of("src/Lib/Json/A.cs"), Some("src/Lib"));
        assert_eq!(projects.project_of("src/App.cs"), Some("src"));
    }

    #[test]
    fn test_project_of_no_ancestor_returns_none() {
        let projects = projects(&[("src/Lib", true)], 4);
        assert_eq!(projects.project_of("tools/A.cs"), None);
        assert_eq!(projects.project_of("A.cs"), None);
    }

    #[test]
    fn test_project_of_root_project_returns_empty_dir() {
        let projects = projects(&[("", true)], 4);
        assert_eq!(projects.project_of("A.cs"), Some(""));
        assert_eq!(projects.project_of("a/b/A.cs"), Some(""));
    }

    #[test]
    fn test_project_of_deeper_than_scan_returns_none() {
        let projects = projects(&[("src", true)], 2);
        assert_eq!(projects.project_of("src/a/A.cs"), Some("src"));
        assert_eq!(projects.project_of("src/a/b/A.cs"), None);
    }

    #[test]
    fn test_project_of_listed_project_returns_none() {
        let projects = projects(&[("src", true), ("src/Old", false)], 4);
        assert_eq!(projects.project_of("src/Old/A.cs"), None);
        assert_eq!(projects.project_of("src/New/A.cs"), Some("src"));
    }

    #[test]
    fn test_add_two_projects_in_one_dir_subtree_only_when_both() {
        let projects = projects(&[("src", true), ("src", false)], 4);
        assert_eq!(projects.project_of("src/A.cs"), None);
    }
}
