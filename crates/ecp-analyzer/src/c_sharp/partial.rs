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
//! An attribute class ([`NodeKind::Annotation`], a class whose base list
//! names an `*Attribute`) compares as a class: the part that lists the base
//! is the one the parser reclassifies, and the parts are still one class.
//!
//! The project is the evidence that two files compile together. A file whose
//! project is unknown ([`CsharpProjects::project_of`]) merges with nothing:
//! an unknown project is never a shared one, and directory proximity does
//! not stand in for it.

use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::file_category::Language;
use ecp_core::graph::NodeKind;
use rustc_hash::FxHashMap;

/// The `reason` of every `PartOf` edge.
pub const PART_OF_REASON: &str = "csharp_partial_part";

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

/// The node ids of every C# `partial` type declared in two parts or more:
/// the representative part first ([`Part::rank`]), then the other parts
/// ascending; the groups ordered by their representative. Two parts in one
/// file share a node already (a uid twin binds its survivor), so a group
/// lists each node once.
///
/// The representative depends on the parts' declarations only, never on
/// the file enumeration or the node-id order.
pub fn partial_groups(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    projects: &CsharpProjects,
) -> Vec<Vec<u32>> {
    type Key<'a> = (&'a str, &'a str, &'a str, u32, u8);
    let mut parts: FxHashMap<Key<'_>, Vec<Part>> = FxHashMap::default();
    for (file_idx, lg) in local_graphs.iter().enumerate() {
        let path = lg.file_path.to_string_lossy();
        if Language::from_path(&path) != Language::CSharp {
            continue;
        }
        let path = path.replace('\\', "/");
        let Some(project) = projects.project_of(&path) else {
            continue;
        };
        for (raw_idx, node) in lg.nodes.iter().enumerate() {
            let Some(scope) = partial_scope(node) else {
                continue;
            };
            let Some(slot) = raw_map.node(FileOrd(file_idx as u32), RawIdx(raw_idx as u32)) else {
                continue;
            };
            let part = Part {
                slot,
                is_attribute: node.kind == NodeKind::Annotation,
                has_base_list: !node.heritage.is_empty(),
                stem_names_type: file_stem(&path) == node.name,
                path: path.clone(),
                position: (node.span.0, node.span.1),
            };
            let group = parts
                .entry((
                    project,
                    scope,
                    node.name.as_str(),
                    node.generic_arity(),
                    merge_kind(node.kind) as u8,
                ))
                .or_default();
            // A same-file twin is the survivor's node: the node declares a
            // base list when either declaration does, and starts at the first.
            match group.iter_mut().find(|seen| seen.slot == slot) {
                Some(seen) => {
                    seen.is_attribute |= part.is_attribute;
                    seen.has_base_list |= part.has_base_list;
                    seen.position = seen.position.min(part.position);
                }
                None => group.push(part),
            }
        }
    }
    let mut groups: Vec<Vec<u32>> = parts
        .into_values()
        .filter(|group| group.len() >= 2)
        .map(|group| {
            let representative = group
                .iter()
                .min_by(|a, b| a.rank().cmp(&b.rank()))
                .expect("a group holds two parts")
                .slot;
            let mut others: Vec<u32> = group
                .iter()
                .map(|part| part.slot)
                .filter(|&slot| slot != representative)
                .collect();
            others.sort_unstable();
            std::iter::once(representative).chain(others).collect()
        })
        .collect();
    groups.sort_unstable_by_key(|ids| ids[0]);
    groups
}

/// One part of a `partial` type: its node and what picks the representative.
struct Part {
    slot: u32,
    is_attribute: bool,
    has_base_list: bool,
    stem_names_type: bool,
    /// Repo-relative, `/`-separated.
    path: String,
    /// The 0-based start row and column of the declaration.
    position: (u32, u32),
}

impl Part {
    /// The representative is the part of lowest rank: the attribute part
    /// (so the type's node carries the attribute kind), else the part that
    /// declares a base list, else the part whose file stem is the type
    /// name, else the first normalized path; within one path, the first
    /// declaration. Each tier breaks its ties by the tiers after it.
    fn rank(&self) -> (bool, bool, bool, &str, (u32, u32)) {
        (
            !self.is_attribute,
            !self.has_base_list,
            !self.stem_names_type,
            &self.path,
            self.position,
        )
    }
}

/// The file name of `path` without its last extension:
/// `Src/JsonReader.Async.cs` → `JsonReader.Async`.
fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// Whether a C# declaration of `kind` is a type that may be `partial`:
/// [`NodeKind::is_type`], plus the attribute class the parser files as
/// [`NodeKind::Annotation`], which `is_type` leaves out.
pub(crate) fn is_partial_kind(kind: NodeKind) -> bool {
    kind.is_type() || kind == NodeKind::Annotation
}

/// The kind a part merges by: an attribute class merges as the class it is.
fn merge_kind(kind: NodeKind) -> NodeKind {
    match kind {
        NodeKind::Annotation => NodeKind::Class,
        kind => kind,
    }
}

/// The declaring scope of a `partial` type node; `None` for any other node.
fn partial_scope(node: &RawNode) -> Option<&str> {
    if is_partial_kind(node.kind) && !node.name.is_empty() {
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
    fn test_file_stem_drops_last_extension_only() {
        assert_eq!(file_stem("Src/JsonReader.Async.cs"), "JsonReader.Async");
        assert_eq!(file_stem("JsonReader.cs"), "JsonReader");
        assert_eq!(file_stem("Src/NoExtension"), "NoExtension");
        assert_eq!(file_stem(""), "");
    }

    fn part(slot: u32, has_base_list: bool, stem_names_type: bool, path: &str) -> Part {
        Part {
            slot,
            is_attribute: false,
            has_base_list,
            stem_names_type,
            path: path.to_string(),
            position: (0, 0),
        }
    }

    /// The lowest rank is the representative: a base list beats a stem
    /// match, a stem match beats an earlier path, and the slot never counts.
    #[test]
    fn test_rank_tiers_order_base_list_then_stem_then_path() {
        let base = part(9, true, false, "z/Reader.io.cs");
        let stem = part(1, false, true, "z/Reader.cs");
        let early = part(0, false, false, "a/Reader.Async.cs");
        assert!(base.rank() < stem.rank());
        assert!(stem.rank() < early.rank());
        let late = part(0, false, false, "b/Reader.More.cs");
        assert!(early.rank() < late.rank(), "the path decides, not the slot");
    }

    /// The attribute part outranks a part with a base list and a stem match,
    /// so the type's node carries the attribute kind.
    #[test]
    fn test_rank_attribute_part_outranks_base_list_and_stem() {
        let attribute = Part {
            is_attribute: true,
            ..part(5, true, false, "z/Mark.Attr.cs")
        };
        let stem = part(0, true, true, "a/MarkAttribute.cs");
        assert!(attribute.rank() < stem.rank());
    }

    #[test]
    fn test_merge_kind_annotation_merges_as_class() {
        assert_eq!(merge_kind(NodeKind::Annotation), NodeKind::Class);
        assert_eq!(merge_kind(NodeKind::Struct), NodeKind::Struct);
        assert!(is_partial_kind(NodeKind::Annotation));
        assert!(!is_partial_kind(NodeKind::Function));
    }

    #[test]
    fn test_add_two_projects_in_one_dir_subtree_only_when_both() {
        let projects = projects(&[("src", true), ("src", false)], 4);
        assert_eq!(projects.project_of("src/A.cs"), None);
    }
}
