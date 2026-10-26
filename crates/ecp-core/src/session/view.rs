//! Query-time L1 overlay merge view (FU-2026-06-10-398a846bca42 root cure).
//!
//! `OverlayView` makes uncommitted-edit symbols AND edges visible to graph
//! traversals (impact, cypher) without rebuilding L2 and without loading
//! O(graph) data. The base `ArchivedZeroCopyGraph` stays an immutable mmap;
//! the view is a small in-memory delta whose own allocation is O(dirty
//! symbols). Building it scans `graph.files` and `graph.nodes` once each,
//! and a query whose session cache holds a view of the same graph,
//! fragments and tombstones decodes that ([`OverlayView::from_cache_bytes`])
//! instead; two lazy helpers cost more, and only when their call shapes occur
//! ([`OverlayView::build`] has the full cost):
//!
//! - **virtual nodes** — every symbol in a dirty file, addressed by virtual
//!   index `base_len + i` so they compose with the base graph's dense `u32`
//!   node-index space that edges and traversals already use.
//! - **replaced** — a dirty-file symbol whose uid (kind + path + owner +
//!   name) matches a base node: the base node's identity survives the edit,
//!   so traversal redirects base index → virtual index (spans/lines come
//!   from the on-disk version; base IN-edges from clean files stay valid).
//! - **suppressed** — base nodes of dirty files NOT re-emitted by the fresh
//!   parse: deleted or renamed. Traversal must neither expand into nor
//!   report them — this is what kills phantom callers after a rename. In a
//!   re-parsed file only kinds a fragment can carry are suppressed; nodes
//!   the graph builder makes (File, Route, EntryPoint, Process, detector
//!   nodes) stay visible as indexed, and their edges into dirty symbols
//!   redirect or drop like any other base edge. With the file's records
//!   ([`OverlayFileRecords`]) the replay rebuilds its record nodes
//!   (`PathLiteral`, `TransactionScope`, `SchemaField`, `EventTopic`): a
//!   base one pairs with its replayed twin or is suppressed. A removed file
//!   (deleted, or the old half of a rename: a whole-file tombstone in the
//!   manifest) has no parse at all: every base node of it is suppressed,
//!   builder-made ones included, and so every base edge into or out of it
//!   drops.
//! - **overlay edges** — `Calls` edges re-resolved from each dirty symbol's
//!   [`CallSite`](crate::analyzer::types::CallSite)s, mirroring index-time Pass-2 tier semantics (same-file
//!   with the caller's own member first → import-scoped → unique-global with
//!   `AmbiguousGlobal` suppression unless every candidate is a copy of one
//!   declaration in duplicated source trees, then the constructor fallback
//!   onto a Class / Struct's one constructor or the type) against the
//!   archived `name_index` (O(log N) per lookup, no allocation proportional
//!   to the graph). A nearest-copy edge keeps the index's
//!   [`NEAREST_COPY_REASON`] and a heuristic C# extension edge the index's
//!   [`csharp_extension::HEURISTIC_REASON`]; every other overlay call carries
//!   the uncommitted-edit marker.
//! - **closure references** — lexical `References` from each enclosing
//!   callable to its anonymous closures, using the full index's span rule.
//! - **enum-variant references** — `References` from each dirty symbol to
//!   the `EnumVariant`s its `variant_refs` name (`enum_variant:use` /
//!   `enum_variant:pattern`), by the index's `resolve_variant_ref`.
//! - **name references** — `ReadsField` for member and bare value reads,
//!   and `References` for function values, using the shared name-ref policy.
//! - **heritage** — `Extends` / `Implements` from each dirty declaration
//!   that lists a base (a Rust trait-impl method included) to the type each
//!   base spelling resolves to, by the index's Pass 2 rule
//!   ([`crate::analyzer::heritage::heritage_rel`]): each spelling looked up
//!   as the supertypes pre-pass looks it up, after the dirty supertypes
//!   exist, at the resolver's confidence.
//! - **per-file outputs** — the outputs a build makes from one file's parse
//!   alone, replayed by `session::incremental` with the index's own
//!   derivations: a `File` node for a path `graph.files` lacks (a new file,
//!   empty or not), scope `Defines` (`File` / `Namespace` / `Module` →
//!   member) and enum containment (`Enum` → `EnumVariant`), both by
//!   [`crate::analyzer::containment`], class membership (`HasMethod` /
//!   `HasProperty` from a type to its members) by
//!   [`crate::analyzer::membership`], `Imports` from the file's `File` node
//!   by [`crate::analyzer::import_edges`], and each virtual function's meta
//!   ([`OverlayView::function_meta`]). A re-parsed path that `graph.files`
//!   holds keeps its base `File` nodes, which source the file's rebuilt
//!   `Defines` and `Imports`: with the record edges below, the only overlay
//!   edges whose source is a base index. Membership `HasProperty` is rebuilt
//!   only with the records, as below. An `Imports` lookup runs the index's
//!   import resolver: its whole ladder without a call site, the named-import
//!   tier in every language outside the import-member policy (not the
//!   path-segment match of a call), candidate files without the Rust module
//!   trees, and the guess steps over every file with a `File` node. Each
//!   dirty declaration's `Decorates` edges come from its decorators
//!   ([`OverlaySymbol::decorators`]) by [`crate::analyzer::decorates`], each
//!   name looked up as an `Imports` name is, for a type. A name nothing
//!   declares reaches the base `Annotation` node of its full name, or a new
//!   virtual one with an empty path; a base one no file references any more
//!   is suppressed. Each dirty callable's `Overrides` edges come from
//!   [`crate::analyzer::overrides`]: its owner type and marker from the
//!   fresh parse, its signature from [`OverlaySymbol::params`] and
//!   [`OverlaySymbol::flags`], a dirty type's bases from its heritage
//!   resolved as the supertypes pre-pass resolves them, and a clean type's
//!   bases and members from `graph.bin` (below).
//! - **record outputs** — given each file's [`OverlayFileRecords`]
//!   ([`OverlayView::build_with_records`]), the replay rebuilds the record
//!   nodes and their edges by the index's derivations
//!   ([`crate::analyzer::record_nodes`]): `PathLiteral` + `UsesPathLiteral`,
//!   `QueriesTable`, `TransactionScope` + `OpensTxScope`, `SchemaField` +
//!   `HasProperty`, `EventTopic` + `Publishes` / `Subscribes`.
//!
//! The merge replaces only the base references of dirty sources whose
//! reason the overlay rebuilds (closure, overload set, enum variant, function value),
//! preserving every other References reason.
//!
//! ## The masking invariant: mask ⊆ rebuild
//!
//! A base edge whose **source** lies in a dirty file is masked ONLY when the
//! overlay re-resolves that rel type from fragment data (`Calls`,
//! `ReadsField`, `Defines`, `HasMethod`, `Imports`, `Decorates`,
//! `Overrides`, `Extends`, `Implements`, plus the `References` reasons
//! above, plus `HasProperty`, `UsesPathLiteral`, `QueriesTable`,
//! `OpensTxScope`, `Publishes` and `Subscribes` when the records replayed) —
//! for those the overlay adjacency is the file's truth. `Defines` and the
//! record edges are masked at the file's `File` nodes too, and `Imports`
//! there only. `HasProperty`
//! has two producers, class membership and the schema fields: masked by
//! kind, it needs both rebuilt, so without the records it stays as indexed
//! and a new member gets none. With them, a dirty file's base `SchemaField`
//! nodes are suppressed rather than paired. Rels the fragment carries no
//! inputs for (the builder-made nodes' edges below) keep their base edges:
//! slightly stale beats silently absent for a deterministic edge. A base
//! edge whose **target** lies in a dirty file is
//! redirected (replaced) or dropped (suppressed) regardless of rel.
//! Clean↔clean edges are untouched. Consumers apply this via
//! [`OverlayView::masks_base_edge`] + [`OverlayView::redirect`].
//!
//! ## Clean files: the invalidation closure
//!
//! A clean file that depends on a dirty change is resolved again by
//! [`Self::build_with_closure`](OverlayView::build_with_closure): the
//! caller re-parses each file [`super::closure`] names and hands its parse in
//! beside the dirty ones, so every output above is rebuilt for it too. A
//! clean file outside the closure keeps its indexed edges, as below, and
//! [`OverlayView::stale_closure`] names the kinds a closure left as indexed.
//!
//! ## What stays out of scope (documented fidelity gaps)
//!
//! - `References` edges other than lexical closure, overload-set,
//!   enum-variant, and function-reference edges are not re-resolved: a dirty file keeps the
//!   base ones of its surviving symbols (stale) and gains none for new code.
//! - Builder-made nodes of a re-parsed dirty file other than `File` and the
//!   record nodes (routes, entry points, processes) and their edges are as
//!   indexed, and so are all record outputs and membership `HasProperty`
//!   when the view is built without records. A path with two `LocalGraph`s
//!   keeps the base's count of `File` nodes, whatever count its parse now
//!   has. The controller `EntryPoint` nodes of a dirty file are as indexed.
//! - A clean file outside the closure keeps its `Overrides` edges: an edge
//!   into a dirty method the edit removed drops. A clean subclass whose
//!   supertype a dirty file changed is in the closure. A dirty callable's
//!   lookup reads a clean type as `graph.bin` holds it: its archived
//!   supertypes ([`ArchivedZeroCopyGraph::supertypes_of`]), each at the
//!   confidence of its archived `Extends` / `Implements` edge, and its
//!   methods by its archived `HasMethod` edges with their archived meta.
//!   `graph.bin` keeps no base spelling, so a clean base is never read as
//!   spelled with type arguments, a base the index found ambiguous reads
//!   as each of its candidates, and only the first unresolved base of a
//!   clean type is a gap.
//! - A dirty file's base resolves as the index's `resolve_base` resolves
//!   it, with three kept differences: one candidate at most (the index keeps
//!   each candidate of an ambiguous answer), a clean C# type taken at any
//!   type-argument count (`graph.bin` keeps no generic arity), and no
//!   partial-type global fallback (a C# base whose global candidates are
//!   the parts of one partial type binds nothing). A clean subtype of a
//!   type a dirty file changed is in the closure; outside it, an edge into
//!   a dirty type the edit removed drops.
//! - A clean decorator whose target (a declaration, or the shared
//!   `Annotation` node of its full name) is named like a name a dirty file
//!   adds or removes is in the closure. A dotted decorator whose last
//!   segment alone names the new declaration is not.
//! - A clean importer of a declaration a dirty file removed or changed, or
//!   whose archived import rows name a name the edit adds or removes, is in
//!   the closure. A Rust qualified import name reaches the qualifier tier,
//!   which reads the module trees where the index's import resolver has
//!   none.
//! - The heuristic mirror edges (`MirrorsField`, `EventTopicMirror`) are
//!   index-time: an `EventTopic` that pairs with its replayed twin keeps its
//!   base mirror edges, an unpaired or new one has none, and a dirty file's
//!   `SchemaField` has none. A clean `QueriesTable` edge to a table a dirty
//!   file now declares again is in the closure (the table's namesake); a
//!   clean SQL reference to a table no file declared before the edit bound
//!   nothing, records no dependency, and stays unbound.
//! - A function's meta in the view holds the flags and return type a
//!   fragment carries; the parameters and decorators of a dirty function are
//!   not in it (the `Overrides` and `Decorates` replays read them from the
//!   fragment).
//! - A dirty symbol's type-annotation `Accesses` edges stay as indexed: the
//!   fragment keeps no type annotation (FU-2026-10-26-0ee756874fb8).
//! - A removed path still counts as a file for the passes over
//!   `graph.files` (import-candidate probes, module and crate-root
//!   discovery), and its archived import rows still serve a re-export hop.
//!   Its declarations are gone, so a probe that lands in it binds nothing
//!   there.
//! - A clean call the index left ambiguous records no edge, so a removal
//!   that leaves one namesake of at least two cannot reach it:
//!   [`OverlayView::stale_closure`] names the name. A clean call to a name
//!   no file declared before the edit (an external) stays unbound.
//! - Routes, their `HandlesRoute` edges, the controller `EntryPoint` nodes
//!   routes in another file create, and the `Fetches` edges bound to a
//!   route are index-time: a dirty file that held a `Route` node makes
//!   [`OverlayView::stale_closure`] name them.
//! - TS / JS imports run the index's named-import tier through the shared
//!   rules ([`reexport`](crate::analyzer::reexport)): candidate probing with the path aliases the index
//!   resolved with (`graph.bin` keeps them), ES-module export eligibility,
//!   export aliases and the one-hop barrel re-export, over the archived
//!   import / export facts of clean files. An edit to `tsconfig.json` itself
//!   is not seen until a reindex.
//! - Outside the import-member languages and TS / JS, only a `./` / `../`
//!   import source probes the index's candidate files. Any other source (a
//!   package or `crate::` path) narrows by its last path segment, unique
//!   only, an approximation of the index's module tree; a namespace import
//!   through such a source binds nothing.
//! - The qualifier tier (`K.save`, `K::st`) is replayed with the module
//!   tree read as conventional file layout: a Rust `use` of the qualifier
//!   probes the module file and the import's candidate files as the index
//!   does without a tree, and a `use` whose head names no `crate` / `self` /
//!   `super` path nor a module of the caller's file (a workspace crate) binds
//!   nothing. Its module-file tier counts a base file that declares nothing
//!   (the index skips it), which can only leave a call unresolved.
//! - The same-file pick (`prefer_owned`), the receiver-typing ladder and the
//!   call-site rules of `Resolver::resolve_call` (type receivers, untyped
//!   member calls, the constructor-fallback gate) share their code with the
//!   index ([`crate::analyzer::same_file_pick`],
//!   [`crate::analyzer::type_ladder`]). Supertypes come from `graph.bin`
//!   for clean types and from a pre-pass over the dirty files for theirs; a
//!   clean type keeps the bases a dirty file declared for it before the
//!   edit, as `graph.bin` does not say which file declared a base. A base
//!   Rust method of an `impl` the parser emits no `Impl` node for (`impl T
//!   for &mut X`) reads as free in the same-file pick.
//! - Rust module paths resolve through the module-tree rule the index runs
//!   ([`crate::analyzer::ladder::module_tree_member`]), with crate roots from
//!   the crate table graph.bin stores; a tree with no `Cargo.toml` has no
//!   module tree. `#[path]` and inline modules stay unresolved, and a `pub
//!   use` chain the index follows through an import's visibility binds only
//!   in the index: graph.bin does not store that visibility.

use super::closure::StaleClosure;
use super::incremental::{self, ReplayInputs};
use crate::analyzer::containment::{ENUM_VARIANT_DEFINES_REASON, SCOPE_DEFINES_REASON};
use crate::analyzer::csharp_extension;
use crate::analyzer::decorates::DECORATES_REASON;
use crate::analyzer::heritage::HeritageNode;
use crate::analyzer::heritage::HERITAGE_REASONS;
use crate::analyzer::import_edges::ImportReason;
use crate::analyzer::lexical::{
    enclosing_types, file_identities, later_is_arity_twin, splits_by_arity,
};
use crate::analyzer::membership::{CLASS_MEMBERSHIP_IMPL_REASON, CLASS_MEMBERSHIP_REASON};
use crate::analyzer::outcome::Tier;
use crate::analyzer::overrides::OVERRIDES_REASONS;
use crate::analyzer::record_nodes::{
    EVENT_TOPIC_PUBLISHES_REASON, EVENT_TOPIC_SUBSCRIBES_REASON, PATH_LITERAL_SINK_REASONS,
    SCHEMA_FIELD_HAS_PROPERTY_REASON, TX_SCOPE_REASON,
};
use crate::analyzer::types::{
    innermost_enclosing, owner_key, LocalGraph, RawEventTopic, RawExportAlias, RawImport,
    RawPathLiteral, RawSchemaField, RawSqlRef, RawTxScope, Span, CLOSURE_REFERENCE_REASON,
    FUNCTION_REFERENCE_REASON, NEAREST_COPY_REASON, OVERLOAD_SET_REASON, VALUE_READ_REASON,
    VARIANT_PATTERN_REASON, VARIANT_USE_REASON,
};
use crate::contracts::identity::{pair_by_anchor, DeclIdentity, UidSlot};
use crate::contracts::routing::ParsedLanguage;
use crate::file_category::{FileMeta, Language};
use crate::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::sync::Arc;

mod resolution;

/// One symbol from a freshly parsed dirty file.
#[derive(Debug, Clone)]
pub struct OverlaySymbol {
    pub name: String,
    pub kind: NodeKind,
    pub owner_class: Option<String>,
    /// 1-based, matching `Node::start_line` conventions.
    pub start_line: u32,
    pub end_line: u32,
    /// 0-based columns from the fragment, preserving same-line containment.
    pub start_column: u32,
    pub end_column: u32,
    /// `RawNode.calls` of this symbol: callee short names, some encoded as a
    /// [`CallSite`](crate::analyzer::types::CallSite) (read them through
    /// [`CallSite::parse`](crate::analyzer::types::CallSite::parse)).
    pub calls: Vec<String>,
    /// `RawNode.is_exported`: in an ES module, only an exported declaration
    /// is another file's import target.
    pub is_exported: bool,
    /// `RawNode.heritage`: the declared base names, as the parser wrote
    /// them. Empty for a non-type.
    pub heritage: Vec<String>,
    /// Every raw `RawNode.field_reads` entry. The legacy name stays public
    /// for callers that used it before tagged name references existed.
    pub variant_refs: Vec<String>,
    /// `RawFunctionMeta.return_type` of a function, method or constructor,
    /// as written; `None` when it declares none or the input does not carry
    /// it (a pre-V5 fragment). Types the receiver of a chained call on it.
    pub return_type: Option<String>,
    /// [`RawNode::generic_arity`](crate::analyzer::types::RawNode::generic_arity):
    /// the declared generic arity of a C# type, 0 for every other node. The
    /// identity numbering reads it ([`file_identities`]), so a dirty C#
    /// `Conv<T>` beside `Conv` takes the full build's uid.
    pub generic_arity: u32,
    /// `RawFunctionMeta.flags` of a function, method or constructor: static,
    /// async, visibility and extension bits. `None` when the parse paired no
    /// meta with the node, or the input does not carry it (a pre-V7
    /// fragment). `Some(0)` is a real value (public instance), not absence.
    pub flags: Option<u16>,
    /// `RawNode.decorators` as the parser wrote them, parser markers
    /// included ([`RawNode::is_parser_marker`](crate::analyzer::types::RawNode::is_parser_marker)):
    /// the replay rebuilds the node's `Decorates` edges from them
    /// ([`crate::analyzer::decorates`]), and the override sentinel marks a
    /// declared override. Empty when the node has none.
    pub decorators: Vec<String>,
    /// `RawFunctionMeta.params` of a function, method or constructor, flat
    /// as `[name1, type1, name2, type2, ...]` with an empty type where the
    /// source writes none: the `Overrides` replay
    /// ([`crate::analyzer::overrides`]) matches signatures on them. Empty
    /// when the parse paired no meta with the node ([`Self::flags`] is then
    /// `None`) or the declaration takes no parameter.
    pub params: Vec<String>,
}

impl crate::analyzer::overrides::OverrideNode for OverlaySymbol {
    fn name(&self) -> &str {
        &self.name
    }

    fn owner_class(&self) -> Option<&str> {
        self.owner_class.as_deref()
    }
}

impl HeritageNode for OverlaySymbol {
    fn kind(&self) -> NodeKind {
        self.kind
    }

    fn span(&self) -> (u32, u32, u32, u32) {
        (
            self.start_line.saturating_sub(1),
            self.start_column,
            self.end_line.saturating_sub(1),
            self.end_column,
        )
    }

    fn heritage(&self) -> &[String] {
        &self.heritage
    }
}

/// One dirty file's fresh parse, as loaded from a fragment bin. Its imports
/// and exports replace the archived ones of the same file, empty included.
#[derive(Debug, Clone)]
pub struct OverlayFileInput {
    /// Repo-relative path, same normalization as `File.path` in the graph.
    pub rel_path: String,
    pub symbols: Vec<OverlaySymbol>,
    pub imports: Vec<RawImport>,
    /// `LocalGraph.export_aliases`, empty when the file has none.
    pub export_aliases: Vec<RawExportAlias>,
    /// The C5 language after parse that this parse reported: the value the
    /// index would store in `File.parsed_language`. The overlay reads the
    /// file's language from it, so a Vue file with a TypeScript script
    /// resolves as TypeScript on both sides. `None` when no parse decided
    /// one (an omitted file, a hand-built input, a fragment that stored
    /// `ParsedLanguage::ABSENT`): the path then decides, as the builder does
    /// for a record that stores none.
    pub parsed_language: Option<ParsedLanguage>,
}

impl OverlayFileInput {
    /// The [`FileMeta`] the index would record for this file: the language
    /// after parse when the parse reported one, else the path's.
    /// `rel_path` is forward-slashed.
    pub fn file_meta(&self) -> FileMeta {
        match self.parsed_language {
            Some(parsed) => FileMeta::of_parsed(&self.rel_path, parsed),
            None => FileMeta::from_normalized_path(&self.rel_path),
        }
    }
}

impl From<LocalGraph> for OverlayFileInput {
    fn from(local: LocalGraph) -> Self {
        let parsed_language = ParsedLanguage::from_bytes(local.parsed_language);
        // Paired by span, as the index's Pass 1.8 pairs them.
        let metas: FxHashMap<_, _> = local
            .raw_function_metas
            .into_iter()
            .map(|meta| (meta.span, meta))
            .collect();
        Self {
            rel_path: local.file_path.to_string_lossy().replace('\\', "/"),
            symbols: local
                .nodes
                .into_iter()
                .map(|n| {
                    let is_callable = matches!(
                        n.kind,
                        NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                    );
                    let meta = is_callable.then(|| metas.get(&n.span)).flatten();
                    OverlaySymbol {
                        return_type: meta
                            .map(|meta| meta.return_type.clone())
                            .filter(|return_type| !return_type.is_empty()),
                        flags: meta.map(|meta| meta.flags),
                        params: meta.map(|meta| meta.params.clone()).unwrap_or_default(),
                        generic_arity: n.generic_arity(),
                        name: n.name,
                        kind: n.kind,
                        owner_class: n.owner_class,
                        start_line: n.span.0 + 1,
                        end_line: n.span.2 + 1,
                        start_column: n.span.1,
                        end_column: n.span.3,
                        calls: n.calls,
                        is_exported: n.is_exported,
                        heritage: n.heritage,
                        variant_refs: n.field_reads,
                        decorators: n.decorators,
                    }
                })
                .collect(),
            imports: local.imports,
            export_aliases: local.export_aliases.map(Vec::from).unwrap_or_default(),
            parsed_language,
        }
    }
}

/// One dirty file's raw records, the inputs of the record outputs the
/// per-file replay rebuilds (`session::incremental`). Each record carries
/// its endpoint, bound against the file's raw nodes when the fragment is
/// written; [`OverlaySymbol`]s are those raw nodes in order, so a `Node`
/// endpoint indexes [`OverlayFileInput::symbols`].
#[derive(Debug, Clone, Default, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
pub struct OverlayFileRecords {
    pub path_literals: Vec<RawPathLiteral>,
    pub sql_refs: Vec<RawSqlRef>,
    pub tx_scopes: Vec<RawTxScope>,
    pub schema_fields: Vec<RawSchemaField>,
    pub event_topics: Vec<RawEventTopic>,
}

impl OverlayFileRecords {
    /// The records of `local`, each endpoint bound as the index binds it
    /// (`endpoint_in`).
    pub fn from_local(local: &LocalGraph) -> Self {
        let nodes = &local.nodes;
        Self {
            path_literals: local
                .path_literals
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|r| RawPathLiteral {
                    endpoint: r.endpoint_in(nodes),
                    ..r.clone()
                })
                .collect(),
            sql_refs: local
                .sql_refs
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|r| RawSqlRef {
                    endpoint: r.endpoint_in(nodes),
                    ..r.clone()
                })
                .collect(),
            tx_scopes: local.tx_scopes.as_deref().unwrap_or_default().to_vec(),
            schema_fields: local
                .schema_fields
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|r| RawSchemaField {
                    endpoint: r.endpoint_in(nodes),
                    ..r.clone()
                })
                .collect(),
            event_topics: local
                .event_topics
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|r| RawEventTopic {
                    endpoint: r.endpoint_in(nodes),
                    ..r.clone()
                })
                .collect(),
        }
    }
}

/// A materialized overlay node. `rel_path` is shared per file via `Arc`.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewNode {
    pub uid: u64,
    pub name: String,
    pub kind: NodeKind,
    pub owner_class: Option<String>,
    pub rel_path: Arc<str>,
    pub start_line: u32,
    pub end_line: u32,
    /// 0-based, from the fragment: same-line containment needs them.
    pub start_column: u32,
    pub end_column: u32,
    /// `Some(base_idx)` when this node replaces a surviving base node —
    /// traversal reads the base CSR IN-edges of `base_idx` (clean callers)
    /// in addition to [`OverlayView::overlay_in`].
    pub replaced_base: Option<u32>,
    /// A later overload ([`crate::uid::keeps_overloads`]), or a later C#
    /// type of another generic arity
    /// ([`crate::analyzer::lexical::splits_by_arity`]): the index keeps it as
    /// a node under its own uid but never binds it by name.
    pub later_overload: bool,
}

impl ViewNode {
    /// False for a node no single file owns: a new `Annotation` node the
    /// replay makes for a decorator nothing declares, which has an empty
    /// `rel_path`. The mirror of [`crate::graph::Node::has_owning_file`]:
    /// a consumer that reports or reads a node's file skips such a node.
    pub fn has_owning_file(&self) -> bool {
        !self.rel_path.is_empty()
    }
}

/// An overlay-resolved edge. Indices are merged-space: `< base_len` = base
/// node, `>= base_len` = virtual node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewEdge {
    pub source: u32,
    pub target: u32,
    pub rel_type: RelType,
    pub confidence: f32,
    pub reason: &'static str,
}

/// The function meta of a virtual node: the part of the index's
/// `FunctionMeta` a fragment carries ([`OverlaySymbol::flags`],
/// [`OverlaySymbol::return_type`]). See [`OverlayView::function_meta`].
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ViewFunctionMeta {
    /// The virtual index of the function, method or constructor.
    pub node: u32,
    /// `FunctionMeta::flags`, bit for bit.
    pub flags: u16,
    /// The return type as written; `None` when the declaration writes none.
    pub return_type: Option<String>,
}

/// Tier confidences mirroring index-time Pass-2 resolution.
const CONF_IMPORT_SCOPED: f32 = Tier::ImportScoped.confidence();

#[derive(Debug, Default, PartialEq)]
pub struct OverlayView {
    base_len: u32,
    nodes: Vec<ViewNode>,
    /// base idx → virtual idx for surviving symbols, paired by anchor
    /// ([`pair_with_base`]).
    replaced: FxHashMap<u32, u32>,
    /// base idxs of dirty-file symbols NOT re-emitted (deleted/renamed),
    /// the base record nodes of a re-parsed file the replay did not pair,
    /// every base idx of a removed file, and each base `Annotation` node no
    /// file references after the edit.
    suppressed: FxHashSet<u32>,
    /// every base idx of a re-parsed dirty file whose kind
    /// [`fragment_emits`], and every base idx of a removed file
    /// (replaced ∪ suppressed): base edges sourced here are masked in
    /// favour of overlay adjacency, and no resolution tier binds them.
    /// Builder-made nodes (File, Route, …) of a re-parsed file stay outside
    /// it, visible and slightly stale.
    dirty_base: FxHashSet<u32>,
    /// Flat overlay-edge store; adjacency maps hold indices into it so
    /// consumers (cypher edge vars) can address an overlay edge by a stable
    /// virtual edge index (`graph.edges.len() + i`).
    edges: Vec<ViewEdge>,
    out_adj: FxHashMap<u32, Vec<u32>>,
    in_adj: FxHashMap<u32, Vec<u32>>,
    /// Virtual Class / Struct idx → the virtual constructors of its file
    /// whose [`owner_key`] is its name, in source order. Types with none have
    /// no entry.
    type_ctors: FxHashMap<u32, Vec<u32>>,
    /// Scoped virtual idx → its lexical parent's virtual idx, from the fresh
    /// parse by the index's one derivation
    /// ([`lexical_parents`](crate::contracts::identity::lexical_parents)). The
    /// graph's table has no row for a virtual node, so the merged display
    /// owner reads these. Empty when no dirty file's language scopes a kind
    /// ([`lexical_scope_kinds`](crate::analyzer::lexical::lexical_scope_kinds)).
    lexical_parents: FxHashMap<u32, u32>,
    /// Base declarations of a dirty (not removed) path that the pairing left
    /// unpaired although the parse still names their anchor, grouped by
    /// (path, kind, owner, name) in graph order. Their nodes are suppressed,
    /// so every edge into them is gone from the merged graph. A deleted
    /// declaration is not in it: its anchor is absent from the parse.
    hidden: Vec<HiddenDecl>,
    /// The calls the overlay could not resolve as a full build would, in
    /// discovery order, each once.
    gaps: Vec<CallGap>,
    /// The base `File` node of each re-parsed path whose `Defines` the
    /// per-file replay ([`incremental`]) rebuilt: its base `Defines` are
    /// masked, and its overlay ones are the only overlay edges with a base
    /// source.
    defines_files: FxHashSet<u32>,
    /// Each virtual function, method or constructor whose fragment paired a
    /// meta, ascending by node ([`Self::function_meta`]).
    function_metas: Vec<ViewFunctionMeta>,
    /// Whether the replay rebuilt the record outputs
    /// ([`Self::build_with_records`]): the base record edges of the dirty
    /// files are then masked.
    records_replayed: bool,
    /// The clean-file edge kinds the invalidation closure left as indexed
    /// ([`Self::stale_closure`]).
    stale_closure: StaleClosure,
}

/// A group of base declarations the overlay could not pair by identity: the
/// same (path, kind, owner, name) with no `Signature` discriminator to tell
/// the members apart. See [`OverlayView::hidden_declarations`].
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct HiddenDecl {
    pub path: String,
    pub kind: NodeKind,
    /// Empty when the declaration has no owner.
    pub owner: String,
    pub name: String,
    /// How many base declarations the group hides; at least 1.
    pub count: u32,
}

/// [`CallGap::reason`] of a dirty C# caller: a dirty C# file declares a
/// method named like the call's member, and the overlay cannot read its
/// `FunctionMeta` flags, so the extension retry binds nothing.
pub const GAP_CS_EXTENSION_DIRTY_DECLARATION: &str = "cs-extension-dirty-declaration";

/// [`CallGap::reason`] of a clean C# caller: its indexed call edge reaches a
/// C# extension method named like a method a dirty C# file declares. The
/// overlay does not re-resolve a clean caller, so the edge may be stale.
pub const GAP_CS_EXTENSION_STALE_EDGE: &str = "cs-extension-stale-edge";

/// The strictest `FunctionMeta::flags` the C# access rule reads: a
/// `private static` method that is no extension (visibility code 2,
/// `csharp_access::PRIVATE`, in bits 6-8). A call the rule admits under
/// them, it admits under any flags.
const CS_STRICTEST_FLAGS: u16 = crate::graph::FunctionMeta::FLAG_STATIC | (2 << 6);

/// [`CallGap::reason`] of a dirty C# caller: the C# access rule judged a
/// method of a dirty file ([`csharp_access`](crate::analyzer::csharp_access)), and its verdict rests on
/// flags the overlay cannot read. A dirty file's input carries no flags of
/// its own: a method that replaced a base declaration is judged by that
/// declaration's flags, which the edit may have changed, and an overload,
/// whose anchor repeats and pairs with no base declaration, by none. A
/// verdict that every flag value gives (a bare call from the method's own
/// type) records no gap.
pub const GAP_CS_ACCESS_DIRTY_TARGET: &str = "cs-access-dirty-target";

/// [`CallGap::reason`] of a dirty C# caller: a bare call from a type the
/// overlay shows no `partial` parts, while a C# type of the same name, kind
/// and scope in another file owns or inherits the member. The overlay keeps
/// the last index's grouping, so a part an edit added, or a type an edit
/// made `partial`, stays a type of its own; a full build may merge them and
/// bind the member at the partial-type tier.
pub const GAP_CS_PARTIAL_BARE_CALL: &str = "cs-partial-bare-call";

/// A call the overlay could not resolve as a full build would (codex ruling
/// P-ext2 B and C). See [`OverlayView::call_gaps`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct CallGap {
    /// The caller's repo-relative file.
    pub path: String,
    /// The caller's 1-based start line.
    pub line: u32,
    /// The caller's name.
    pub caller: String,
    /// The member the call names.
    pub member: String,
    /// [`GAP_CS_EXTENSION_DIRTY_DECLARATION`],
    /// [`GAP_CS_EXTENSION_STALE_EDGE`], [`GAP_CS_ACCESS_DIRTY_TARGET`] or
    /// [`GAP_CS_PARTIAL_BARE_CALL`].
    pub reason: String,
}

/// A dirty C# file declares a method named like the call's member
/// ([`GAP_CS_EXTENSION_DIRTY_DECLARATION`]).
#[derive(Debug, Clone, Copy)]
struct DirtyNamesake;

impl OverlayView {
    /// Build the view. Returns `None` when `files` is empty — the clean-tree
    /// path must stay literally view-free so traversals take their original
    /// branch. Cost when dirty: one O(files) pass over `graph.files` plus one
    /// O(nodes) pass over `graph.nodes` (pure scans; the view's own
    /// allocation stays O(dirty symbols)), then O(dirty symbols × callees ×
    /// log N) name-index lookups. Two lazy helpers add more:
    /// - a Python/Java/Kotlin/PHP call through an import builds, once per
    ///   such language, a file index over `graph.files` that allocates
    ///   O(files of that language) for module discovery;
    /// - a dirty Rust file with a `::` call or variant path makes one more
    ///   pass over `graph.files` for crate roots;
    /// - the path-alias table is rebuilt from `graph.bin` once per build;
    /// - an ambiguous Tier-3 call whose first two candidates are copies of
    ///   one Java / Kotlin / PHP declaration builds that language's file
    ///   index too, to compare their files' namespaces.
    ///
    /// The shared import rules scan nothing: a TS / JS import probes its
    /// candidate paths by binary search over `graph.files`, reads one
    /// module's archived import rows per re-export hop, and looks
    /// declarations up through the name index. The per-file replay
    /// (`session::incremental`) costs O(dirty symbols) on top: the `File`
    /// nodes it needs come from the node scan above.
    pub fn build(graph: &ArchivedZeroCopyGraph, files: &[OverlayFileInput]) -> Option<Self> {
        Self::build_with_removed(graph, files, &[])
    }

    /// [`Self::build`] for a session that also removed files. `removed`
    /// holds repo-relative paths, normalized as `File.path`, that are gone
    /// from the worktree: a deleted file, or the old half of a rename (the
    /// manifest's whole-file tombstones). Every base node of such a path is
    /// suppressed, builder-made ones included, so no traversal reports it or
    /// follows an edge into or out of it. A path that also has an entry in
    /// `files` exists on disk, and its parse wins. `None` when both lists
    /// are empty. Without a removed path the cost is [`Self::build`]'s: the
    /// empty sets allocate nothing. The record outputs stay as indexed
    /// ([`Self::build_with_records`] rebuilds them).
    pub fn build_with_removed(
        graph: &ArchivedZeroCopyGraph,
        files: &[OverlayFileInput],
        removed: &[String],
    ) -> Option<Self> {
        Self::build_with_records(graph, files, &[], removed)
    }

    /// [`Self::build_with_removed`] that also rebuilds each dirty file's
    /// record outputs from `records`, one per input of `files` in order
    /// (`session::incremental`). Any other length rebuilds none of them, as
    /// an empty `records` does. Cost on top: O(dirty records + base record
    /// nodes of the dirty paths) and one name-index probe per distinct SQL
    /// table name; the base record nodes come from the node scan below.
    pub fn build_with_records(
        graph: &ArchivedZeroCopyGraph,
        files: &[OverlayFileInput],
        records: &[OverlayFileRecords],
        removed: &[String],
    ) -> Option<Self> {
        if files.is_empty() && removed.is_empty() {
            return None;
        }
        let base_len = graph.nodes.len() as u32;

        // ── dirty and removed file_idx sets ───────────────────────────────
        let rel_paths: FxHashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        let removed_paths: FxHashSet<&str> = removed.iter().map(String::as_str).collect();
        // A path can own several `graph.files` entries (two `LocalGraph`s
        // of one path), so the dirty map keys each entry to its path.
        let mut dirty_file_idx: FxHashMap<u32, &str> = FxHashMap::default();
        let mut removed_file_idx: FxHashSet<u32> = FxHashSet::default();
        for (i, f) in graph.files.iter().enumerate() {
            let path = f.path.resolve(&graph.string_pool);
            if rel_paths.contains(path) {
                dirty_file_idx.insert(i as u32, path);
            } else if removed_paths.contains(path) {
                removed_file_idx.insert(i as u32);
            }
        }

        // ── base nodes living in dirty files ──────────────────────────────
        // Single O(N) scan, collecting only matches, per path in graph
        // order: the virtual-node pass pairs each path's base declarations
        // with its parse by anchor (`pair_by_anchor`). Only kinds a fragment
        // can re-emit are collected: mask ⊆ rebuild applies to nodes too. A
        // removed file has no parse to re-emit anything, so all of its nodes
        // are collected; no parse names its path, so no virtual node
        // replaces one. The same scan keeps each re-parsed path's `File`
        // nodes, the sources of the `Defines` and record edges the replay
        // rebuilds, and its record nodes, which the replay pairs.
        let mut dirty_base_by_path: FxHashMap<&str, Vec<u32>> = FxHashMap::default();
        let mut dirty_base: FxHashSet<u32> = FxHashSet::default();
        let mut base_files: FxHashMap<&str, Vec<u32>> = FxHashMap::default();
        let mut base_records: Vec<u32> = Vec::new();
        if !dirty_file_idx.is_empty() || !removed_file_idx.is_empty() {
            for (i, node) in graph.nodes.iter().enumerate() {
                let file_idx = node.file_idx.to_native();
                if let Some(&path) = dirty_file_idx.get(&file_idx) {
                    let kind = NodeKind::from(&node.kind);
                    if fragment_emits(kind) {
                        dirty_base_by_path.entry(path).or_default().push(i as u32);
                        dirty_base.insert(i as u32);
                    } else if kind == NodeKind::File {
                        base_files.entry(path).or_default().push(i as u32);
                    } else if incremental::replays_record_kind(kind) {
                        base_records.push(i as u32);
                    }
                } else if removed_file_idx.contains(&file_idx) {
                    dirty_base.insert(i as u32);
                }
            }
        }

        // ── virtual nodes + replaced mapping ──────────────────────────────
        let mut nodes: Vec<ViewNode> = Vec::new();
        let mut replaced: FxHashMap<u32, u32> = FxHashMap::default();
        // First virtual idx → its later overloads, as the index's
        // `SymbolTable::register_overload` numbers them.
        let mut later_overloads: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        // Virtual offsets of the later same-uid declarations the index
        // tombstones. They keep their first's uid and name until every pass
        // below has read them as its twin, then take the tombstone's shape.
        let mut tombstones: Vec<usize> = Vec::new();
        // Each tombstone's virtual idx with its survivor's: the slot pairs
        // of the replay's `RawIndexMap`.
        let mut survivors: Vec<(u32, u32)> = Vec::new();
        let mut unpaired_base: FxHashSet<u32> = FxHashSet::default();
        // First virtual idx of a member with arity twins (`Foo1.foo` beside
        // `Foo1<T>.foo`) → its file's input position, and the first and each
        // twin with the span of the type that declares it: the index's
        // `SymbolTable::twin_read_from`.
        let mut member_twins: resolution::MemberTwins = FxHashMap::default();

        for (file_ord, file) in files.iter().enumerate() {
            let rel: Arc<str> = Arc::from(file.rel_path.as_str());
            let file_start = base_len + nodes.len() as u32;
            // The index's numbering (`assign_file`), from the same fields.
            let identities = file_identities(&file.rel_path, &file.symbols, |sym| {
                (
                    sym.kind,
                    sym.owner_class.as_deref(),
                    sym.name.as_str(),
                    HeritageNode::span(sym),
                    sym.generic_arity,
                )
            });
            let (pairs, unpaired) = pair_with_base(
                graph,
                &file.rel_path,
                dirty_base_by_path
                    .get(file.rel_path.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                &identities,
            );
            unpaired_base.extend(unpaired);
            let language = file.file_meta().language;
            // The innermost type around each symbol, computed on the file's
            // first member twin only.
            let mut declaring_types: Option<Vec<Option<usize>>> = None;
            for (offset, ((sym, identity), replaced_base)) in
                file.symbols.iter().zip(identities).zip(pairs).enumerate()
            {
                let virt = base_len + nodes.len() as u32;
                let later_overload = match identity.slot {
                    // An arity twin: a C# type or delegate of another
                    // generic arity, or a member such a type declares. Like
                    // an overload, the index binds it by id only, never by
                    // name, but it joins no overload set
                    // (`register_arity_sibling`).
                    UidSlot::Later { first, .. } if later_is_arity_twin(language, sym.kind) => {
                        if !splits_by_arity(language, sym.kind) {
                            let types = declaring_types.get_or_insert_with(|| {
                                let rows: Vec<(NodeKind, Span)> = file
                                    .symbols
                                    .iter()
                                    .map(|s| (s.kind, HeritageNode::span(s)))
                                    .collect();
                                enclosing_types(&rows)
                            });
                            let scope = |raw: usize| {
                                types[raw].map(|ty| HeritageNode::span(&file.symbols[ty]))
                            };
                            let (_, set) =
                                member_twins.entry(file_start + first).or_insert_with(|| {
                                    let first_scope = scope(first as usize)
                                        .map(|span| (file_start + first, span));
                                    (file_ord, first_scope.into_iter().collect())
                                });
                            set.extend(scope(offset).map(|span| (virt, span)));
                        }
                        true
                    }
                    UidSlot::Later { first, .. } => {
                        later_overloads
                            .entry(file_start + first)
                            .or_default()
                            .push(virt);
                        true
                    }
                    UidSlot::Duplicate { first, .. } => {
                        tombstones.push(nodes.len());
                        survivors.push((virt, file_start + first));
                        false
                    }
                    UidSlot::First(_) => false,
                };
                let uid = identity.slot.uid();
                // Two inputs of one path both pair a base node: the first
                // one replaces it.
                if let Some(base_idx) = replaced_base {
                    replaced.entry(base_idx).or_insert(virt);
                }
                nodes.push(ViewNode {
                    uid,
                    name: sym.name.clone(),
                    kind: sym.kind,
                    owner_class: sym.owner_class.clone(),
                    rel_path: rel.clone(),
                    start_line: sym.start_line,
                    end_line: sym.end_line,
                    start_column: sym.start_column,
                    end_column: sym.end_column,
                    replaced_base,
                    later_overload,
                });
            }
        }
        let mut suppressed: FxHashSet<u32> = dirty_base
            .iter()
            .copied()
            .filter(|idx| !replaced.contains_key(idx))
            .collect();
        // Two inputs of one path can pair different halves of its base, so
        // the unpaired set is final only once `replaced` is.
        let mut hidden_base: Vec<u32> = unpaired_base
            .into_iter()
            .filter(|idx| !replaced.contains_key(idx))
            .collect();
        hidden_base.sort_unstable();
        let hidden = hidden_declarations(graph, &hidden_base);

        let type_ctors = virtual_constructors(files, &nodes, base_len);

        // Each dirty path's `File` node, numbered as the replay below numbers
        // a new path's: the `Imports` edges start there.
        let indexed: FxHashSet<&str> = dirty_file_idx.values().copied().collect();
        let first_new = base_len + nodes.len() as u32;
        let dirty_files = incremental::dirty_file_nodes(files, &indexed, &base_files, first_new);
        // The `Annotation` nodes the `Decorates` edges reach and the base
        // lacks follow those `File` nodes.
        let mut decorated = resolution::Decorated {
            survivors: &survivors,
            first_annotation: incremental::first_annotation(files, &indexed, first_new),
            edges: Vec::new(),
            annotations: Vec::new(),
        };

        // ── overlay Calls and lexical closure References ──────────────────
        // The resolution submodule binds them. Its name maps borrow `nodes`'
        // strings and drop when it returns, before `nodes` moves into Self.
        let mut edges: Vec<ViewEdge> = Vec::new();
        let mut out_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        let mut in_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        let gaps: RefCell<Vec<CallGap>> = RefCell::new(Vec::new());
        let mut imports: Vec<ViewEdge> = Vec::new();
        let mut overrides: Vec<ViewEdge> = Vec::new();
        let lexical_parents = resolution::resolve_edges(
            graph,
            files,
            base_len,
            &nodes,
            &replaced,
            &dirty_base,
            &removed_paths,
            &type_ctors,
            &later_overloads,
            &member_twins,
            &mut edges,
            &mut out_adj,
            &mut in_adj,
            &gaps,
            (&dirty_files, &mut imports),
            &mut decorated,
            &mut overrides,
        );
        let resolution::Decorated {
            edges: decorates,
            annotations,
            ..
        } = decorated;

        let mut gaps = gaps.into_inner();
        gaps.extend(stale_extension_gaps(graph, files, &dirty_base, &suppressed));
        dedup_gaps(&mut gaps);

        // The index's tombstone (`pass1_register_nodes`): the slot stays,
        // with the bit-inverse uid and no name or owner.
        for offset in tombstones {
            let node = &mut nodes[offset];
            node.uid = crate::uid::tombstone(node.uid);
            node.name.clear();
            node.owner_class = None;
        }

        // ── per-file outputs: File nodes, scope Defines, enum containment,
        // function meta ───────────────────────────────────────────────────
        let replayed = incremental::replay(
            ReplayInputs {
                graph,
                files,
                records,
                base_len,
                tombstones: survivors,
                indexed,
                base_files,
                dirty_files: &dirty_files,
                imports,
                decorates,
                annotations,
                overrides,
                base_records,
                dirty_base: &dirty_base,
                lexical_parents: &lexical_parents,
                later_overloads: &later_overloads,
            },
            &mut nodes,
            &mut edges,
            &mut out_adj,
            &mut in_adj,
        );
        replaced.extend(replayed.replaced);
        suppressed.extend(replayed.suppressed);

        Some(Self {
            base_len,
            nodes,
            replaced,
            suppressed,
            dirty_base,
            edges,
            out_adj,
            in_adj,
            type_ctors,
            lexical_parents,
            hidden,
            gaps,
            defines_files: replayed.defines_files,
            function_metas: replayed.function_metas,
            records_replayed: replayed.records_replayed,
            stale_closure: StaleClosure::default(),
        })
    }

    /// [`Self::build_with_records`] over the dirty files and the clean
    /// files of their invalidation closure ([`super::closure`]), whose
    /// parses `files` and `records` hold beside the dirty ones: the view
    /// resolves a closure file as it resolves a dirty one. `stale` is what
    /// the closure left as indexed; the view carries it for the payload's
    /// caveat ([`Self::stale_closure`]). No other cost.
    pub fn build_with_closure(
        graph: &ArchivedZeroCopyGraph,
        files: &[OverlayFileInput],
        records: &[OverlayFileRecords],
        removed: &[String],
        stale: &StaleClosure,
    ) -> Option<Self> {
        let mut view = Self::build_with_records(graph, files, records, removed)?;
        view.stale_closure = stale.clone();
        Some(view)
    }

    /// The clean-file edges this view may hold as indexed: the kinds and
    /// names the invalidation closure could not re-resolve (an unbounded
    /// closure, a dependency `graph.bin` does not record, a dirty route
    /// file, or no closure for this dirty state). Empty when the closure
    /// re-resolved every dependent it found, and for a view built without
    /// one ([`Self::build_with_records`]).
    pub fn stale_closure(&self) -> &StaleClosure {
        &self.stale_closure
    }

    pub fn base_len(&self) -> u32 {
        self.base_len
    }

    /// The base declarations of a re-parsed path that the pairing could not
    /// match by identity and so suppressed, grouped by (path, kind, owner,
    /// name). Empty for a normal deletion, a removed path and a tombstone.
    pub fn hidden_declarations(&self) -> &[HiddenDecl] {
        &self.hidden
    }

    /// The calls whose overlay edge may differ from a full build's, with
    /// the reason ([`CallGap`]). Empty when every call resolved as the
    /// index would.
    pub fn call_gaps(&self) -> &[CallGap] {
        &self.gaps
    }

    /// All virtual nodes; index `i` here is merged index `base_len + i`.
    pub fn virtual_nodes(&self) -> &[ViewNode] {
        &self.nodes
    }

    /// Virtual-index accessor. `None` for base indices.
    pub fn node(&self, idx: u32) -> Option<&ViewNode> {
        idx.checked_sub(self.base_len)
            .and_then(|i| self.nodes.get(i as usize))
    }

    /// Where traversal should actually go when it lands on `idx`:
    /// suppressed base → `None` (node no longer exists on disk), replaced
    /// base → its virtual successor, anything else → itself.
    pub fn redirect(&self, idx: u32) -> Option<u32> {
        if self.suppressed.contains(&idx) {
            return None;
        }
        Some(self.replaced.get(&idx).copied().unwrap_or(idx))
    }

    /// True when a base edge of type `rel` SOURCED at `base_idx` must be
    /// ignored: the owning file was re-parsed or removed AND the overlay
    /// rebuilds this rel (`overlay_out` carries its truth). Masking a rel
    /// the overlay can't rebuild would silently drop deterministic edges —
    /// keep mask ⊆ rebuild as fragment capabilities grow. `Defines` is
    /// rebuilt per file (`session::incremental`), so it is masked at the
    /// dirty file's declarations and at its `File` nodes; so are the record
    /// edges when the records replayed. Class membership is rebuilt per
    /// file too: `HasMethod` always, and `HasProperty` when the records
    /// replayed, as a schema field's `HasProperty` shares the kind (module
    /// doc). `Imports` is rebuilt per file from its `File` node, so masked
    /// there. `Decorates` is rebuilt per decorated declaration,
    /// `Overrides` per dirty callable, and `Extends` / `Implements` per
    /// dirty declaration that lists a base.
    pub fn masks_base_edge(&self, base_idx: u32, rel: RelType) -> bool {
        let per_file =
            || self.dirty_base.contains(&base_idx) || self.defines_files.contains(&base_idx);
        match rel {
            RelType::Calls
            | RelType::ReadsField
            | RelType::HasMethod
            | RelType::Decorates
            | RelType::Overrides
            | RelType::Extends
            | RelType::Implements => self.dirty_base.contains(&base_idx),
            RelType::HasProperty if self.records_replayed => self.dirty_base.contains(&base_idx),
            RelType::Defines | RelType::Imports => per_file(),
            rel if self.records_replayed && replays_record_edge(rel) => per_file(),
            _ => false,
        }
    }

    /// The function meta of the virtual function, method or constructor
    /// `idx`, as the index's `ArchivedZeroCopyGraph::function_metas` holds
    /// it for a base node: `None` for a base index, for another kind, and
    /// when the parse paired no meta with the node. A dirty function's
    /// parameters reach its `Overrides` edges and its decorators its
    /// `Decorates` edges, not its meta.
    pub fn function_meta(&self, idx: u32) -> Option<&ViewFunctionMeta> {
        self.function_metas
            .binary_search_by_key(&idx, |meta| meta.node)
            .ok()
            .map(|at| &self.function_metas[at])
    }

    /// Whether the overlay rebuilds the base `References` edge with `reason`
    /// sourced at `source`: lexical closure, overload-set, enum-variant,
    /// and function-value references are rebuilt, not References generally.
    pub(crate) fn rebuilds_reference(&self, source: u32, reason: &str) -> bool {
        matches!(
            reason,
            CLOSURE_REFERENCE_REASON
                | OVERLOAD_SET_REASON
                | VARIANT_USE_REASON
                | VARIANT_PATTERN_REASON
                | FUNCTION_REFERENCE_REASON
        ) && self.dirty_base.contains(&source)
    }

    /// All overlay edges; index `i` here is overlay edge index `i`, addressed
    /// by consumers as merged edge index `graph.edges.len() + i`.
    pub fn edges(&self) -> &[ViewEdge] {
        &self.edges
    }

    /// Overlay-edge accessor by overlay edge index (NOT merged index —
    /// callers subtract `graph.edges.len()` first).
    pub fn edge(&self, overlay_edge_idx: u32) -> Option<&ViewEdge> {
        self.edges.get(overlay_edge_idx as usize)
    }

    /// Overlay-resolved outgoing edges of `idx`: virtual sources, plus the
    /// base `File` node of a re-parsed path, which sources the file's
    /// rebuilt `Defines` and `Imports`. A clean base node gains no outgoing
    /// edge. Yields `(overlay_edge_idx, edge)`.
    pub fn overlay_out(&self, idx: u32) -> impl Iterator<Item = (u32, &ViewEdge)> + '_ {
        self.out_adj
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|&ei| (ei, &self.edges[ei as usize]))
    }

    /// Overlay-resolved incoming edges of `idx` (current callers living in
    /// dirty files). For a replaced virtual node, base CSR IN-edges of its
    /// `replaced_base` are ALSO valid — consumers read both. Yields
    /// `(overlay_edge_idx, edge)`.
    pub fn overlay_in(&self, idx: u32) -> impl Iterator<Item = (u32, &ViewEdge)> + '_ {
        self.in_adj
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|&ei| (ei, &self.edges[ei as usize]))
    }

    /// Constructors of the virtual Class / Struct `idx`: the virtual
    /// Constructor nodes of its file whose [`owner_key`] is its name, in
    /// source order, overloads included. Empty for any other node.
    pub fn constructors_of(&self, idx: u32) -> &[u32] {
        self.type_ctors.get(&idx).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The lexical parent of the virtual `idx`, a virtual index of the same
    /// file: the callable that declares it in the fresh parse. `None` for an
    /// unscoped virtual node and for every base index.
    pub fn lexical_parent(&self, idx: u32) -> Option<u32> {
        self.lexical_parents.get(&idx).copied()
    }

    /// How many virtual nodes have a lexical parent: the most steps a walk
    /// up the virtual parents can take without a cycle.
    pub fn lexical_parent_count(&self) -> usize {
        self.lexical_parents.len()
    }
}

/// Layout version of [`OverlayView::to_cache_bytes`]. Bump it whenever the
/// cached layout changes shape; cache keys include it.
/// 5 (F2b): a removed file's base nodes are suppressed; a 4 view kept them.
/// 6 (R-PI-b): the view stores the base declarations it could not pair; a 5
/// view lost that caveat on a cache hit.
/// 7 (P-ext): a C# extension call adds edges and a reason; a 6 view lacks them.
/// 8 (P-ext2): the view stores its C# call gaps; a 7 view lost them.
/// 9 (P-part): a chain through a C# partial type binds; an 8 view lacks the edge.
/// 10 (P-recv): a C# value receiver of unknown type binds no ladder tier; a
/// 9 view kept those edges.
/// 11 (P-flags): a dirty C# method carries its own flags; a 10 view read
/// guessed ones and recorded a gap the 11 view does not.
/// 12 (ctor-span): a virtual node carries its columns; an 11 view has none.
/// 13 (R6 fix): the view stores each scoped virtual node's lexical parent; a
/// 12 view has none.
/// 14 (F7a-1): the view stores a new file's `File` node, the rebuilt
/// `Defines` with two reasons, the `File` nodes they mask, and each virtual
/// function's meta; a 13 view has none of them. F7a-2 extends 14 unreleased:
/// the view stores the replayed record nodes and edges, the base record
/// nodes they pair or suppress, and whether the records replayed. Fix wave
/// B extends it again: a dirty file binds no call into a removed module,
/// and a replayed literal of a nested function hashes its lexical parents.
/// F7b1 extends it again: the view stores a dirty file's class membership
/// edges with two more reasons, and masks the base ones. F7b1-2 extends it
/// again: the view stores a dirty file's `Imports` edges with three more
/// reasons, and masks the base ones. F7b1-3 extends it again: the view
/// stores a dirty file's `Decorates` edges with one more reason and the new
/// `Annotation` nodes they reach, masks the base edges, and suppresses a
/// base `Annotation` node no file references any more. F7b1-4 extends it
/// again: the view stores a dirty file's `Overrides` edges with three more
/// reasons, and masks the base ones; so does its heritage replay, for the
/// `Extends` and `Implements` edges and their two reasons. F7b2 extends it
/// again: the view stores the clean-file edge kinds its invalidation
/// closure left as indexed.
pub const VIEW_CACHE_FORMAT: u32 = 14;

/// The base edge kinds the record replay rebuilds, so masks at a re-parsed
/// file's nodes. `HasProperty` has its own arm in
/// [`OverlayView::masks_base_edge`] (module doc).
fn replays_record_edge(rel: RelType) -> bool {
    matches!(
        rel,
        RelType::UsesPathLiteral
            | RelType::QueriesTable
            | RelType::OpensTxScope
            | RelType::Publishes
            | RelType::Subscribes
    )
}

/// Every reason [`OverlayView::build`] puts on a [`ViewEdge`]. A cached view
/// stores each reason as its index here; a view with an unlisted reason is
/// never stored, so it costs a build per query as before the cache.
const VIEW_EDGE_REASONS: [&str; 43] = [
    OVERLOAD_SET_REASON,
    CLOSURE_REFERENCE_REASON,
    NEAREST_COPY_REASON,
    super::merged::OVERLAY_EDGE_REASON,
    VARIANT_USE_REASON,
    VARIANT_PATTERN_REASON,
    VALUE_READ_REASON,
    FUNCTION_REFERENCE_REASON,
    "field_read",
    csharp_extension::HEURISTIC_REASON,
    SCOPE_DEFINES_REASON,
    ENUM_VARIANT_DEFINES_REASON,
    PATH_LITERAL_SINK_REASONS[0],
    PATH_LITERAL_SINK_REASONS[1],
    PATH_LITERAL_SINK_REASONS[2],
    PATH_LITERAL_SINK_REASONS[3],
    PATH_LITERAL_SINK_REASONS[4],
    PATH_LITERAL_SINK_REASONS[5],
    PATH_LITERAL_SINK_REASONS[6],
    PATH_LITERAL_SINK_REASONS[7],
    PATH_LITERAL_SINK_REASONS[8],
    PATH_LITERAL_SINK_REASONS[9],
    PATH_LITERAL_SINK_REASONS[10],
    PATH_LITERAL_SINK_REASONS[11],
    PATH_LITERAL_SINK_REASONS[12],
    PATH_LITERAL_SINK_REASONS[13],
    "read",
    "write",
    TX_SCOPE_REASON,
    SCHEMA_FIELD_HAS_PROPERTY_REASON,
    EVENT_TOPIC_PUBLISHES_REASON,
    EVENT_TOPIC_SUBSCRIBES_REASON,
    CLASS_MEMBERSHIP_REASON,
    CLASS_MEMBERSHIP_IMPL_REASON,
    ImportReason::Named.as_str(),
    ImportReason::Module.as_str(),
    ImportReason::Guess.as_str(),
    DECORATES_REASON,
    OVERRIDES_REASONS[0],
    OVERRIDES_REASONS[1],
    OVERRIDES_REASONS[2],
    HERITAGE_REASONS[0],
    HERITAGE_REASONS[1],
];

pub(super) fn reason_index(reason: &str) -> Option<u8> {
    VIEW_EDGE_REASONS
        .iter()
        .position(|&r| r == reason)
        .map(|i| i as u8)
}

/// [`OverlayView`] as the session view cache stores it. Maps and sets are
/// sorted vectors, so the layout depends on no rkyv hasher support. The
/// adjacency maps are not stored: `build` fills them in edge order, and
/// decoding replays that order.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct CachedView {
    base_len: u32,
    paths: Vec<String>,
    nodes: Vec<CachedNode>,
    replaced: Vec<(u32, u32)>,
    suppressed: Vec<u32>,
    dirty_base: Vec<u32>,
    edges: Vec<CachedEdge>,
    type_ctors: Vec<(u32, Vec<u32>)>,
    lexical_parents: Vec<(u32, u32)>,
    hidden: Vec<HiddenDecl>,
    gaps: Vec<CallGap>,
    defines_files: Vec<u32>,
    function_metas: Vec<ViewFunctionMeta>,
    records_replayed: bool,
    stale_closure: StaleClosure,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct CachedNode {
    uid: u64,
    name: String,
    kind: NodeKind,
    owner_class: Option<String>,
    /// Index into [`CachedView::paths`].
    path: u32,
    start_line: u32,
    end_line: u32,
    start_column: u32,
    end_column: u32,
    replaced_base: Option<u32>,
    later_overload: bool,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct CachedEdge {
    source: u32,
    target: u32,
    rel_type: RelType,
    confidence: f32,
    /// Index into [`VIEW_EDGE_REASONS`].
    reason: u8,
}

fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort_unstable();
    items
}

impl CachedView {
    /// Whether every index lies where [`OverlayView::build`] puts it, for a
    /// graph of `graph_nodes` nodes. Traversals index slices and the base
    /// CSR with these values, so a decoded view that breaks one must never
    /// reach them.
    fn indices_valid(&self, graph_nodes: usize) -> bool {
        let base_len = self.base_len;
        let merged_len = u64::from(base_len) + self.nodes.len() as u64;
        let base = |idx: u32| idx < base_len;
        let virt = |idx: u32| idx >= base_len && u64::from(idx) < merged_len;
        let merged = |idx: u32| u64::from(idx) < merged_len;
        base_len as usize == graph_nodes
            && self
                .nodes
                .iter()
                .all(|n| (n.path as usize) < self.paths.len() && n.replaced_base.is_none_or(base))
            && self.replaced.iter().all(|&(b, v)| base(b) && virt(v))
            && self.suppressed.iter().copied().all(base)
            && self.dirty_base.iter().copied().all(base)
            && self.defines_files.iter().copied().all(base)
            && self.defines_files.windows(2).all(|w| w[0] < w[1])
            && self.edges.iter().all(|e| {
                // A rebuilt `Defines`, `Imports` or record edge is the one
                // edge a base node sources: the `File` node of a re-parsed
                // path.
                let base_source = matches!(e.rel_type, RelType::Defines | RelType::Imports)
                    || (self.records_replayed && replays_record_edge(e.rel_type));
                let source = virt(e.source)
                    || (base_source && self.defines_files.binary_search(&e.source).is_ok());
                source && merged(e.target)
            })
            && self
                .type_ctors
                .iter()
                .all(|(ty, ctors)| virt(*ty) && ctors.iter().copied().all(virt))
            && self
                .lexical_parents
                .iter()
                .all(|&(node, parent)| virt(node) && virt(parent))
            && self.function_metas.iter().all(|meta| virt(meta.node))
            && self
                .function_metas
                .windows(2)
                .all(|w| w[0].node < w[1].node)
    }

    fn to_bytes(&self) -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
            .map(|bytes| bytes.into_vec())
            .unwrap_or_default()
    }
}

impl OverlayView {
    /// The view as bytes for [`Self::from_cache_bytes`]. Empty when an edge
    /// reason is missing from `VIEW_EDGE_REASONS` or serialization fails;
    /// the cache then stores nothing.
    pub fn to_cache_bytes(&self) -> Vec<u8> {
        self.to_cached()
            .map(|cached| cached.to_bytes())
            .unwrap_or_default()
    }

    fn to_cached(&self) -> Option<CachedView> {
        let mut path_ids: FxHashMap<&str, u32> = FxHashMap::default();
        let mut paths: Vec<String> = Vec::new();
        let nodes = self
            .nodes
            .iter()
            .map(|n| CachedNode {
                uid: n.uid,
                name: n.name.clone(),
                kind: n.kind,
                owner_class: n.owner_class.clone(),
                path: *path_ids.entry(&*n.rel_path).or_insert_with(|| {
                    paths.push(n.rel_path.to_string());
                    paths.len() as u32 - 1
                }),
                start_line: n.start_line,
                end_line: n.end_line,
                start_column: n.start_column,
                end_column: n.end_column,
                replaced_base: n.replaced_base,
                later_overload: n.later_overload,
            })
            .collect();
        let edges = self
            .edges
            .iter()
            .map(|e| {
                Some(CachedEdge {
                    source: e.source,
                    target: e.target,
                    rel_type: e.rel_type,
                    confidence: e.confidence,
                    reason: reason_index(e.reason)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(CachedView {
            base_len: self.base_len,
            paths,
            nodes,
            replaced: sorted(self.replaced.iter().map(|(&b, &v)| (b, v)).collect()),
            suppressed: sorted(self.suppressed.iter().copied().collect()),
            dirty_base: sorted(self.dirty_base.iter().copied().collect()),
            edges,
            type_ctors: sorted(
                self.type_ctors
                    .iter()
                    .map(|(&ty, ctors)| (ty, ctors.clone()))
                    .collect(),
            ),
            lexical_parents: sorted(
                self.lexical_parents
                    .iter()
                    .map(|(&node, &parent)| (node, parent))
                    .collect(),
            ),
            hidden: self.hidden.clone(),
            gaps: self.gaps.clone(),
            defines_files: sorted(self.defines_files.iter().copied().collect()),
            function_metas: self.function_metas.clone(),
            records_replayed: self.records_replayed,
            stale_closure: self.stale_closure.clone(),
        })
    }

    /// Decode [`Self::to_cache_bytes`] output for a graph of `graph_nodes`
    /// nodes. `None` for bytes that fail rkyv validation, name a reason past
    /// `VIEW_EDGE_REASONS`, or hold an index `build` never produces: a
    /// wrong view is worse than a rebuilt one.
    pub fn from_cache_bytes(bytes: &[u8], graph_nodes: usize) -> Option<Self> {
        // Checked access refuses misaligned input, and a `fs::read` buffer
        // carries no alignment guarantee.
        let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
        aligned.extend_from_slice(bytes);
        let cached = rkyv::from_bytes::<CachedView, rkyv::rancor::Error>(&aligned).ok()?;
        Self::from_cached(cached, graph_nodes)
    }

    fn from_cached(cached: CachedView, graph_nodes: usize) -> Option<Self> {
        if !cached.indices_valid(graph_nodes) {
            return None;
        }
        let paths: Vec<Arc<str>> = cached.paths.into_iter().map(Arc::from).collect();
        let nodes = cached
            .nodes
            .into_iter()
            .map(|n| ViewNode {
                uid: n.uid,
                name: n.name,
                kind: n.kind,
                owner_class: n.owner_class,
                rel_path: paths[n.path as usize].clone(),
                start_line: n.start_line,
                end_line: n.end_line,
                start_column: n.start_column,
                end_column: n.end_column,
                replaced_base: n.replaced_base,
                later_overload: n.later_overload,
            })
            .collect();
        let edges = cached
            .edges
            .into_iter()
            .map(|e| {
                Some(ViewEdge {
                    source: e.source,
                    target: e.target,
                    rel_type: e.rel_type,
                    confidence: e.confidence,
                    reason: VIEW_EDGE_REASONS.get(usize::from(e.reason)).copied()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let mut out_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        let mut in_adj: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for (ei, edge) in edges.iter().enumerate() {
            out_adj.entry(edge.source).or_default().push(ei as u32);
            in_adj.entry(edge.target).or_default().push(ei as u32);
        }
        Some(Self {
            base_len: cached.base_len,
            nodes,
            replaced: cached.replaced.into_iter().collect(),
            suppressed: cached.suppressed.into_iter().collect(),
            dirty_base: cached.dirty_base.into_iter().collect(),
            edges,
            out_adj,
            in_adj,
            type_ctors: cached.type_ctors.into_iter().collect(),
            lexical_parents: cached.lexical_parents.into_iter().collect(),
            hidden: cached.hidden,
            gaps: cached.gaps,
            defines_files: cached.defines_files.into_iter().collect(),
            function_metas: cached.function_metas,
            records_replayed: cached.records_replayed,
            stale_closure: cached.stale_closure,
        })
    }
}

/// The base node each declaration of one dirty file replaces, in slice
/// order: [`pair_by_anchor`] between the anchors of `identities` (the
/// file's parse) and the anchors of `base` (the path's base nodes in graph
/// order, numbered by the same [`file_identities`]). A declaration pairs
/// only when its anchor occurs once on each side, so an overload set without
/// a discriminator pairs nothing, and its base members are suppressed: an
/// ordinal proves nothing about which member an edit kept.
///
/// A `Duplicate` row takes no part. The index tombstones it and keeps its
/// first as the one node of that anchor, so the first still pairs. A base
/// tombstone has an empty name, so its anchor matches no declaration.
///
/// The second value lists the base nodes left unpaired although the parse
/// still names their anchor and they carry a name: with the anchor unique on
/// both sides they would have paired, so each one sits in an ambiguous set.
/// An anchor absent from the parse was deleted by the edit and is not listed.
fn pair_with_base(
    graph: &ArchivedZeroCopyGraph,
    path: &str,
    base: &[u32],
    identities: &[DeclIdentity],
) -> (Vec<Option<u32>>, Vec<u32>) {
    let mut out = vec![None; identities.len()];
    if base.is_empty() {
        return (out, Vec::new());
    }
    let pool = &graph.string_pool;
    // The stored graph keeps no generic arity. Only the anchors are read
    // here, and the arity enters no anchor, so 0 changes none of them.
    let base_anchors: Vec<u64> = file_identities(path, base, |&i| {
        let node = &graph.nodes[i as usize];
        (
            NodeKind::from(&node.kind),
            Some(node.owner_class.resolve(pool)),
            node.name.resolve(pool),
            (
                node.span.0.to_native(),
                node.span.1.to_native(),
                node.span.2.to_native(),
                node.span.3.to_native(),
            ),
            0,
        )
    })
    .into_iter()
    .map(|id| id.anchor)
    .collect();
    let rows: Vec<usize> = (0..identities.len())
        .filter(|&j| !matches!(identities[j].slot, UidSlot::Duplicate { .. }))
        .collect();
    let dirty_anchors: Vec<u64> = rows.iter().map(|&j| identities[j].anchor).collect();
    for (&j, pair) in rows
        .iter()
        .zip(pair_by_anchor(&base_anchors, &dirty_anchors))
    {
        out[j] = pair.map(|k| base[k as usize]);
    }
    let paired: FxHashSet<u32> = out.iter().flatten().copied().collect();
    let dirty_set: FxHashSet<u64> = dirty_anchors.into_iter().collect();
    let unpaired = base
        .iter()
        .zip(&base_anchors)
        .filter(|&(&idx, anchor)| {
            !paired.contains(&idx)
                && dirty_set.contains(anchor)
                && !graph.nodes[idx as usize].name.resolve(pool).is_empty()
        })
        .map(|(&idx, _)| idx)
        .collect();
    (out, unpaired)
}

/// [`GAP_CS_EXTENSION_STALE_EDGE`] gaps: each indexed `Calls` edge from a
/// clean caller into a base C# extension method named like a method a dirty
/// C# file declares. A dirty file may add an instance member of that name,
/// or change or drop the extension, and the overlay does not re-resolve the
/// clean caller. Cost: one name-index probe per dirty C# method name, plus
/// the in-edges of the extension methods it finds. One gap per edge, with
/// no cap: `call_gaps_line` prints the total, so the list keeps every call
/// site. The caller drops duplicates ([`dedup_gaps`]).
fn stale_extension_gaps(
    graph: &ArchivedZeroCopyGraph,
    files: &[OverlayFileInput],
    dirty_base: &FxHashSet<u32>,
    suppressed: &FxHashSet<u32>,
) -> Vec<CallGap> {
    let pool = &graph.string_pool;
    let mut names: Vec<&str> = files
        .iter()
        .filter(|file| file.file_meta().language == Language::CSharp)
        .flat_map(|file| &file.symbols)
        .filter(|symbol| symbol.kind == NodeKind::Method)
        .map(|symbol| symbol.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut gaps: Vec<CallGap> = Vec::new();
    for name in names {
        for decl in graph.nodes_by_name(name) {
            let extension = graph.nodes[decl as usize].name.resolve(pool) == name
                && base_function_meta(graph, decl).is_some_and(|meta| {
                    meta.flags.to_native() & csharp_extension::EXTENSION_FLAG != 0
                });
            if !extension {
                continue;
            }
            let start = graph.in_offsets[decl as usize].to_native() as usize;
            let end = graph.in_offsets[decl as usize + 1].to_native() as usize;
            for &edge_idx in &graph.in_edge_idx[start..end] {
                let edge = &graph.edges[edge_idx.to_native() as usize];
                let source = edge.source.to_native();
                // A dirty caller is re-resolved, and a removed one is gone.
                if RelType::from(&edge.rel_type) != RelType::Calls
                    || dirty_base.contains(&source)
                    || suppressed.contains(&source)
                {
                    continue;
                }
                let caller = &graph.nodes[source as usize];
                let Some(path) = graph.files.get(caller.file_idx.to_native() as usize) else {
                    continue;
                };
                gaps.push(CallGap {
                    path: path.path.resolve(pool).to_owned(),
                    line: caller.start_line(),
                    caller: caller.name.resolve(pool).to_owned(),
                    member: name.to_owned(),
                    reason: GAP_CS_EXTENSION_STALE_EDGE.to_owned(),
                });
            }
        }
    }
    gaps
}

/// Drop every gap equal to an earlier one, keeping discovery order: one
/// hash probe per gap, where a scan of the list per gap was quadratic.
fn dedup_gaps(gaps: &mut Vec<CallGap>) {
    let keep: Vec<bool> = {
        let mut seen: FxHashSet<&CallGap> = FxHashSet::default();
        gaps.iter().map(|gap| seen.insert(gap)).collect()
    };
    let mut keep = keep.into_iter();
    gaps.retain(|_| keep.next().unwrap_or(false));
}

/// The archived `FunctionMeta` of the base node `id`.
pub(super) fn base_function_meta(
    graph: &ArchivedZeroCopyGraph,
    id: u32,
) -> Option<&crate::graph::ArchivedFunctionMeta> {
    let metas = &graph.function_metas;
    let at = metas
        .binary_search_by_key(&id, |meta| meta.node_idx.to_native())
        .ok()?;
    Some(&metas[at])
}

/// [`OverlayView::hidden_declarations`] for the base nodes `idxs`, which
/// must be in ascending order: groups come out in graph order.
fn hidden_declarations(graph: &ArchivedZeroCopyGraph, idxs: &[u32]) -> Vec<HiddenDecl> {
    let pool = &graph.string_pool;
    let mut group_of: FxHashMap<(&str, u8, &str, &str), usize> = FxHashMap::default();
    let mut groups: Vec<HiddenDecl> = Vec::new();
    for &idx in idxs {
        let node = &graph.nodes[idx as usize];
        let path = graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(pool);
        let kind = NodeKind::from(&node.kind);
        let owner = node.owner_class.resolve(pool);
        let name = node.name.resolve(pool);
        let group = *group_of
            .entry((path, kind as u8, owner, name))
            .or_insert_with(|| {
                groups.push(HiddenDecl {
                    path: path.to_string(),
                    kind,
                    owner: owner.to_string(),
                    name: name.to_string(),
                    count: 0,
                });
                groups.len() - 1
            });
        groups[group].count += 1;
    }
    groups
}

/// Whether a fresh parse (a fragment's `RawNode`s) can carry a node of
/// `kind`. The graph builder makes the rest from whole-repo passes (File,
/// routes, entry points, processes, detector and path-literal nodes), so a
/// fragment never re-emits them: absence there says nothing about deletion.
/// The per-file replay (`session::incremental`) makes the `File` node of a
/// path the base never indexed; the base `File` node of a re-parsed path
/// stays visible.
/// Exhaustive on purpose — a new kind must decide which side it is on.
fn fragment_emits(kind: NodeKind) -> bool {
    match kind {
        NodeKind::Function
        | NodeKind::Class
        | NodeKind::Method
        | NodeKind::Interface
        | NodeKind::Constructor
        | NodeKind::Property
        | NodeKind::Variable
        | NodeKind::Const
        | NodeKind::Section
        | NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Typedef
        | NodeKind::Namespace
        | NodeKind::Module
        | NodeKind::Macro
        | NodeKind::Annotation
        | NodeKind::Trait
        | NodeKind::Impl
        | NodeKind::EnumVariant => true,
        // Import and Document have no producer today; keeping a node beats
        // suppressing one nothing can re-emit.
        NodeKind::File
        | NodeKind::Import
        | NodeKind::Route
        | NodeKind::Process
        | NodeKind::Document
        | NodeKind::EntryPoint
        | NodeKind::SchemaField
        | NodeKind::EventTopic
        | NodeKind::TransactionScope
        | NodeKind::PathLiteral => false,
    }
}

/// [`OverlayView::constructors_of`] for every virtual Class / Struct.
fn virtual_constructors(
    files: &[OverlayFileInput],
    nodes: &[ViewNode],
    base_len: u32,
) -> FxHashMap<u32, Vec<u32>> {
    let mut by_owner: FxHashMap<(usize, &str), Vec<u32>> = FxHashMap::default();
    let mut types: Vec<(usize, u32)> = Vec::new();
    let mut virt = base_len;
    for (file_ord, file) in files.iter().enumerate() {
        for _ in &file.symbols {
            let node = &nodes[(virt - base_len) as usize];
            if node.kind == NodeKind::Constructor {
                if let Some(owner) = node.owner_class.as_deref().and_then(owner_key) {
                    by_owner.entry((file_ord, owner)).or_default().push(virt);
                }
            } else if node.kind.is_constructible() {
                types.push((file_ord, virt));
            }
            virt += 1;
        }
    }
    types
        .into_iter()
        .filter_map(|(file_ord, ty)| {
            let name = nodes[(ty - base_len) as usize].name.as_str();
            by_owner
                .get(&(file_ord, name))
                .map(|ctors| (ty, ctors.clone()))
        })
        .collect()
}

/// Constructors of the base Class / Struct `type_idx`: the Constructor
/// nodes in its file whose [`owner_key`] is its name. Empty for any other
/// kind. The index's `SymbolTable::build_constructor_index` applies the
/// same rule.
///
/// `HasMethod` cannot list them: `class_membership` skips a constructor
/// named like its class (Java, C#, Kotlin, C++, Dart), and with two classes
/// in one file binds each `__init__` / `constructor` to the first. So its
/// edges only supply candidate names: the type's own name plus each
/// constructor name they reach, each probed once in the name index.
pub fn base_constructors(graph: &ArchivedZeroCopyGraph, type_idx: u32) -> Vec<u32> {
    let pool = &graph.string_pool;
    let ty = &graph.nodes[type_idx as usize];
    if !NodeKind::from(&ty.kind).is_constructible() || !ty.has_owning_file() {
        return Vec::new();
    }
    let name = ty.name.resolve(pool);
    let start = graph.out_offsets[type_idx as usize].to_native() as usize;
    let end = graph.out_offsets[type_idx as usize + 1].to_native() as usize;
    let mut probes: Vec<&str> = vec![name];
    for edge in &graph.edges.as_slice()[start..end] {
        let member = &graph.nodes[edge.target.to_native() as usize];
        if RelType::from(&edge.rel_type) == RelType::HasMethod
            && NodeKind::from(&member.kind) == NodeKind::Constructor
        {
            let member_name = member.name.resolve(pool);
            if !probes.contains(&member_name) {
                probes.push(member_name);
            }
        }
    }
    probes
        .iter()
        .flat_map(|&probe| graph.nodes_by_name(probe))
        .filter(|&idx| {
            let node = &graph.nodes[idx as usize];
            NodeKind::from(&node.kind) == NodeKind::Constructor
                && node.file_idx == ty.file_idx
                && owner_key(node.owner_class.resolve(pool)) == Some(name)
        })
        .collect()
}

/// [`base_constructors`] for many types: every base Constructor grouped by
/// its file and [`owner_key`], built in one pass over the nodes. Every
/// `constructor` / `__init__` shares one name, so probing the name index
/// once per type costs types x constructors on a large repo (vscode: a
/// fuzzy `find` over 12k classes against 10.7k `constructor` nodes).
pub struct BaseConstructorIndex<'g> {
    by_owner: FxHashMap<(u32, &'g str), Vec<u32>>,
}

impl<'g> BaseConstructorIndex<'g> {
    pub fn build(graph: &'g ArchivedZeroCopyGraph) -> Self {
        let pool = &graph.string_pool;
        let mut by_owner: FxHashMap<(u32, &'g str), Vec<u32>> = FxHashMap::default();
        for (idx, node) in graph.nodes.iter().enumerate() {
            if NodeKind::from(&node.kind) != NodeKind::Constructor {
                continue;
            }
            if let Some(owner) = owner_key(node.owner_class.resolve(pool)) {
                by_owner
                    .entry((node.file_idx.to_native(), owner))
                    .or_default()
                    .push(idx as u32);
            }
        }
        Self { by_owner }
    }

    /// The same set [`base_constructors`] returns for `type_idx`.
    pub fn of(&self, graph: &'g ArchivedZeroCopyGraph, type_idx: u32) -> &[u32] {
        let ty = &graph.nodes[type_idx as usize];
        if !NodeKind::from(&ty.kind).is_constructible() || !ty.has_owning_file() {
            return &[];
        }
        let key = (ty.file_idx.to_native(), ty.name.resolve(&graph.string_pool));
        self.by_owner.get(&key).map_or(&[], Vec::as_slice)
    }
}

/// The `Calls` target of a construction of the Class / Struct `ty`: its one
/// constructor, else the type itself.
fn construction_target(
    graph: &ArchivedZeroCopyGraph,
    nodes: &[ViewNode],
    type_ctors: &FxHashMap<u32, Vec<u32>>,
    ty: u32,
) -> u32 {
    let base_len = graph.nodes.len() as u32;
    let base;
    let ctors: &[u32] = if ty >= base_len {
        type_ctors.get(&ty).map(Vec::as_slice).unwrap_or(&[])
    } else {
        base = base_constructors(graph, ty);
        &base
    };
    sole_constructor(ctors, |idx| is_later_overload(graph, nodes, idx)).unwrap_or(ty)
}

/// The one constructor among `ctors`, or `None`. Later overloads do not
/// count: the index registers only the first of them, which then stands for
/// the type's constructor.
fn sole_constructor(ctors: &[u32], is_later: impl Fn(u32) -> bool) -> Option<u32> {
    let mut firsts = ctors.iter().copied().filter(|&c| !is_later(c));
    let first = firsts.next()?;
    firsts.next().is_none().then_some(first)
}

/// [`ViewNode::later_overload`] in merged space.
fn is_later_overload(graph: &ArchivedZeroCopyGraph, nodes: &[ViewNode], idx: u32) -> bool {
    match idx.checked_sub(graph.nodes.len() as u32) {
        Some(virt_off) => nodes[virt_off as usize].later_overload,
        None => graph.is_later_overload(idx),
    }
}

/// `s`'s source range, in the coordinates of the parse's [`Span`].
fn span_of(s: &OverlaySymbol) -> Span {
    (s.start_line, s.start_column, s.end_line, s.end_column)
}

/// Mirror full indexing's lexical references and first-UID survivor rule.
fn emit_closure_references(
    file: &OverlayFileInput,
    nodes: &[ViewNode],
    start: u32,
    push: &mut impl FnMut(ViewEdge),
) {
    let anonymous =
        |s: &OverlaySymbol| s.kind == NodeKind::Function && s.name.starts_with("<anonymous:");
    if !file.symbols.iter().any(anonymous) {
        return;
    }
    let functions: Vec<_> = file
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            matches!(
                s.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            )
        })
        .collect();
    let spans: Vec<_> = functions.iter().map(|(_, s)| span_of(s)).collect();
    let parents = innermost_enclosing(&spans);
    // Overloads carry their own uids, as in full indexing. Full indexing
    // tombstones other later identical UIDs, skips closure tombstones, and
    // redirects parent tombstones.
    let mut live_by_uid = FxHashMap::default();
    for (offset, node) in nodes.iter().enumerate() {
        live_by_uid.entry(node.uid).or_insert(offset);
    }
    for (i, &(offset, symbol)) in functions.iter().enumerate() {
        if !anonymous(symbol) || live_by_uid[&nodes[offset].uid] != offset {
            continue;
        }
        let Some(parent) = parents[i] else {
            continue;
        };
        let source = live_by_uid[&nodes[functions[parent].0].uid];
        push(ViewEdge {
            source: start + source as u32,
            target: start + offset as u32,
            rel_type: RelType::References,
            confidence: 1.0,
            reason: CLOSURE_REFERENCE_REASON,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::resolution::{
        is_relative_specifier, leaf_path, path_dirs, qualifier_prefix_is_internal,
        relative_module_rank, RustModules,
    };
    use super::*;
    use crate::analyzer::ladder::split_qualifier;
    use crate::analyzer::path_aliases::PathAliases;
    use crate::analyzer::types::{CallSite, RawImport, NEAREST_COPY_CONFIDENCE};
    use crate::graph_fixture::GraphFixture;
    use rkyv::rancor::Error as RkyvError;

    /// Fixture: a base graph with a real `name_index` and uid-correct nodes.
    ///
    /// files:  0 = src/clean.rs · 1 = src/dirty.rs · 2 = src/other.rs
    /// nodes:  0 keep_fn(dirty) · 1 gone_fn(dirty) · 2 callee_unique(clean)
    ///         3 dup_fn(clean) · 4 dup_fn(other) · 5 imp_fn(other)
    ///         6 imp_fn(clean)
    fn base_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        // Pre-register in this order so `file_idx` matches the doc comment
        // above regardless of which file a node references first.
        let paths = ["src/clean.rs", "src/dirty.rs", "src/other.rs"];
        for p in paths {
            fx.file(p);
        }

        let specs: &[(&str, usize)] = &[
            ("keep_fn", 1),
            ("gone_fn", 1),
            ("callee_unique", 0),
            ("dup_fn", 0),
            ("dup_fn", 2),
            ("imp_fn", 2),
            ("imp_fn", 0),
        ];
        for (name, file_idx) in specs {
            fx.func(paths[*file_idx], name);
        }

        fx.into_bytes()
    }

    fn sym(name: &str, calls: &[&str]) -> OverlaySymbol {
        OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind: NodeKind::Function,
            owner_class: None,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 0,
            calls: calls.iter().map(|c| c.to_string()).collect(),
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
            decorators: Vec::new(),
            params: Vec::new(),
        }
    }

    /// Stands for: a closure whose uid collides links from the first live
    /// enclosing node to the first live closure. The new file's `File` node
    /// and its replayed `Defines` (F7a-1) are not what this pins.
    #[test]
    fn test_build_closure_uid_collisions_use_first_live_nodes() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut parent = sym("enclosing", &[]);
        parent.end_line = 4;
        let mut overload = parent.clone();
        overload.start_line = 5;
        overload.end_line = 9;
        let mut closure = sym("<anonymous:5:2>", &[]);
        closure.start_line = 6;
        closure.end_line = 7;
        let file = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "App.java".into(),
            symbols: vec![parent, overload, closure.clone(), closure],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let references: Vec<_> = view
            .edges()
            .iter()
            .filter(|edge| edge.rel_type == RelType::References)
            .map(|edge| (edge.source, edge.target, edge.confidence))
            .collect();
        assert_eq!(references, vec![(0, 2, 1.0)]);
    }

    #[test]
    fn test_build_overload_twins_redirect_base_to_closure_parent() {
        let mut fx = GraphFixture::new();
        let base_enclosing = fx.func("App.java", "enclosing");
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut parent = sym("enclosing", &[]);
        parent.end_line = 4;
        let mut overload = parent.clone();
        overload.start_line = 5;
        overload.end_line = 9;
        let mut closure = sym("<anonymous:6:2>", &[]);
        closure.start_line = 6;
        closure.end_line = 7;
        let file = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "App.java".into(),
            symbols: vec![parent, overload, closure],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let parent = view.redirect(base_enclosing).unwrap();
        let closure = view.base_len() + 2;
        assert!(
            view.overlay_out(parent)
                .any(|(_, e)| e.target == closure && e.rel_type == RelType::References),
            "a clean caller of the base overload must reach the closure downstream"
        );
    }

    /// `class Foo1 { foo; Read() }` on lines 3-7, then `class Foo1<T> {
    /// foo; Read2() }` on lines 8-13, in one dirty C# file; each method
    /// reads `foo` (Newtonsoft `TestObjects/Foo1.cs`).
    fn foo1_twins_input() -> OverlayFileInput {
        let member = |name: &str, kind: NodeKind, line: u32, reads: &[&str]| OverlaySymbol {
            owner_class: Some("Foo1".to_string()),
            kind,
            start_line: line,
            end_line: line,
            start_column: 8,
            end_column: 50,
            variant_refs: reads.iter().map(|r| r.to_string()).collect(),
            ..sym(name, &[])
        };
        let class = |start: u32, end: u32, arity: u32| OverlaySymbol {
            kind: NodeKind::Class,
            start_line: start,
            end_line: end,
            start_column: 4,
            end_column: 5,
            generic_arity: arity,
            flags: None,
            ..sym("Foo1", &[])
        };
        OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "N/Foo1.cs".into(),
            symbols: vec![
                class(3, 7, 0),
                member("foo", NodeKind::Property, 5, &[]),
                member("Read", NodeKind::Method, 6, &["foo"]),
                class(8, 13, 1),
                member("foo", NodeKind::Property, 10, &[]),
                member("Read2", NodeKind::Method, 11, &["foo"]),
            ],
            imports: vec![],
        }
    }

    /// Stands for: the overlay numbers a member of a C# type of another
    /// generic arity as the full build does (codex ruling P-a2):
    /// `Foo1<T>.foo` keeps its name under overload uid 1, binds by id only,
    /// and joins no overload set; a read inside each class reads that
    /// class's own `foo`. Before the fix the second `foo` was a tombstone
    /// and both reads went to `Foo1.foo`.
    #[test]
    fn test_build_csharp_member_of_arity_twin_keeps_uid_and_reads_own_twin() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[foo1_twins_input()]).unwrap();
        let path = "N/Foo1.cs";
        let class = crate::uid::compute(NodeKind::Class, path, None, "Foo1");
        let foo = crate::uid::compute(NodeKind::Property, path, Some("Foo1"), "foo");
        let nodes = view.virtual_nodes();
        assert_eq!(
            [nodes[3].uid, nodes[4].uid],
            [crate::uid::overload(class, 1), crate::uid::overload(foo, 1)]
        );
        assert_eq!(nodes[4].name, "foo", "the twin keeps its name");
        assert!(nodes[3].later_overload && nodes[4].later_overload);
        assert!(
            !view.edges().iter().any(|e| e.reason == OVERLOAD_SET_REASON),
            "{:?}",
            view.edges()
        );
        let base = view.base_len();
        let reads: Vec<(u32, u32)> = view
            .edges()
            .iter()
            .filter(|e| e.rel_type == RelType::ReadsField)
            .map(|e| (e.source - base, e.target - base))
            .collect();
        assert_eq!(reads, [(2, 1), (5, 4)], "each class reads its own foo");
    }

    fn dirty_input() -> OverlayFileInput {
        OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/dirty.rs".to_string(),
            symbols: vec![
                sym(
                    "keep_fn",
                    &["callee_unique", "dup_fn", "imp_fn", "new_helper"],
                ),
                sym("new_helper", &["keep_fn"]),
            ],
            imports: vec![RawImport {
                source: "crate::other".to_string(),
                imported_name: "imp_fn".to_string(),
                alias: None,
                binding_kind: None,
            }],
        }
    }

    fn edge_to(view: &OverlayView, source: u32, target: u32) -> Option<ViewEdge> {
        view.overlay_out(source)
            .map(|(_, e)| e)
            .find(|e| e.target == target)
            .copied()
    }

    #[test]
    fn empty_input_builds_no_view() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        assert!(OverlayView::build(graph, &[]).is_none());
    }

    #[test]
    fn test_rust_module_file_lookup_unordered_archive_finds_exact_paths() {
        let mut fixture = GraphFixture::new();
        let paths = ["src/z.rs", "src/lib.rs", "src/a/mod.rs", "src/b.rs"];
        for path in paths {
            fixture.file(path);
        }
        let bytes = fixture.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = FxHashSet::default();
        let modules = RustModules::new(graph, &[], &removed);
        for path in paths {
            assert_eq!(modules.file(std::path::Path::new(path)), Some(path));
        }
        assert_eq!(modules.file(std::path::Path::new("src/missing.rs")), None);
    }

    /// Crate `b` at `b/`, entry `b/src/lib.rs` declaring `mod util`.
    fn crate_b_graph_bytes() -> Vec<u8> {
        use crate::contracts::workspace::{
            ConfigPresence, ConfigRead, Ecosystem, Membership, NameRank, Scope, ScopeId, ScopeName,
            WorkspaceFacts,
        };
        let mut fixture = GraphFixture::new();
        fixture.file("b/src/lib.rs");
        fixture.file("b/src/util.rs");
        fixture.node(NodeKind::Module, "b/src/lib.rs", "util");
        let mut graph = fixture.build();
        let scope = Scope {
            ecosystem: Ecosystem::Cargo,
            root: "b".into(),
            config_files: vec![ConfigRead {
                path: "b/Cargo.toml".into(),
                presence: ConfigPresence::Read,
            }],
            membership: Membership::Subtree,
            entry_files: vec!["b/src/lib.rs".into()],
            aliases: Vec::new(),
            search_roots: Vec::new(),
            dependencies: Vec::new(),
        };
        let name = ScopeName {
            ecosystem: Ecosystem::Cargo,
            name: "b".into(),
            rank: NameRank::Primary,
            scope: ScopeId(0),
        };
        graph.workspace = WorkspaceFacts::new(vec![scope], vec![name]).unwrap();
        rkyv::to_bytes::<RkyvError>(&graph).unwrap().to_vec()
    }

    #[test]
    fn test_rust_modules_resolve_empty_chain_crate_head_resolves() {
        let bytes = crate_b_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = FxHashSet::default();
        let modules = RustModules::new(graph, &[], &removed);
        assert_eq!(modules.resolve(&[], "b::util"), Some("b/src/util.rs"));
        assert_eq!(modules.resolve(&[], "b"), Some("b/src/lib.rs"));
        for anchored in [
            "crate::util",
            "self::util",
            "super::util",
            "crate",
            "self",
            "super",
        ] {
            assert_eq!(modules.resolve(&[], anchored), None, "{anchored}");
        }
        assert_eq!(modules.resolve(&[], "std::fs"), None);
    }

    #[test]
    fn test_rust_modules_crate_table_loaded_on_first_crate_head() {
        let bytes = crate_b_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = FxHashSet::default();
        let modules = RustModules::new(graph, &[], &removed);
        assert!(
            modules.crates.get().is_none(),
            "building the view reads no table"
        );
        assert_eq!(modules.resolve(&[], "crate::util"), None);
        assert!(
            modules.crates.get().is_none(),
            "an anchored head reads no table"
        );
        assert_eq!(modules.crate_head("b"), Some("b/src/lib.rs"));
        assert_eq!(
            modules.crates.get().map(|facts| facts.scopes.len()),
            Some(1),
            "the first crate head loads the stored table"
        );
    }

    /// Module paths built with `PathBuf::join` carry `\\` on Windows; the
    /// lookup still finds the archived `/` path.
    #[test]
    fn test_rust_module_file_lookup_backslash_path_finds_archived_path() {
        let mut fixture = GraphFixture::new();
        fixture.file("src/bin/util.rs");
        let bytes = fixture.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = FxHashSet::default();
        let modules = RustModules::new(graph, &[], &removed);
        assert_eq!(
            modules.file(std::path::Path::new("src\\bin\\util.rs")),
            Some("src/bin/util.rs")
        );
    }

    #[test]
    fn replaced_suppressed_and_masking() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let base_len = view.base_len();

        // keep_fn survives the edit: base 0 redirects to its virtual twin.
        assert_eq!(view.redirect(0), Some(base_len));
        assert_eq!(view.node(base_len).unwrap().replaced_base, Some(0));
        // gone_fn was deleted: traversal must drop it entirely.
        assert_eq!(view.redirect(1), None);
        // Both dirty-file base nodes mask the stale rels the overlay rebuilds.
        assert!(view.masks_base_edge(0, RelType::Calls));
        assert!(view.masks_base_edge(1, RelType::Calls));
        assert!(view.masks_base_edge(0, RelType::ReadsField));
        // clean nodes keep their base edges.
        assert!(!view.masks_base_edge(2, RelType::Calls));
        assert_eq!(view.redirect(2), Some(2));
    }

    /// Stands for: a removed file leaves nothing of itself in the view, the
    /// nodes the graph builder made included, and no base edge into or out
    /// of it survives the merge. A clean file keeps its own nodes. The view
    /// also survives the cache round trip, so a cached view hides them too.
    #[test]
    fn test_build_with_removed_deleted_file_suppresses_every_base_node_and_edge() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/gone.rs", "gone.rs");
        let route = fx.node(NodeKind::Route, "src/gone.rs", "GET /gone");
        let literal = fx.node(NodeKind::PathLiteral, "src/gone.rs", "./kept");
        let gone = fx.func("src/gone.rs", "gone_fn");
        let kept = fx.func("src/kept.rs", "kept_fn");
        let kept_file = fx.node(NodeKind::File, "src/kept.rs", "kept.rs");
        fx.edge(kept, gone, RelType::Calls);
        fx.edge(gone, kept, RelType::Calls);
        fx.edge(file, gone, RelType::Defines);
        fx.edge(kept_file, kept, RelType::Defines);
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();

        let view = OverlayView::build_with_removed(graph, &[], &["src/gone.rs".to_string()])
            .expect("a removed path alone builds a view");

        for (what, idx) in [
            ("File", file),
            ("Route", route),
            ("PathLiteral", literal),
            ("Function", gone),
        ] {
            assert_eq!(view.redirect(idx), None, "{what} of the removed file");
        }
        assert_eq!(view.redirect(kept), Some(kept));
        assert_eq!(view.redirect(kept_file), Some(kept_file));
        assert!(view.virtual_nodes().is_empty());
        let merged = crate::session::merged::MergedGraph::new(graph, Some(&view));
        let edges: Vec<(u32, u32)> = merged.all_edges().map(|e| (e.source, e.target)).collect();
        assert_eq!(edges, vec![(kept_file, kept)]);
        assert_eq!(
            OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len()).as_ref(),
            Some(&view)
        );
    }

    /// Stands for: a removed path that also has a fresh parse exists on
    /// disk, so its parse decides: a surviving symbol redirects to its
    /// virtual twin instead of vanishing.
    #[test]
    fn test_build_with_removed_path_with_parse_keeps_parse() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();

        let view =
            OverlayView::build_with_removed(graph, &[dirty_input()], &["src/dirty.rs".to_string()])
                .unwrap();

        assert_eq!(view.redirect(0), Some(view.base_len()), "keep_fn survives");
        assert_eq!(view.redirect(1), None, "gone_fn is not re-emitted");
        assert_eq!(view, OverlayView::build(graph, &[dirty_input()]).unwrap());
    }

    /// Stands for: a removed path the graph never indexed (an untracked file
    /// added and deleted again), an empty path and a whitespace-only one
    /// hide no base node; no input at all builds no view.
    #[test]
    fn test_build_with_removed_unknown_paths_hide_nothing() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let removed = ["src/never.rs".to_string(), String::new(), " ".to_string()];

        let view = OverlayView::build_with_removed(graph, &[], &removed).unwrap();

        assert!((0..view.base_len()).all(|idx| view.redirect(idx) == Some(idx)));
        assert!(OverlayView::build_with_removed(graph, &[], &[]).is_none());
    }

    /// Base `Method`s of owner `K`, one per name, 10 lines apart so no span
    /// nests in another.
    fn method_base(path: &str, names: &[&str]) -> Vec<u8> {
        let mut fx = GraphFixture::new();
        for (i, name) in names.iter().enumerate() {
            let node = fx.method(path, "K", name);
            let line = i as u32 * 10;
            fx.span(node, (line, 0, line + 4, 0));
        }
        fx.into_bytes()
    }

    /// The parse of [`method_base`]'s shape: the same spans, one per name.
    fn method_parse(path: &str, names: &[&str]) -> OverlayFileInput {
        let symbols = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut s = sym(name, &[]);
                s.kind = NodeKind::Method;
                s.owner_class = Some("K".to_string());
                s.start_line = i as u32 * 10 + 1;
                s.end_line = i as u32 * 10 + 5;
                s
            })
            .collect();
        OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: path.to_string(),
            symbols,
            imports: vec![],
        }
    }

    fn hidden(path: &str, name: &str, count: u32) -> HiddenDecl {
        HiddenDecl {
            path: path.to_string(),
            kind: NodeKind::Method,
            owner: "K".to_string(),
            name: name.to_string(),
            count,
        }
    }

    /// Stands for: a comment-only edit of a file with an overload set hides
    /// the set's base members, and the view names them so a caveat can.
    #[test]
    fn test_build_with_removed_shared_overloads_comment_edit_records_ambiguous() {
        let bytes = method_base("K.java", &["save", "save"]);
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();

        let view = OverlayView::build(graph, &[method_parse("K.java", &["save", "save"])]).unwrap();

        assert_eq!(view.hidden_declarations(), [hidden("K.java", "save", 2)]);
        assert_eq!(
            (0..2).map(|i| view.redirect(i)).collect::<Vec<_>>(),
            [None, None]
        );
    }

    /// Stands for: the base side alone makes a set ambiguous, so deleting one
    /// of two overloads still hides both base members.
    #[test]
    fn test_build_with_removed_base_two_dirty_one_records_ambiguous() {
        let bytes = method_base("K.java", &["save", "save", "load"]);
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();

        let view = OverlayView::build(graph, &[method_parse("K.java", &["save", "load"])]).unwrap();

        assert_eq!(view.hidden_declarations(), [hidden("K.java", "save", 2)]);
        assert_eq!(view.redirect(2), Some(view.base_len() + 1), "load pairs");
    }

    /// Stands for: a deleted member is not a pairing gap. Its anchor is gone
    /// from the parse, so no caveat names it.
    #[test]
    fn test_build_with_removed_unique_method_deleted_records_nothing() {
        let bytes = method_base("K.java", &["save", "load"]);
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();

        let view = OverlayView::build(graph, &[method_parse("K.java", &["load"])]).unwrap();

        assert_eq!(view.redirect(0), None, "save is suppressed");
        assert!(view.hidden_declarations().is_empty());
    }

    /// Stands for: a removed path has no parse to pair with, and a base
    /// tombstone (empty name) matches no declaration; neither is a gap.
    #[test]
    fn test_build_with_removed_removed_path_and_tombstone_record_nothing() {
        let mut fx = GraphFixture::new();
        for line in [0, 10] {
            let node = fx.method("Gone.java", "K", "save");
            fx.span(node, (line, 0, line + 4, 0));
            let tombstone = fx.method("K.java", "K", "");
            fx.span(tombstone, (line + 20, 0, line + 24, 0));
        }
        let kept = fx.method("K.java", "K", "load");
        fx.span(kept, (0, 0, 4, 0));
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();

        // The parse names the tombstone's empty name, so only the name
        // check keeps its anchor out of the list.
        let parse = method_parse("K.java", &["load", ""]);

        let view =
            OverlayView::build_with_removed(graph, &[parse], &["Gone.java".to_string()]).unwrap();

        assert_eq!(view.redirect(0), None, "Gone.java save is suppressed");
        assert!(view.hidden_declarations().is_empty());
    }

    /// Stands for: a cache hit carries the same caveat data as a fresh build.
    #[test]
    fn test_overlay_view_cache_round_trip_keeps_ambiguous() {
        let bytes = method_base("K.java", &["save", "save"]);
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[method_parse("K.java", &["save", "save"])]).unwrap();
        assert!(!view.hidden_declarations().is_empty(), "fixture hides");

        let decoded =
            OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len()).unwrap();

        assert_eq!(decoded.hidden_declarations(), view.hidden_declarations());
        assert_eq!(decoded, view);
    }

    #[test]
    fn tier1_same_file_edge() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let keep_virt = view.base_len();
        let helper_virt = view.base_len() + 1;

        // keep_fn → new_helper and new_helper → keep_fn, both same-file.
        let e1 = edge_to(&view, keep_virt, helper_virt).expect("keep_fn → new_helper");
        assert_eq!(e1.confidence, 1.0);
        let e2 = edge_to(&view, helper_virt, keep_virt).expect("new_helper → keep_fn");
        assert_eq!(e2.confidence, 1.0);
        assert_eq!(e2.rel_type, RelType::Calls);
    }

    #[test]
    fn tier3_unique_global_resolves_ambiguous_suppresses() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let keep_virt = view.base_len();

        // callee_unique: one global candidate → edge with Tier-3 confidence.
        let e = edge_to(&view, keep_virt, 2).expect("keep_fn → callee_unique");
        assert_eq!(e.confidence, 0.7);
        // dup_fn: two clean-base candidates → AmbiguousGlobal-style suppression.
        assert!(edge_to(&view, keep_virt, 3).is_none());
        assert!(edge_to(&view, keep_virt, 4).is_none());
        // the reverse index sees the resolved caller.
        assert!(view.overlay_in(2).any(|(_, e)| e.source == keep_virt));
    }

    #[test]
    fn tier2_import_scoped_beats_global_ambiguity() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        let keep_virt = view.base_len();

        // imp_fn exists in other.rs AND clean.rs (globally ambiguous), but
        // the import source `crate::other` scopes it to other.rs (node 5).
        let e = edge_to(&view, keep_virt, 5).expect("keep_fn → imp_fn via import");
        assert_eq!(e.confidence, 0.95);
        assert!(edge_to(&view, keep_virt, 6).is_none());
    }

    #[test]
    fn edge_into_replaced_base_redirects_to_virtual() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        // Second dirty file calls keep_fn — globally unique among callables
        // once dirty-base copies are excluded; target must be keep_fn's
        // VIRTUAL index, never the stale base node.
        let other_dirty = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/clean.rs".to_string(),
            symbols: vec![sym("caller_two", &["keep_fn"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[dirty_input(), other_dirty]).unwrap();
        let keep_virt = view.base_len();
        let caller_two_virt = view.base_len() + 2;

        let e = edge_to(&view, caller_two_virt, keep_virt)
            .expect("caller_two → keep_fn must redirect to the virtual node");
        assert_eq!(e.target, keep_virt);
        assert!(view
            .overlay_in(keep_virt)
            .any(|(_, e)| e.source == caller_two_virt));
    }

    /// files: 0 = src/service.py · 1 = tests/fakes.py (Test) · 2 = src/helper.go
    /// nodes: 0 scan_range(service) · 1 scan_range(fakes) · 2 go_only(helper)
    fn barrier_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.file("src/service.py");
        fx.file_as("tests/fakes.py", crate::graph::FileCategory::Test);
        fx.file("src/helper.go");
        fx.func("src/service.py", "scan_range");
        fx.func("tests/fakes.py", "scan_range");
        fx.func("src/helper.go", "go_only");
        fx.into_bytes()
    }

    fn search_input(rel_path: &str) -> OverlayFileInput {
        OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: rel_path.to_string(),
            symbols: vec![sym("search", &["scan_range", "go_only"])],
            imports: vec![],
        }
    }

    #[test]
    fn test_tier3_overlay_test_double_keeps_name_ambiguous_like_index() {
        let bytes = barrier_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        for caller in ["src/search.py", "tests/test_search.py"] {
            let view = OverlayView::build(graph, &[search_input(caller)]).unwrap();
            let search = view.base_len();
            assert!(edge_to(&view, search, 0).is_none(), "{caller}");
            assert!(edge_to(&view, search, 1).is_none(), "{caller}");
        }
    }

    #[test]
    fn test_tier3_overlay_language_barrier_matches_index() {
        // The index never resolves a Python call to a Go function; the
        // overlay used to, because its Tier 3 checked only the node kind.
        let bytes = barrier_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[search_input("src/search.py")]).unwrap();
        assert!(edge_to(&view, view.base_len(), 2).is_none());
    }

    /// files: 0 = src/widget.py · 1 = src/gadget.py
    /// nodes: 0 Widget(class) · 1 __init__(ctor of Widget) · 2 Gadget(class)
    fn construction_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.file("src/widget.py");
        fx.file("src/gadget.py");
        let widget = fx.node(NodeKind::Class, "src/widget.py", "Widget");
        let init = fx.node_owned(NodeKind::Constructor, "src/widget.py", "Widget", "__init__");
        fx.node(NodeKind::Class, "src/gadget.py", "Gadget");
        fx.edge(widget, init, RelType::HasMethod);
        fx.into_bytes()
    }

    #[test]
    fn test_build_unresolved_construction_calls_constructor_like_index() {
        let bytes = construction_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let untyped_member = CallSite::untyped_member("Widget");
        let app = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/app.py".to_string(),
            symbols: vec![
                sym("make", &["Widget", "Gadget"]),
                sym("use_factory", &[untyped_member.as_str()]),
            ],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[app]).unwrap();
        let make = view.base_len();
        let use_factory = view.base_len() + 1;

        // `Widget()` lands on its one constructor, `Gadget()` on the class
        // that declares none: the edges the index builds for the same file.
        let e = edge_to(&view, make, 1).expect("make → Widget.__init__");
        assert_eq!(e.confidence, 0.7);
        assert!(edge_to(&view, make, 0).is_none(), "one edge per call site");
        assert!(edge_to(&view, make, 2).is_some(), "make → Gadget");
        // `obj.Widget()` on an untyped receiver is a method call.
        assert_eq!(view.overlay_out(use_factory).count(), 0);
    }

    /// The index binds a construction to the first constructor overload; a
    /// later one is no second constructor, so the overlay must not count it.
    #[test]
    fn test_build_construction_of_overloaded_dirty_type_calls_first_constructor_like_index() {
        let bytes = construction_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let ctor = |line: u32| OverlaySymbol {
            is_exported: false,
            name: "Multi".to_string(),
            kind: NodeKind::Constructor,
            owner_class: Some("Multi".to_string()),
            start_line: line,
            end_line: line + 1,
            start_column: 0,
            end_column: 0,
            calls: vec![],
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
            decorators: Vec::new(),
            params: Vec::new(),
        };
        let multi = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/Multi.java".to_string(),
            symbols: vec![
                OverlaySymbol {
                    is_exported: false,
                    kind: NodeKind::Class,
                    owner_class: None,
                    end_line: 9,
                    start_column: 0,
                    end_column: 0,
                    ..ctor(1)
                },
                ctor(2),
                ctor(5),
            ],
            imports: vec![],
        };
        // The Java extractor encodes `new Multi(1)` as a construction.
        let new_multi = CallSite::construct("Multi");
        let app = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/App.java".to_string(),
            symbols: vec![sym("makeMulti", &[new_multi.as_str()])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[multi, app]).unwrap();
        let first_ctor = view.base_len() + 1;
        let make = view.base_len() + 3;

        assert!(
            edge_to(&view, make, first_ctor).is_some(),
            "makeMulti → the first Multi overload"
        );
        assert_eq!(view.overlay_out(make).count(), 1, "one edge per call site");
    }

    /// files: 0 = src/models.py
    /// nodes: 0 Alpha · 1 Alpha.__init__ · 2 Beta · 3 Beta.__init__
    ///
    /// `class_membership` binds both classes' `HasMethod` to the file's
    /// first `__init__`; the fixture keeps that defect.
    fn two_class_graph_bytes() -> Vec<u8> {
        let mut fx = GraphFixture::new();
        fx.file("src/models.py");
        let alpha = fx.node(NodeKind::Class, "src/models.py", "Alpha");
        let alpha_init = fx.node_owned(NodeKind::Constructor, "src/models.py", "Alpha", "__init__");
        let beta = fx.node(NodeKind::Class, "src/models.py", "Beta");
        fx.node_owned(NodeKind::Constructor, "src/models.py", "Beta", "__init__");
        fx.edge(alpha, alpha_init, RelType::HasMethod);
        fx.edge(beta, alpha_init, RelType::HasMethod);
        fx.into_bytes()
    }

    /// The index picks the constructor its owner names, never the one a
    /// `HasMethod` edge points at.
    #[test]
    fn test_build_construction_of_second_class_in_clean_file_calls_its_own_constructor() {
        let bytes = two_class_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let app = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/app.py".to_string(),
            symbols: vec![sym("make", &["Beta"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[app]).unwrap();
        let make = view.base_len();

        assert!(edge_to(&view, make, 3).is_some(), "make → Beta.__init__");
        assert_eq!(view.overlay_out(make).count(), 1, "one edge per call site");
        let merged = crate::session::MergedGraph::new(graph, Some(&view));
        assert_eq!(merged.constructors_of(2), [3], "Beta's constructors");
        assert_eq!(merged.constructors_of(0), [1], "Alpha's constructors");
    }

    /// The overlay compares owners through the index's `owner_key`: a
    /// generic path (`Outer<T>.Inner`) names its last segment, `Inner`.
    #[test]
    fn test_build_construction_owner_with_generic_path_calls_constructor_like_index() {
        let bytes = construction_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let symbol = |name: &str, kind, owner: &str, line: u32| OverlaySymbol {
            is_exported: false,
            name: name.to_string(),
            kind,
            owner_class: Some(owner.to_string()),
            start_line: line,
            end_line: line + 1,
            start_column: 0,
            end_column: 0,
            calls: vec![],
            heritage: Vec::new(),
            variant_refs: Vec::new(),
            return_type: None,
            generic_arity: 0,
            flags: None,
            decorators: Vec::new(),
            params: Vec::new(),
        };
        let outer = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/outer.ts".to_string(),
            symbols: vec![
                symbol("Inner", NodeKind::Class, "Outer<T>", 2),
                symbol("constructor", NodeKind::Constructor, "Outer<T>.Inner", 3),
            ],
            imports: vec![],
        };
        let new_inner = CallSite::construct("Inner");
        let app = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/app.ts".to_string(),
            symbols: vec![sym("makeInner", &[new_inner.as_str()])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[outer, app]).unwrap();
        let ctor = view.base_len() + 1;
        let make = view.base_len() + 2;

        assert!(
            edge_to(&view, make, ctor).is_some(),
            "makeInner → Inner's constructor"
        );
        assert_eq!(view.overlay_out(make).count(), 1, "one edge per call site");
        assert_eq!(view.constructors_of(view.base_len()), [ctor]);
    }

    /// Contract: later overloads count as their first node in any order;
    /// two distinct constructors are several.
    #[test]
    fn test_sole_constructor_overloads_and_distinct_returns_first_or_none() {
        let later = |idx: u32| idx == 5;
        assert_eq!(sole_constructor(&[3, 5], later), Some(3));
        assert_eq!(sole_constructor(&[5, 3], later), Some(3));
        assert_eq!(sole_constructor(&[3, 7, 5], later), None);
        assert_eq!(sole_constructor(&[7], later), Some(7));
        assert_eq!(sole_constructor(&[], later), None);
    }

    /// Contract: builder-made kinds are never in a fragment, so a dirty file
    /// keeps them as indexed; every other kind is the parser's to re-emit.
    #[test]
    fn test_fragment_emits_builder_made_kinds_false_parser_kinds_true() {
        const BUILDER_MADE: [NodeKind; 10] = [
            NodeKind::File,
            NodeKind::Route,
            NodeKind::Process,
            NodeKind::EntryPoint,
            NodeKind::SchemaField,
            NodeKind::EventTopic,
            NodeKind::TransactionScope,
            NodeKind::PathLiteral,
            NodeKind::Import,
            NodeKind::Document,
        ];
        for kind in NodeKind::ALL {
            assert_eq!(
                fragment_emits(kind),
                !BUILDER_MADE.contains(&kind),
                "{kind:?}"
            );
        }
    }

    /// The index's relative candidates: `./` and `../` from the importer's
    /// directory, nothing for any other specifier.
    #[test]
    fn test_relative_module_rank_relative_specifiers_join_importer_dir() {
        let cases = [
            ("src/b.ts", "./a", "src/a.ts"),
            ("src/b.ts", "./a.ts", "src/a.ts"),
            ("src/b.ts", "./lib/", "src/lib/index.ts"),
            ("src/x/b.ts", "../a", "src/a.ts"),
            ("src/x/b.ts", "../../a", "a.ts"),
            ("b.ts", "../a", "a.ts"),
            ("b.ts", "./a", "a.ts"),
        ];
        for (importer, specifier, path) in cases {
            assert!(is_relative_specifier(specifier), "{specifier}");
            assert!(
                relative_module_rank(importer, specifier, path, &PathAliases::new()).is_some(),
                "{importer} {specifier} {path}"
            );
        }
        for specifier in ["a", "@/a", "crate::a", ".a"] {
            assert!(!is_relative_specifier(specifier), "{specifier}");
        }
    }

    /// Probe order: the bare path, the extensions (`.ts` before `.js`), then
    /// the index suffixes; anything else is never probed.
    #[test]
    fn test_relative_module_rank_follows_resolver_probe_order() {
        let rank = |path: &str| relative_module_rank("src/b.ts", "./a", path, &PathAliases::new());
        assert!(rank("src/a").is_some());
        assert!(rank("src/a") < rank("src/a.ts"));
        assert!(rank("src/a.ts") < rank("src/a.js"));
        assert!(rank("src/a.js") < rank("src/a/index.ts"));
        assert!(rank("src/a/index.ts") < rank("src/a/index.js"));
        assert_eq!(rank("src/ab.ts"), None);
        assert_eq!(rank("src/a/other.ts"), None);
        assert_eq!(rank("tests/a.ts"), None);
        assert_eq!(rank("src/__mocks__/a.ts"), None);
        assert!(relative_module_rank("b.ts", "./", "index.ts", &PathAliases::new()).is_some());
    }

    #[test]
    fn test_split_qualifier_qualified_callee_takes_segment_before_member() {
        assert_eq!(split_qualifier("K.save"), Some(("K", "save")));
        assert_eq!(split_qualifier("K::st"), Some(("K", "st")));
        assert_eq!(split_qualifier("std::vec::Vec::new"), Some(("Vec", "new")));
        assert_eq!(split_qualifier("\\App\\helper"), Some(("App", "helper")));
        assert_eq!(split_qualifier("super().m"), Some(("super()", "m")));
        for bare in ["save", ".save", "K.", "::st"] {
            assert_eq!(split_qualifier(bare), None, "{bare}");
        }
    }

    #[test]
    fn test_qualifier_prefix_is_internal_external_path_is_rejected() {
        assert!(qualifier_prefix_is_internal("K.save", "K"));
        assert!(qualifier_prefix_is_internal("util::foo", "util"));
        assert!(qualifier_prefix_is_internal("crate::util::foo", "util"));
        assert!(qualifier_prefix_is_internal("super::super::x::foo", "x"));
        assert!(!qualifier_prefix_is_internal("std::fs::read", "fs"));
        assert!(!qualifier_prefix_is_internal("a::b::foo", "b"));
        assert!(!qualifier_prefix_is_internal("save", "save"));
    }

    /// A dirty file's `k.save()` (recorded `K.save`) binds `K`'s member in
    /// the same file, and `K.st()` its static one; an unknown qualifier binds
    /// nothing, though `save` is the only callable of that name.
    #[test]
    fn test_build_qualified_callee_binds_member_in_qualifier_file() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let member = |name: &str, owner: &str| OverlaySymbol {
            kind: NodeKind::Method,
            owner_class: Some(owner.to_string()),
            ..sym(name, &[])
        };
        let class = |name: &str| OverlaySymbol {
            kind: NodeKind::Class,
            ..sym(name, &[])
        };
        let run = OverlaySymbol {
            kind: NodeKind::Method,
            owner_class: Some("App".to_string()),
            ..sym("run", &["K.save", "K.st", "Missing.save"])
        };
        let file = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "K.java".into(),
            symbols: vec![
                class("K"),
                member("save", "K"),
                member("st", "K"),
                class("App"),
                run,
            ],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let base = view.base_len();
        let calls: Vec<_> = view
            .overlay_out(base + 4)
            .filter(|(_, e)| e.rel_type == RelType::Calls)
            .map(|(_, e)| (e.target - base, e.confidence))
            .collect();
        assert_eq!(
            calls,
            vec![
                (1, Tier::QualifierScoped.confidence()),
                (2, Tier::QualifierScoped.confidence())
            ]
        );
    }

    /// A bare `save()` in `K.run` binds `K.save`, not the interface's
    /// `A.save` that precedes it in the file (`SymbolTable::prefer_owned`).
    #[test]
    fn test_build_same_file_call_from_owner_binds_own_member() {
        let bytes = GraphFixture::new().into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let owned = |kind, name: &str, owner: Option<&str>, calls: &[&str]| OverlaySymbol {
            kind,
            owner_class: owner.map(str::to_string),
            ..sym(name, calls)
        };
        let file = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "K.java".into(),
            symbols: vec![
                owned(NodeKind::Interface, "A", None, &[]),
                owned(NodeKind::Method, "save", Some("A"), &[]),
                owned(NodeKind::Class, "K", None, &[]),
                owned(NodeKind::Method, "save", Some("K"), &[]),
                owned(NodeKind::Method, "run", Some("K"), &["save"]),
            ],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let base = view.base_len();
        let calls: Vec<_> = view
            .overlay_out(base + 4)
            .filter(|(_, e)| e.rel_type == RelType::Calls)
            .map(|(_, e)| (e.target - base, e.confidence, e.reason))
            .collect();
        assert_eq!(
            calls,
            vec![(
                3,
                Tier::SameFile.confidence(),
                crate::session::merged::OVERLAY_EDGE_REASON
            )]
        );
    }

    /// Base copies of one C file under `core/` and `android/core/`, and a
    /// dirty caller of `c_make` at `caller`: the `Calls` edges of its `t`.
    fn nearest_copy_calls(copies: [&str; 2], caller: &str) -> (Vec<u32>, Vec<ViewEdge>) {
        let mut fx = GraphFixture::new();
        let ids = copies.map(|path| fx.func(path, "c_make")).to_vec();
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let file = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: caller.into(),
            symbols: vec![sym("t", &["c_make"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        let edges = view
            .overlay_out(view.base_len())
            .map(|(_, e)| *e)
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        (ids, edges)
    }

    #[test]
    fn test_build_ambiguous_copies_bind_copy_in_callers_tree() {
        let copies = ["core/c/src/cm.c", "android/core/c/src/cm.c"];
        for (caller, nearest) in [("core/c/test/t.c", 0), ("android/core/c/test/t.c", 1)] {
            let (ids, edges) = nearest_copy_calls(copies, caller);
            let calls: Vec<_> = edges
                .iter()
                .map(|e| (e.target, e.confidence, e.reason))
                .collect();
            assert_eq!(
                calls,
                vec![(ids[nearest], NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON)],
                "{caller}"
            );
        }
    }

    #[test]
    fn test_build_equidistant_copies_bind_nothing() {
        let (_, edges) =
            nearest_copy_calls(["left/c/src/cm.c", "right/c/src/cm.c"], "mid/c/test/t.c");
        assert!(edges.is_empty(), "{edges:?}");
    }

    #[test]
    fn test_build_same_name_in_differently_named_files_binds_nothing() {
        let (_, edges) = nearest_copy_calls(
            ["core/c/src/a.c", "android/core/c/src/b.c"],
            "core/c/test/t.c",
        );
        assert!(edges.is_empty(), "{edges:?}");
    }

    #[test]
    fn test_path_dirs_and_leaf_path_match_index_helpers() {
        assert_eq!(path_dirs("a/b/c.rs").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(path_dirs("c.rs").count(), 0);
        assert_eq!(leaf_path("a/b/c.rs"), "b/c.rs");
        assert_eq!(leaf_path("b/c.rs"), "b/c.rs");
        assert_eq!(leaf_path("c.rs"), "c.rs");
    }

    /// A view holding every part the cache stores: replaced and suppressed
    /// base nodes, `Calls` edges, closure and overload-set references, and a
    /// type's constructors.
    fn composite_view(graph: &ArchivedZeroCopyGraph) -> OverlayView {
        let mut parent = sym("enclosing", &[]);
        parent.end_line = 4;
        let mut closure = sym("<anonymous:2:2>", &[]);
        closure.start_line = 2;
        closure.end_line = 3;
        let closures = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/Closures.java".into(),
            symbols: vec![parent, closure],
            imports: vec![],
        };
        let ctor = |line: u32| OverlaySymbol {
            kind: NodeKind::Constructor,
            owner_class: Some("Multi".to_string()),
            start_line: line,
            end_line: line + 1,
            ..sym("Multi", &[])
        };
        let multi = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/Multi.java".to_string(),
            symbols: vec![
                OverlaySymbol {
                    kind: NodeKind::Class,
                    end_line: 9,
                    ..sym("Multi", &[])
                },
                ctor(2),
                ctor(5),
            ],
            imports: vec![],
        };
        let new_multi = CallSite::construct("Multi");
        let app = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/App.java".to_string(),
            symbols: vec![sym("makeMulti", &[new_multi.as_str()])],
            imports: vec![],
        };
        OverlayView::build(graph, &[dirty_input(), closures, multi, app]).unwrap()
    }

    #[test]
    fn test_from_cache_bytes_built_view_round_trip_returns_equal_view() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = composite_view(graph);
        // The fixture must reach every part, or equality proves less.
        let reasons: FxHashSet<&str> = view.edges().iter().map(|e| e.reason).collect();
        for reason in [
            CLOSURE_REFERENCE_REASON,
            OVERLOAD_SET_REASON,
            crate::session::merged::OVERLAY_EDGE_REASON,
        ] {
            assert!(
                reasons.contains(reason),
                "fixture lacks {reason}: {reasons:?}"
            );
        }
        assert_eq!(view.redirect(0), Some(view.base_len()), "replaced");
        assert_eq!(view.redirect(1), None, "suppressed");
        // dirty_input's two symbols and the closure file's two precede it.
        let multi = view.base_len() + 4;
        assert_eq!(view.constructors_of(multi).len(), 2, "type_ctors");

        let decoded =
            OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len()).unwrap();

        assert_eq!(decoded, view);
        let nodes = decoded.virtual_nodes();
        assert!(
            Arc::ptr_eq(&nodes[0].rel_path, &nodes[1].rel_path),
            "one Arc per distinct path"
        );
        for idx in 0..view.base_len() + nodes.len() as u32 {
            assert_eq!(decoded.redirect(idx), view.redirect(idx), "redirect({idx})");
            assert_eq!(
                decoded.overlay_out(idx).collect::<Vec<_>>(),
                view.overlay_out(idx).collect::<Vec<_>>(),
                "overlay_out({idx})"
            );
            assert_eq!(
                decoded.overlay_in(idx).collect::<Vec<_>>(),
                view.overlay_in(idx).collect::<Vec<_>>(),
                "overlay_in({idx})"
            );
            assert_eq!(
                decoded.masks_base_edge(idx, RelType::Calls),
                view.masks_base_edge(idx, RelType::Calls)
            );
        }
    }

    #[test]
    fn test_from_cache_bytes_nearest_copy_edge_returns_same_reason_constant() {
        let mut fx = GraphFixture::new();
        fx.func("core/c/src/cm.c", "c_make");
        fx.func("android/core/c/src/cm.c", "c_make");
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let file = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "core/c/test/t.c".into(),
            symbols: vec![sym("t", &["c_make"])],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[file]).unwrap();
        assert_eq!(view.edges()[0].reason, NEAREST_COPY_REASON);

        let decoded =
            OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len()).unwrap();

        assert_eq!(decoded, view);
    }

    #[test]
    fn test_reason_index_listed_and_unknown_reasons_map_to_index_or_none() {
        for (i, reason) in VIEW_EDGE_REASONS.into_iter().enumerate() {
            // An edge's reason may be a different allocation of the same
            // text: the lookup must match by content, not by pointer.
            let owned = String::from(reason);
            assert_eq!(reason_index(&owned), Some(i as u8));
        }
        for unknown in ["", " ", "call", "l1-overlay "] {
            assert_eq!(reason_index(unknown), None, "{unknown:?}");
        }
    }

    /// Every reason the shared `Overrides` and heritage derivations emit is
    /// cacheable: an unlisted one keeps every view with a dirty override or
    /// a dirty base out of the cache.
    #[test]
    fn test_reason_index_overrides_and_heritage_reasons_are_listed() {
        for reason in OVERRIDES_REASONS.into_iter().chain(HERITAGE_REASONS) {
            assert!(reason_index(reason).is_some(), "{reason}");
        }
    }

    /// An unlisted reason must leave nothing to store, or every query would
    /// write a view that no decode accepts.
    #[test]
    fn test_to_cache_bytes_unknown_reason_returns_empty() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        view.edges[0].reason = "made-up-reason";
        assert!(view.to_cache_bytes().is_empty());
    }

    #[test]
    fn test_from_cache_bytes_edge_past_node_space_returns_none() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut view = OverlayView::build(graph, &[dirty_input()]).unwrap();
        view.edges[0].target = view.base_len() + view.virtual_nodes().len() as u32;
        assert!(OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len()).is_none());
    }

    /// A base node sources an overlay edge only as a listed `File` node of a
    /// rebuilt `Defines`; any other base source is a view `build` never
    /// makes, and so is a function meta outside the virtual space.
    #[test]
    fn test_from_cache_bytes_base_sourced_edge_or_meta_outside_virtual_space_returns_none() {
        let mut fx = GraphFixture::new();
        let file = fx.node(NodeKind::File, "src/dirty.rs", "dirty.rs");
        fx.func("src/dirty.rs", "keep_fn");
        let bytes = fx.into_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut keep = sym("keep_fn", &[]);
        keep.flags = Some(0);
        let input = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/dirty.rs".to_string(),
            symbols: vec![keep],
            imports: vec![],
        };
        let view = OverlayView::build(graph, &[input]).unwrap();
        assert_eq!(view.defines_files, FxHashSet::from_iter([file]));
        assert!(view
            .edges
            .iter()
            .any(|e| e.source == file && e.rel_type == RelType::Defines));
        let decode = |view: &OverlayView| {
            OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len())
        };
        assert_eq!(decode(&view).as_ref(), Some(&view));

        let mut unlisted = OverlayView::build(graph, &[dirty_input_at("src/dirty.rs")]).unwrap();
        unlisted.defines_files.clear();
        assert!(decode(&unlisted).is_none(), "a base source not listed");

        let mut calls_from_file =
            OverlayView::build(graph, &[dirty_input_at("src/dirty.rs")]).unwrap();
        for edge in &mut calls_from_file.edges {
            edge.rel_type = RelType::Calls;
        }
        assert!(decode(&calls_from_file).is_none(), "a base-sourced Calls");

        let mut meta = view;
        meta.function_metas[0].node = 0;
        assert!(decode(&meta).is_none(), "a meta on a base node");
    }

    fn dirty_input_at(path: &str) -> OverlayFileInput {
        OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: path.to_string(),
            symbols: vec![sym("keep_fn", &[])],
            imports: vec![],
        }
    }

    #[test]
    fn test_from_cache_bytes_graph_node_count_differs_returns_none() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let cached = composite_view(graph).to_cache_bytes();
        let nodes = graph.nodes.len();
        assert!(OverlayView::from_cache_bytes(&cached, nodes).is_some());
        for other in [nodes - 1, nodes + 1, 0] {
            assert!(
                OverlayView::from_cache_bytes(&cached, other).is_none(),
                "graph of {other} nodes"
            );
        }
    }

    /// Each index `build` keeps in a fixed range, moved one step out of it.
    /// The composite view holds every part, so each mutation has a target.
    #[test]
    fn test_from_cache_bytes_index_out_of_build_range_returns_none() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = composite_view(graph);
        let merged = |c: &CachedView| c.base_len + c.nodes.len() as u32;
        type Mutation = (&'static str, fn(&mut CachedView));
        let cases: [Mutation; 14] = [
            ("node path past paths", |c| {
                c.nodes[0].path = c.paths.len() as u32
            }),
            ("replaced_base in virtual space", |c| {
                c.nodes[0].replaced_base = Some(c.base_len)
            }),
            ("replaced key in virtual space", |c| {
                c.replaced[0].0 = c.base_len
            }),
            ("replaced value in base space", |c| {
                c.replaced[0].1 = c.base_len - 1
            }),
            ("replaced value past merged space", |c| {
                c.replaced[0].1 = c.base_len + c.nodes.len() as u32
            }),
            ("suppressed in virtual space", |c| {
                c.suppressed.push(c.base_len)
            }),
            ("dirty_base in virtual space", |c| {
                c.dirty_base.push(c.base_len)
            }),
            ("edge source in base space", |c| {
                c.edges[0].source = c.base_len - 1
            }),
            ("edge source past merged space", |c| {
                c.edges[0].source = c.base_len + c.nodes.len() as u32
            }),
            ("edge target past merged space", |c| {
                c.edges[0].target = c.base_len + c.nodes.len() as u32
            }),
            ("edge reason past the table", |c| {
                c.edges[0].reason = VIEW_EDGE_REASONS.len() as u8
            }),
            ("type_ctors key in base space", |c| {
                c.type_ctors[0].0 = c.base_len - 1
            }),
            ("type_ctors value in base space", |c| {
                c.type_ctors[0].1[0] = c.base_len - 1
            }),
            ("type_ctors value past merged space", |c| {
                c.type_ctors[0].1[0] = c.base_len + c.nodes.len() as u32
            }),
        ];
        let intact = view.to_cached().unwrap();
        assert!(intact.base_len > 0 && merged(&intact) > intact.base_len);
        assert!(OverlayView::from_cache_bytes(&intact.to_bytes(), graph.nodes.len()).is_some());
        for (what, mutate) in cases {
            let mut cached = view.to_cached().unwrap();
            mutate(&mut cached);
            assert!(
                OverlayView::from_cache_bytes(&cached.to_bytes(), graph.nodes.len()).is_none(),
                "{what}"
            );
        }
    }

    /// A base-space edge target is legitimate (a call into a clean file);
    /// the bounds check must keep it.
    #[test]
    fn test_from_cache_bytes_edge_into_base_space_returns_view() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = composite_view(graph);
        assert!(view.edges().iter().any(|e| e.target < view.base_len()));

        let decoded = OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len());

        assert_eq!(decoded.as_ref(), Some(&view));
    }

    #[test]
    fn test_from_cache_bytes_truncated_garbage_or_empty_returns_none() {
        let bytes = base_graph_bytes();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let cached = composite_view(graph).to_cache_bytes();
        let nodes = graph.nodes.len();
        assert!(!cached.is_empty());
        for len in [cached.len() - 1, 4, 0] {
            assert!(
                OverlayView::from_cache_bytes(&cached[..len], nodes).is_none(),
                "truncated to {len}"
            );
        }
        assert!(OverlayView::from_cache_bytes(&[0xAB; 512], nodes).is_none());
    }

    /// Stands for: the payload lists each call site once, in discovery
    /// order, and its count (`call_gaps_line`) counts distinct calls.
    #[test]
    fn test_dedup_gaps_repeated_gaps_keep_first_of_each_in_order() {
        let gap = |caller: &str, member: &str, reason: &str| CallGap {
            path: "Lib/A.cs".to_owned(),
            line: 3,
            caller: caller.to_owned(),
            member: member.to_owned(),
            reason: reason.to_owned(),
        };
        let mut gaps = vec![
            gap("Run", "Spin", GAP_CS_EXTENSION_DIRTY_DECLARATION),
            gap("Run", "Spin", GAP_CS_ACCESS_DIRTY_TARGET),
            gap("Run", "Spin", GAP_CS_EXTENSION_DIRTY_DECLARATION),
            gap("Use", "Spin", GAP_CS_EXTENSION_STALE_EDGE),
            gap("Run", "Spin", GAP_CS_ACCESS_DIRTY_TARGET),
        ];
        dedup_gaps(&mut gaps);
        assert_eq!(
            gaps,
            [
                gap("Run", "Spin", GAP_CS_EXTENSION_DIRTY_DECLARATION),
                gap("Run", "Spin", GAP_CS_ACCESS_DIRTY_TARGET),
                gap("Use", "Spin", GAP_CS_EXTENSION_STALE_EDGE),
            ]
        );
        let mut empty: Vec<CallGap> = Vec::new();
        dedup_gaps(&mut empty);
        assert!(empty.is_empty());
    }

    /// A dirty Rust file: `outer` on lines 1-5 nests `helper` on line 2. The
    /// base holds the same two, nested by span as the index parses them.
    fn nested_rust_view_parts() -> (Vec<u8>, OverlayFileInput) {
        let mut fx = GraphFixture::new();
        let outer = fx.func("src/a.rs", "outer");
        let helper = fx.func("src/a.rs", "helper");
        fx.span(outer, (0, 0, 4, 0));
        fx.span(helper, (1, 0, 1, 0));
        let at = |name: &str, start_line: u32, end_line: u32| OverlaySymbol {
            start_line,
            end_line,
            ..sym(name, &[])
        };
        let file = OverlayFileInput {
            parsed_language: None,
            export_aliases: Vec::new(),
            rel_path: "src/a.rs".to_string(),
            symbols: vec![at("outer", 1, 5), at("helper", 2, 2)],
            imports: vec![],
        };
        (fx.into_bytes(), file)
    }

    /// Stands for: the view keeps each scoped virtual node's lexical parent
    /// from the fresh parse, and the session view cache stores it, so a
    /// cache hit answers the display owner a fresh build answers.
    #[test]
    fn test_from_cache_bytes_rust_nested_fn_round_trip_keeps_lexical_parent() {
        let (bytes, file) = nested_rust_view_parts();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[file]).unwrap();
        let (outer, helper) = (view.base_len(), view.base_len() + 1);
        assert_eq!(view.lexical_parent(helper), Some(outer));
        assert_eq!(view.lexical_parent(outer), None);
        assert_eq!(view.lexical_parent(0), None, "a base index has no view row");
        assert_eq!(view.lexical_parent_count(), 1);

        let decoded =
            OverlayView::from_cache_bytes(&view.to_cache_bytes(), graph.nodes.len()).unwrap();

        assert_eq!(decoded, view);
        assert_eq!(decoded.lexical_parent(helper), Some(outer));
    }

    /// Stands for: a cached row whose node or parent lies outside virtual
    /// space is refused, as every other cached index is.
    #[test]
    fn test_from_cache_bytes_lexical_parent_outside_virtual_space_returns_none() {
        let (bytes, file) = nested_rust_view_parts();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let view = OverlayView::build(graph, &[file]).unwrap();
        type Mutation = (&'static str, fn(&mut CachedView));
        let cases: [Mutation; 3] = [
            ("node in base space", |c| {
                c.lexical_parents[0].0 = c.base_len - 1
            }),
            ("parent in base space", |c| {
                c.lexical_parents[0].1 = c.base_len - 1
            }),
            ("parent past merged space", |c| {
                c.lexical_parents[0].1 = c.base_len + c.nodes.len() as u32
            }),
        ];
        assert_eq!(view.to_cached().unwrap().lexical_parents.len(), 1);
        for (what, mutate) in cases {
            let mut cached = view.to_cached().unwrap();
            mutate(&mut cached);
            assert!(
                OverlayView::from_cache_bytes(&cached.to_bytes(), graph.nodes.len()).is_none(),
                "{what}"
            );
        }
    }

    /// Stands for: virtual rows that form a cycle (only a corrupt cache
    /// could hold them) end the display-owner walk with no owner instead of
    /// a loop.
    #[test]
    fn test_display_owner_cyclic_virtual_rows_returns_none() {
        let (bytes, file) = nested_rust_view_parts();
        let graph = rkyv::access::<ArchivedZeroCopyGraph, RkyvError>(&bytes).unwrap();
        let mut cached = OverlayView::build(graph, &[file])
            .unwrap()
            .to_cached()
            .unwrap();
        let (outer, helper) = (cached.base_len, cached.base_len + 1);
        cached.lexical_parents = vec![(outer, helper), (helper, outer)];
        let view = OverlayView::from_cached(cached, graph.nodes.len()).unwrap();
        let merged = crate::session::merged::MergedGraph::new(graph, Some(&view));

        assert_eq!(merged.display_owner(helper), None);
        assert_eq!(merged.display_owner(outer), None);
    }
}
