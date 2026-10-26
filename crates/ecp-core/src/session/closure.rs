//! The invalidation closure of the incremental overlay (architecture
//! direction, "Beyond calls: the incremental overlay", the incremental row):
//! the clean files a dirty change makes the overlay resolve again.
//!
//! A clean file is resolved again by re-parsing it, byte-identical, and
//! handing its parse to the view beside the dirty files
//! ([`super::view::OverlayView::build_with_closure`]). Every output the view
//! rebuilds for a dirty file is then rebuilt for it too, against the dirty
//! view: its `Calls`, `Imports`, heritage, `Overrides`, `Decorates`,
//! `ReadsField`, `References` and record edges. Its declarations pair with
//! their base nodes one to one, so edges into it from other clean files stay.
//!
//! ## Input
//!
//! What the dirty files changed, read against `graph.bin`, per dirty path:
//! each declaration key (kind, owner, name) the base holds and the parse
//! does not (removed), the parse holds and the base does not (added), or
//! both hold with another count or another resolution fact (changed): the
//! ES-module export flag, a callable's return type, parameters and static
//! flag, a type's resolved bases. A type whose member set changed is
//! changed. A removed path removes every base declaration of its own.
//!
//! ## Dependency facts
//!
//! From `graph.bin`, through the archived tables the view already reads:
//! the reverse adjacency (`in_offsets`, `in_edge_idx`), the name index and
//! each file's archived import rows. No table is rebuilt and nothing is
//! stored for the closure, so no format changes. The parse cache was the
//! other candidate: it holds every call site, but only by (path, content),
//! so finding a name's callers in it reads every file of the repo.
//!
//! | Change | Dependent clean files |
//! |---|---|
//! | a removed or changed declaration | the source files of its base in-edges of a kind the view rebuilds ([`REBUILT_RELS`]) |
//! | an added or removed name | the same, for every base node of that name in any file: a clean call bound to a namesake may turn ambiguous or move |
//! | a changed type | its clean subtypes, transitively over the base `Extends` / `Implements` in-edges |
//! | an added or removed name | each file whose archived import rows import that name |
//!
//! The base graph records a call site only when it bound. Two gaps follow,
//! and the closure names them in [`StaleClosure`] instead of guessing:
//!
//! - A removed declaration that leaves exactly one namesake in the repo, of
//!   at least two before: a clean call the index found ambiguous may bind to
//!   the survivor in a fresh build, and no edge records it
//!   (FU-2026-10-08-12f0cfa8b581). [`StaleClosure::names`] lists the name.
//! - A dirty path that held `Route` nodes: routes, their `HandlesRoute`
//!   edges, the controller `EntryPoint` nodes and the `Fetches` edges that
//!   other files bind to them are made by the builder and stay as indexed
//!   (C4 Open point 4).
//!
//! A clean call to a name no file declared before the edit is a call into
//! an external library, and the index left it unbound. A fresh build binds
//! it to a new declaration of that name by the global tier; the overlay
//! keeps it unbound and says nothing, because every new declaration would
//! otherwise carry a caveat that is almost never true.
//!
//! ## The bound
//!
//! [`CLOSURE_LIMIT`] clean files. DIR's latency gate allows the first query
//! after an edit 363 ms on vscode; the closure takes at most a tenth of it,
//! 36 ms. The cold index parses vscode's 12,737 files in 3.16 s, 0.25 ms a
//! file, so 36 ms parses 145 files; the limit is the power of two below.
//! Past the limit the closure re-resolves nothing and names every kind in
//! [`REBUILT_RELS`] as stale.
//!
//! ## Cost
//!
//! One pass over `graph.files` and, when a dirty path is indexed, one over
//! `graph.nodes` (the view's build makes the same two). Then per changed
//! declaration its in-degree, per changed name O(log N) plus its namesakes'
//! in-degrees, per changed type its subtype tree, and one pass over the
//! archived import rows when a name was added or removed. Nothing here runs
//! per query: the refresh that wrote the dirty fragments computes it once.

use super::view::{base_function_meta, OverlayFileInput, OverlaySymbol};
use crate::file_category::Language;
use crate::graph::{ArchivedZeroCopyGraph, FunctionMeta, NodeKind, RelType, SYNTHETIC_FILE_IDX};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;

/// The most clean files one closure re-resolves (module doc, "The bound").
pub const CLOSURE_LIMIT: usize = 128;

/// The base edge kinds the view rebuilds for a re-parsed file: an in-edge of
/// one of these kinds makes its clean source a dependent. Past the bound,
/// these are the kinds a clean file may hold stale.
pub const REBUILT_RELS: [RelType; 9] = [
    RelType::Calls,
    RelType::Imports,
    RelType::Extends,
    RelType::Implements,
    RelType::Overrides,
    RelType::Decorates,
    RelType::ReadsField,
    RelType::References,
    RelType::QueriesTable,
];

/// What the builder makes from routes and binds across files, which no
/// overlay replays: [`StaleClosure::kinds`] of a dirty route file.
pub const ROUTE_KINDS: [&str; 3] = ["Fetches", "HandlesRoute", "EntryPoint"];

/// Why some clean-file edges of a dirty view may be stale: empty when the
/// closure re-resolved every dependent it could find and every dependency
/// fact was recorded. The payload names these kinds (F6).
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(derive(Debug))]
pub struct StaleClosure {
    /// The dependent count when it passed [`CLOSURE_LIMIT`]: no clean file
    /// was re-resolved. `None` when the closure is bounded.
    pub unbounded: Option<u32>,
    /// No closure was computed for the current dirty state (a refresh
    /// failed, or an older binary wrote the fragments): clean files keep
    /// their indexed edges.
    pub not_computed: bool,
    /// The edge and node kinds that may be stale in clean files, as their
    /// `as_str` names, sorted, unique.
    pub kinds: Vec<String>,
    /// The removed names whose last namesake a clean ambiguous call may now
    /// bind to (module doc), sorted, unique.
    pub names: Vec<String>,
}

impl StaleClosure {
    /// True when no clean-file edge is known to be stale.
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// The closure was not computed for the current dirty state.
    pub fn not_computed() -> Self {
        Self {
            not_computed: true,
            kinds: rebuilt_kind_names(),
            ..Self::default()
        }
    }

    fn add_kinds<'k>(&mut self, kinds: impl IntoIterator<Item = &'k str>) {
        self.kinds.extend(kinds.into_iter().map(str::to_owned));
        self.kinds.sort_unstable();
        self.kinds.dedup();
    }
}

fn rebuilt_kind_names() -> Vec<String> {
    let mut kinds: Vec<String> = REBUILT_RELS.iter().map(|r| r.as_str().to_owned()).collect();
    kinds.sort_unstable();
    kinds
}

/// The clean files to re-resolve and what stays stale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Closure {
    /// Repo-relative paths, normalized as `File.path`, sorted, unique; no
    /// dirty or removed path. Empty when the closure is unbounded.
    pub paths: Vec<String>,
    pub stale: StaleClosure,
}

/// The closure of the dirty `files` (their fresh parses) and the `removed`
/// paths over `graph` (module doc). Paths are `File.path`-normalized.
pub fn invalidation_closure(
    graph: &ArchivedZeroCopyGraph,
    files: &[OverlayFileInput],
    removed: &[String],
) -> Closure {
    if files.is_empty() && removed.is_empty() {
        return Closure::default();
    }
    let changes = Changes::of(graph, files, removed);
    let mut stale = StaleClosure::default();
    if changes.routes {
        stale.add_kinds(ROUTE_KINDS);
    }

    let mut deps = Dependents {
        graph,
        dirty: &changes.dirty_files,
        files: FxHashSet::default(),
    };
    for &node in &changes.base {
        deps.in_edges_of(node);
    }
    for name in &changes.names {
        for node in graph.nodes_by_name(name) {
            deps.in_edges_of(node);
        }
    }
    deps.subtypes_of(&changes.types);
    deps.importers_of(&changes.names);

    for (name, kind) in &changes.removed {
        if leaves_one_namesake(graph, files, &changes.dirty_files, name) {
            stale.names.push((*name).to_owned());
            stale.add_kinds(name_lookup_kinds(*kind).iter().copied());
        }
    }
    stale.names.sort_unstable();
    stale.names.dedup();

    if deps.files.len() > CLOSURE_LIMIT {
        stale.unbounded = Some(deps.files.len() as u32);
        stale.add_kinds(REBUILT_RELS.iter().map(|r| r.as_str()));
        return Closure {
            paths: Vec::new(),
            stale,
        };
    }
    let pool = &graph.string_pool;
    let dirty_paths: FxHashSet<&str> = files
        .iter()
        .map(|f| f.rel_path.as_str())
        .chain(removed.iter().map(String::as_str))
        .collect();
    let paths: BTreeSet<&str> = deps
        .files
        .iter()
        .filter_map(|&file| graph.files.get(file as usize))
        .map(|file| file.path.resolve(pool))
        .filter(|path| !dirty_paths.contains(path))
        .collect();
    Closure {
        paths: paths.into_iter().map(str::to_owned).collect(),
        stale,
    }
}

/// A declaration key: kind (`NodeKind as u8`, as `NodeKind` is not
/// `Hash`), owner (empty without one), name.
type Key<'a> = (u8, &'a str, &'a str);

/// What the dirty paths changed against the base (module doc, "Input").
struct Changes<'a> {
    /// Every `graph.files` index of a dirty or removed path.
    dirty_files: FxHashSet<u32>,
    /// The base declarations removed or changed.
    base: Vec<u32>,
    /// The base types changed, a subset of `base`.
    types: Vec<u32>,
    /// The names added or removed.
    names: FxHashSet<&'a str>,
    /// Each removed key's name and kind.
    removed: Vec<(&'a str, NodeKind)>,
    /// A dirty or removed path held a `Route` node.
    routes: bool,
}

impl<'a> Changes<'a> {
    fn of(
        graph: &'a ArchivedZeroCopyGraph,
        files: &'a [OverlayFileInput],
        removed: &[String],
    ) -> Self {
        let pool = &graph.string_pool;
        let mut inputs: FxHashMap<&str, Vec<&OverlayFileInput>> = FxHashMap::default();
        for file in files {
            inputs.entry(file.rel_path.as_str()).or_default().push(file);
        }
        let removed: FxHashSet<&str> = removed.iter().map(String::as_str).collect();
        let mut path_of: FxHashMap<u32, &'a str> = FxHashMap::default();
        for (i, file) in graph.files.iter().enumerate() {
            let path = file.path.resolve(pool);
            if inputs.contains_key(path) || removed.contains(path) {
                path_of.insert(i as u32, path);
            }
        }
        let mut changes = Self {
            dirty_files: path_of.keys().copied().collect(),
            base: Vec::new(),
            types: Vec::new(),
            names: FxHashSet::default(),
            removed: Vec::new(),
            routes: false,
        };

        // One node scan: each dirty path's base declarations, in graph order.
        let mut base_decls: FxHashMap<&'a str, Vec<u32>> = FxHashMap::default();
        if !path_of.is_empty() {
            for (i, node) in graph.nodes.iter().enumerate() {
                let Some(&path) = path_of.get(&node.file_idx.to_native()) else {
                    continue;
                };
                let kind = NodeKind::from(&node.kind);
                if kind == NodeKind::Route {
                    changes.routes = true;
                }
                if declares(kind) {
                    base_decls.entry(path).or_default().push(i as u32);
                }
            }
        }

        let dirty_types = dirty_type_names(files);
        let mut paths: BTreeSet<&'a str> = base_decls.keys().copied().collect();
        paths.extend(inputs.keys().copied());
        for path in paths {
            let symbols: Vec<&'a OverlaySymbol> = inputs
                .get(path)
                .into_iter()
                .flatten()
                .flat_map(|&file| file.symbols.iter())
                .filter(|s| declares(s.kind))
                .collect();
            let base = base_decls.get(path).map(Vec::as_slice).unwrap_or_default();
            let es_module = inputs
                .get(path)
                .and_then(|files| files.first())
                .is_some_and(|file| {
                    matches!(
                        file.file_meta().language,
                        Language::TypeScript | Language::JavaScript
                    )
                });
            changes.diff_path(graph, base, &symbols, es_module, &dirty_types);
        }
        changes
    }

    /// One path's removed, added and changed keys.
    fn diff_path(
        &mut self,
        graph: &'a ArchivedZeroCopyGraph,
        base: &[u32],
        symbols: &[&'a OverlaySymbol],
        es_module: bool,
        dirty_types: &FxHashSet<&'a str>,
    ) {
        let pool = &graph.string_pool;
        let mut old: FxHashMap<Key<'a>, (NodeKind, Vec<u32>)> = FxHashMap::default();
        for &idx in base {
            let node = &graph.nodes[idx as usize];
            let kind = NodeKind::from(&node.kind);
            let key = (
                kind as u8,
                node.owner_class.resolve(pool),
                node.name.resolve(pool),
            );
            old.entry(key)
                .or_insert_with(|| (kind, Vec::new()))
                .1
                .push(idx);
        }
        let mut new: FxHashMap<Key<'a>, Vec<&'a OverlaySymbol>> = FxHashMap::default();
        for &symbol in symbols {
            let key = (
                symbol.kind as u8,
                symbol.owner_class.as_deref().unwrap_or(""),
                symbol.name.as_str(),
            );
            new.entry(key).or_default().push(symbol);
        }
        // Owners whose member set changed.
        let mut owners: FxHashSet<&'a str> = FxHashSet::default();
        for (key, (kind, idxs)) in &old {
            let (_, owner, name) = *key;
            let changed = match new.get(key) {
                None => {
                    self.names.insert(name);
                    self.removed.push((name, *kind));
                    owners.insert(owner);
                    true
                }
                Some(fresh) => {
                    fresh.len() != idxs.len()
                        || facts_differ(graph, idxs[0], fresh[0], es_module, dirty_types)
                }
            };
            if changed {
                self.base.extend(idxs);
                if is_type(*kind) {
                    self.types.extend(idxs);
                }
            }
        }
        for key in new.keys() {
            if !old.contains_key(key) {
                let (_, owner, name) = *key;
                self.names.insert(name);
                owners.insert(owner);
            }
        }
        owners.remove("");
        for (key, (kind, idxs)) in &old {
            let (_, _, name) = *key;
            if is_type(*kind) && owners.contains(name) {
                self.base.extend(idxs);
                self.types.extend(idxs);
            }
        }
        self.base.sort_unstable();
        self.base.dedup();
        self.types.sort_unstable();
        self.types.dedup();
    }
}

/// Whether the base declaration `idx` and its fresh parse `symbol` (same
/// key) differ in a fact another file's resolution reads.
fn facts_differ(
    graph: &ArchivedZeroCopyGraph,
    idx: u32,
    symbol: &OverlaySymbol,
    es_module: bool,
    dirty_types: &FxHashSet<&str>,
) -> bool {
    let pool = &graph.string_pool;
    // `exported_nodes` marks ES-module declarations only; any other file
    // has no export eligibility to change.
    if es_module {
        let file = graph.nodes[idx as usize].file_idx.to_native();
        let base_exported = graph.is_es_module_file(file) && graph.is_exported_node(idx);
        if base_exported != symbol.is_exported {
            return true;
        }
    }
    if is_callable(symbol.kind) {
        let meta = base_function_meta(graph, idx);
        let base_return = meta
            .map(|m| m.return_type.resolve(pool))
            .filter(|t| !t.is_empty());
        let fresh_return = symbol.return_type.as_deref().filter(|t| !t.is_empty());
        if base_return != fresh_return {
            return true;
        }
        if let (Some(meta), Some(flags)) = (meta, symbol.flags) {
            let base_static = meta.flags.to_native() & FunctionMeta::FLAG_STATIC;
            if base_static != flags & FunctionMeta::FLAG_STATIC {
                return true;
            }
            let base_params = meta.params.iter().map(|p| p.resolve(pool));
            if !base_params.eq(symbol.params.iter().map(String::as_str)) {
                return true;
            }
        }
    }
    if is_type(symbol.kind) {
        let base_names: FxHashSet<&str> = graph
            .supertypes_of(idx)
            .map(|(bases, _)| bases)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|base| graph.nodes.get(base as usize))
            .map(|node| node.name.resolve(pool))
            .collect();
        let fresh_names: FxHashSet<&str> =
            symbol.heritage.iter().map(|s| base_short_name(s)).collect();
        if base_names.iter().any(|name| !fresh_names.contains(name)) {
            return true;
        }
        // A new spelling that names no type anywhere is an external base:
        // the index left it unresolved, and so does a fresh build.
        let names_a_type = |name: &str| {
            dirty_types.contains(name)
                || graph
                    .nodes_by_name(name)
                    .any(|n| is_type(NodeKind::from(&graph.nodes[n as usize].kind)))
        };
        if fresh_names
            .iter()
            .any(|name| !base_names.contains(name) && names_a_type(name))
        {
            return true;
        }
    }
    false
}

/// The type a base spelling names, without its type arguments and
/// qualifier: `java.util.List<T>` and `crate::a::List` both name `List`.
fn base_short_name(spelling: &str) -> &str {
    let head = spelling
        .split(['<', '(', '['])
        .next()
        .unwrap_or(spelling)
        .trim();
    head.rsplit(['.', ':', '\\']).next().unwrap_or(head).trim()
}

/// Every type name a dirty parse declares.
fn dirty_type_names(files: &[OverlayFileInput]) -> FxHashSet<&str> {
    files
        .iter()
        .flat_map(|file| &file.symbols)
        .filter(|s| is_type(s.kind))
        .map(|s| s.name.as_str())
        .collect()
}

/// Whether removing `name` leaves exactly one declaration of it in the
/// repo, of at least two before: the ambiguity a clean call met may
/// collapse onto the survivor.
fn leaves_one_namesake(
    graph: &ArchivedZeroCopyGraph,
    files: &[OverlayFileInput],
    dirty_files: &FxHashSet<u32>,
    name: &str,
) -> bool {
    let mut before = 0usize;
    let mut clean = 0usize;
    for node in graph.nodes_by_name(name) {
        let kind = NodeKind::from(&graph.nodes[node as usize].kind);
        if !declares(kind) {
            continue;
        }
        before += 1;
        if !dirty_files.contains(&graph.nodes[node as usize].file_idx.to_native()) {
            clean += 1;
        }
    }
    let fresh = files
        .iter()
        .flat_map(|file| &file.symbols)
        .filter(|s| declares(s.kind) && s.name == name)
        .count();
    before >= 2 && clean + fresh == 1
}

/// The clean files that depend on a change.
struct Dependents<'g> {
    graph: &'g ArchivedZeroCopyGraph,
    dirty: &'g FxHashSet<u32>,
    /// `graph.files` indexes.
    files: FxHashSet<u32>,
}

impl Dependents<'_> {
    /// The file of the base node `source` when it is clean and owned.
    fn clean_file(&self, source: u32) -> Option<u32> {
        let file = self.graph.nodes.get(source as usize)?.file_idx.to_native();
        (file != SYNTHETIC_FILE_IDX && !self.dirty.contains(&file)).then_some(file)
    }

    /// The clean source files of `node`'s base in-edges of a rebuilt kind.
    fn in_edges_of(&mut self, node: u32) {
        let graph = self.graph;
        for edge in in_edges(graph, node) {
            if !REBUILT_RELS.contains(&RelType::from(&edge.rel_type)) {
                continue;
            }
            if let Some(file) = self.clean_file(edge.source.to_native()) {
                self.files.insert(file);
            }
        }
    }

    /// The clean subtypes of `types`, transitively: each one's file. A
    /// dirty subtype is walked through, as its clean subtypes inherit too.
    fn subtypes_of(&mut self, types: &[u32]) {
        let graph = self.graph;
        let mut seen: FxHashSet<u32> = types.iter().copied().collect();
        let mut queue: Vec<u32> = types.to_vec();
        while let Some(ty) = queue.pop() {
            for edge in in_edges(graph, ty) {
                if !matches!(
                    RelType::from(&edge.rel_type),
                    RelType::Extends | RelType::Implements
                ) {
                    continue;
                }
                let sub = edge.source.to_native();
                if !seen.insert(sub) {
                    continue;
                }
                if let Some(file) = self.clean_file(sub) {
                    self.files.insert(file);
                }
                queue.push(sub);
            }
        }
    }

    /// Each clean file whose archived import rows import one of `names`.
    fn importers_of(&mut self, names: &FxHashSet<&str>) {
        if names.is_empty() {
            return;
        }
        let graph = self.graph;
        let pool = &graph.string_pool;
        for file in 0..graph.files.len() as u32 {
            if self.dirty.contains(&file) || self.files.contains(&file) {
                continue;
            }
            let Some(rows) = graph.file_imports(file) else {
                return; // a graph without module facts has no rows at all
            };
            if rows
                .iter()
                .any(|row| names.contains(row.imported_name.resolve(pool)))
            {
                self.files.insert(file);
            }
        }
    }
}

/// The base in-edges of `node`.
fn in_edges(
    graph: &ArchivedZeroCopyGraph,
    node: u32,
) -> impl Iterator<Item = &crate::graph::ArchivedEdge> + '_ {
    let at = node as usize;
    let range = match (graph.in_offsets.get(at), graph.in_offsets.get(at + 1)) {
        (Some(start), Some(end)) => start.to_native() as usize..end.to_native() as usize,
        _ => 0..0,
    };
    graph.in_edge_idx[range]
        .iter()
        .filter_map(move |&edge| graph.edges.get(edge.to_native() as usize))
}

/// The kinds a fragment declares and another file can name.
fn declares(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Constructor
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::Impl
            | NodeKind::Typedef
            | NodeKind::Property
            | NodeKind::Variable
            | NodeKind::Const
            | NodeKind::EnumVariant
            | NodeKind::Namespace
            | NodeKind::Module
            | NodeKind::Macro
    )
}

fn is_callable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Constructor
    )
}

fn is_type(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::Impl
    )
}

/// The edge kinds a clean site that names a `kind` declaration resolves
/// by name.
fn name_lookup_kinds(kind: NodeKind) -> &'static [&'static str] {
    match kind {
        NodeKind::Function | NodeKind::Method | NodeKind::Constructor => &["Calls", "References"],
        NodeKind::Class
        | NodeKind::Struct
        | NodeKind::Interface
        | NodeKind::Trait
        | NodeKind::Enum => &["Calls", "Extends", "Implements", "QueriesTable"],
        NodeKind::Property | NodeKind::Variable | NodeKind::Const | NodeKind::EnumVariant => {
            &["ReadsField", "References"]
        }
        _ => &["References"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_fixture::GraphFixture;
    use rkyv::rancor::Error as RkyvError;

    fn sym(kind: NodeKind, owner: Option<&str>, name: &str) -> OverlaySymbol {
        OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind,
            owner_class: owner.map(str::to_string),
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            calls: Vec::new(),
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
            decorators: Vec::new(),
            params: Vec::new(),
        }
    }

    fn input(path: &str, symbols: Vec<OverlaySymbol>) -> OverlayFileInput {
        OverlayFileInput {
            rel_path: path.to_string(),
            symbols,
            imports: Vec::new(),
            export_aliases: Vec::new(),
            parsed_language: None,
        }
    }

    fn graph(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
        rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(bytes).unwrap()
    }

    /// files: a.js declares `solo`, b.js calls it, c.js calls `other`.
    fn solo_graph() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        let solo = fx.func("src/a.js", "solo");
        let run = fx.func("src/b.js", "run");
        let other = fx.func("src/c.js", "other");
        let main = fx.func("src/c.js", "main");
        fx.edge(run, solo, RelType::Calls);
        fx.edge(main, other, RelType::Calls);
        fx.into_bytes()
    }

    /// Stands for: a new same-name candidate in another file re-resolves
    /// the clean caller bound to the old one, and no unrelated clean file.
    #[test]
    fn test_invalidation_closure_candidate_added_names_bound_caller() {
        let bytes = solo_graph();
        let closure = invalidation_closure(
            graph(&bytes),
            &[input(
                "src/d.js",
                vec![sym(NodeKind::Function, None, "solo")],
            )],
            &[],
        );
        assert_eq!(closure.paths, vec!["src/b.js".to_string()]);
        assert!(closure.stale.is_empty(), "{:?}", closure.stale);
    }

    /// Stands for: deleting the only candidate re-resolves its caller.
    #[test]
    fn test_invalidation_closure_candidate_deleted_names_caller() {
        let bytes = solo_graph();
        let closure = invalidation_closure(
            graph(&bytes),
            &[input(
                "src/a.js",
                vec![sym(NodeKind::Function, None, "kept")],
            )],
            &[],
        );
        assert_eq!(closure.paths, vec!["src/b.js".to_string()]);
    }

    /// Stands for: a removed path removes its declarations, and a removed
    /// path is never a dependent of itself.
    #[test]
    fn test_invalidation_closure_removed_file_names_caller_not_itself() {
        let bytes = solo_graph();
        let closure = invalidation_closure(graph(&bytes), &[], &["src/a.js".to_string()]);
        assert_eq!(closure.paths, vec!["src/b.js".to_string()]);
    }

    /// Stands for: an unchanged re-parse (a whitespace edit, a shadow
    /// re-parse) depends on nothing; an empty input is no closure.
    #[test]
    fn test_invalidation_closure_unchanged_or_empty_is_empty() {
        let bytes = solo_graph();
        let unchanged = invalidation_closure(
            graph(&bytes),
            &[input(
                "src/a.js",
                vec![sym(NodeKind::Function, None, "solo")],
            )],
            &[],
        );
        assert_eq!(unchanged, Closure::default());
        assert_eq!(
            invalidation_closure(graph(&bytes), &[], &[]),
            Closure::default()
        );
    }

    /// Stands for: a dirty file that is also a dependent is no closure
    /// path: the view already re-resolves it.
    #[test]
    fn test_invalidation_closure_dirty_dependent_stays_out() {
        let bytes = solo_graph();
        let closure = invalidation_closure(
            graph(&bytes),
            &[
                input("src/a.js", vec![]),
                input("src/b.js", vec![sym(NodeKind::Function, None, "run")]),
            ],
            &[],
        );
        assert!(closure.paths.is_empty(), "{:?}", closure.paths);
    }

    /// Stands for: a type that gains a member re-resolves its clean
    /// subtypes, transitively, and a cycle in the base heritage ends.
    #[test]
    fn test_invalidation_closure_supertype_changed_names_subtypes_transitively() {
        let mut fx = GraphFixture::new();
        let base = fx.node(NodeKind::Class, "src/Base.java", "Base");
        let sub = fx.node(NodeKind::Class, "src/Sub.java", "Sub");
        let leaf = fx.node(NodeKind::Class, "src/Leaf.java", "Leaf");
        fx.edge(sub, base, RelType::Extends);
        fx.edge(leaf, sub, RelType::Extends);
        // A cycle the walk must survive.
        fx.edge(base, leaf, RelType::Implements);
        let bytes = fx.into_bytes();
        let closure = invalidation_closure(
            graph(&bytes),
            &[input(
                "src/Base.java",
                vec![
                    sym(NodeKind::Class, None, "Base"),
                    sym(NodeKind::Method, Some("Base"), "added"),
                ],
            )],
            &[],
        );
        assert_eq!(
            closure.paths,
            vec!["src/Leaf.java".to_string(), "src/Sub.java".to_string()]
        );
    }

    /// Stands for: a changed return type re-resolves the callers whose
    /// chained call it types.
    #[test]
    fn test_invalidation_closure_return_type_changed_names_caller() {
        let mut fx = GraphFixture::new();
        let make = fx.func("src/make.kt", "make");
        let user = fx.func("src/use.kt", "use");
        fx.edge(user, make, RelType::Calls);
        fx.function_meta(make, 0, &[]);
        let bytes = fx.into_bytes();
        let mut fresh = sym(NodeKind::Function, None, "make");
        fresh.return_type = Some("B".into());
        let closure =
            invalidation_closure(graph(&bytes), &[input("src/make.kt", vec![fresh])], &[]);
        assert_eq!(closure.paths, vec!["src/use.kt".to_string()]);
    }

    /// Stands for: more dependents than the bound re-resolve none and name
    /// every rebuilt kind.
    #[test]
    fn test_invalidation_closure_unbounded_names_stale_kinds() {
        let mut fx = GraphFixture::new();
        let hot = fx.func("src/hot.js", "hot");
        for i in 0..=CLOSURE_LIMIT {
            let caller = fx.func(&format!("src/c{i}.js"), "caller");
            fx.edge(caller, hot, RelType::Calls);
        }
        let bytes = fx.into_bytes();
        let closure = invalidation_closure(graph(&bytes), &[], &["src/hot.js".to_string()]);
        assert!(closure.paths.is_empty());
        assert_eq!(closure.stale.unbounded, Some(CLOSURE_LIMIT as u32 + 1));
        for rel in REBUILT_RELS {
            assert!(
                closure.stale.kinds.iter().any(|k| k == rel.as_str()),
                "{rel:?} in {:?}",
                closure.stale.kinds
            );
        }
    }

    /// Stands for: removing one of two declarations names the survivor's
    /// name, the ambiguous clean call graph.bin did not record.
    #[test]
    fn test_invalidation_closure_ambiguity_collapse_names_survivor() {
        let mut fx = GraphFixture::new();
        fx.func("src/a.js", "dup");
        fx.func("src/b.js", "dup");
        fx.func("src/c.js", "user");
        let bytes = fx.into_bytes();
        let closure = invalidation_closure(graph(&bytes), &[], &["src/a.js".to_string()]);
        assert_eq!(closure.stale.names, vec!["dup".to_string()]);
        assert!(closure.stale.kinds.iter().any(|k| k == "Calls"));
    }

    #[test]
    fn test_base_short_name_qualified_or_generic_returns_last_segment() {
        assert_eq!(base_short_name("java.util.List<T>"), "List");
        assert_eq!(base_short_name("crate::a::Base"), "Base");
        assert_eq!(base_short_name("  Base  "), "Base");
        assert_eq!(base_short_name(""), "");
    }
}
