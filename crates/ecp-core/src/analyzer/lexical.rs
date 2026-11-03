//! Lexical call scoping: a bare call binds the innermost named function in
//! scope before any name tier. Every language that opts in through
//! [`lexical_scope_kinds`] admits its scoped functions; JS / TS admit every
//! file-level function too, Rust and Python only the scoped ones
//! ([`LexicalAdmission`]). Full indexing (Pass 2) and the session overlay
//! build this same index over one file's declarations.
//!
//! The same lexical facts feed declaration identity: [`file_identities`]
//! numbers one file's declarations for every path that stores a uid.

use crate::analyzer::outcome::{Resolution, Tier};
use crate::analyzer::types::{
    innermost_enclosing, span_contains, CallSite, LocalGraph, NameRef, NameRefKind, RawNode, Span,
};
use crate::contracts::identity::{
    assign_file, container_parents_in, lexical_parents, lexical_parents_in, DeclIdentity, FileDecl,
    UidSlot,
};
use crate::file_category::Language;
use crate::graph::NodeKind;
use crate::registry::os_path_to_slash;
use crate::uid;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cmp::Ordering;
use std::ops::Range;
use std::path::Path;

/// The kinds `language` scopes under their enclosing callable (design A
/// §4.2): such a node's uid hashes its parent's anchor instead of an owner.
/// Rust opts in its nested `fn` and closure nodes (slice C); Python, JS and
/// TS their nested functions, lambda and callback nodes and, in JS / TS,
/// an arrow function a local `const` holds (slice D). Slice E adds the
/// other languages.
pub fn lexical_scope_kinds(language: Language) -> &'static [NodeKind] {
    match language {
        Language::Rust | Language::Python | Language::JavaScript | Language::TypeScript => {
            &[NodeKind::Function]
        }
        _ => &[],
    }
}

/// The lexical tier of a `language` file: JS / TS admit their file-level
/// functions too, any other language that scopes a kind
/// ([`lexical_scope_kinds`]) its scoped nodes only; `None` for a language
/// with no lexical tier.
fn lexical_admission(language: Language) -> Option<LexicalAdmission> {
    match language {
        Language::JavaScript | Language::TypeScript => Some(LexicalAdmission::ScopedAndFileLevel),
        _ if !lexical_scope_kinds(language).is_empty() => Some(LexicalAdmission::ScopedOnly),
        _ => None,
    }
}

/// Clears the owner of every node of a `language` file that
/// [`lexical_parents`] scopes (design A §3.2, invariant I1): a scoped node
/// has a lexical parent and no owner. A parser that opts in runs this once,
/// last, so its own owner rules and `stamp_owner_class_by_span` stay as they
/// are. A language that scopes no kind keeps every owner.
pub fn clear_scoped_owners(language: Language, nodes: &mut [RawNode]) {
    let scoped = lexical_scope_kinds(language);
    if scoped.is_empty() {
        return;
    }
    let decls: Vec<(NodeKind, Span)> = nodes.iter().map(|n| (n.kind, n.span)).collect();
    for (node, parent) in nodes.iter_mut().zip(lexical_parents(&decls, scoped)) {
        if parent.is_some() {
            node.owner_class = None;
        }
    }
}

/// Whether each declaration of one Rust file is body-local: a callable's
/// span strictly holds it (a type, an `impl` block and its members, a nested
/// `fn`). A body-local declaration is no item of its module: no path or
/// `use` names it from outside its function, and it shadows no glob
/// `pub use`. Uid identity stays with [`lexical_scope_kinds`]. Spans from
/// one syntax tree nest, so [`inside_callable`] gives the same answer for one
/// declaration. `enclosing` is [`decls_enclosing`] of `decls`.
pub fn body_locals(decls: &[(NodeKind, Span)], enclosing: &[Option<usize>]) -> Vec<bool> {
    (0..decls.len())
        .map(|i| {
            let mut up = enclosing[i];
            while let Some(j) = up {
                if decls[j].0.is_callable() {
                    return true;
                }
                up = enclosing[j];
            }
            false
        })
        .collect()
}

/// The per-uid AND over one file's [`body_locals`]: Pass 1 merges each later
/// twin of a uid of a merge kind ([`crate::uid::merges_repeats`]) into its
/// first, and the merged node is body-local only when every declaration
/// merged into it is. Any other repeat keeps a node of its own, so it is no
/// twin here. `twins` holds (twin, its first) as
/// indices into `local`. The index's builder and the overlay's dirty side
/// both fold here.
pub fn fold_twins(local: &mut [bool], twins: impl IntoIterator<Item = (usize, usize)>) {
    for (twin, first) in twins {
        if !local[twin] {
            local[first] = false;
        }
    }
}

/// The [`innermost_enclosing`] of `decls`' spans: what [`body_locals`] and
/// [`lexical_parents_in`](crate::contracts::identity::lexical_parents_in)
/// read, built once per file.
pub fn decls_enclosing(decls: &[(NodeKind, Span)]) -> Vec<Option<usize>> {
    let spans: Vec<Span> = decls.iter().map(|&(_, span)| span).collect();
    innermost_enclosing(&spans)
}

/// [`body_locals`] for the one declaration at `span`, from the spans of its
/// file's callables: the form a reader of stored nodes, which keeps no
/// per-file list, can afford.
pub fn inside_callable(callables: &[Span], span: Span) -> bool {
    callables
        .iter()
        .any(|&outer| outer != span && span_contains(outer, span))
}

/// One declaration as [`file_identities`] reads it: kind, owner, name,
/// 0-based span and declared generic arity ([`RawNode::generic_arity`]).
/// The arity counts only where [`splits_by_arity`] holds.
pub type DeclFields<'a> = (NodeKind, Option<&'a str>, &'a str, Span, u32);

/// The [`DeclFields`] of a parsed node.
pub fn raw_decl(node: &RawNode) -> DeclFields<'_> {
    (
        node.kind,
        node.owner_class.as_deref(),
        node.name.as_str(),
        node.span,
        match node.kind.declares_type() || node.kind == NodeKind::Function {
            true => node.generic_arity(),
            false => 0,
        },
    )
}

/// Whether a `kind` declaration of a `language` file declares a generic
/// arity that keeps it apart from a same-named one of another arity: a C#
/// type (`Conv` and `Conv<T>`) or a C# delegate (`Func<T, R>` beside
/// `Func<T1, T2, R>`). The C# parser emits a delegate as a `Function` and
/// writes the arity marker on no other `Function`, so every other C#
/// function compares at arity 0 and keeps the repeat rule: a repeat is
/// `Later`, unless [`uid::merges_repeats`] merges it into a tombstone.
/// Restricted to C# by codex rulings P-a and P-a2; every other language
/// keeps the repeat rule.
pub fn splits_by_arity(language: Language, kind: NodeKind) -> bool {
    language == Language::CSharp && (kind.declares_type() || kind == NodeKind::Function)
}

/// For each declaration of one file, the index of the innermost type
/// declaration ([`NodeKind::declares_type`]) whose span contains it, by
/// [`innermost_enclosing`]. A type's own entry is the type that encloses it.
pub fn enclosing_types(rows: &[(NodeKind, Span)]) -> Vec<Option<usize>> {
    enclosing_types_in(rows, &decls_enclosing(rows))
}

/// [`enclosing_types`] over `enclosing`, the [`decls_enclosing`] of `rows`,
/// for a caller that reads it for another rule too.
fn enclosing_types_in(
    rows: &[(NodeKind, Span)],
    enclosing: &[Option<usize>],
) -> Vec<Option<usize>> {
    (0..rows.len())
        .map(|i| {
            let mut up = enclosing[i];
            while let Some(j) = up {
                if rows[j].0.declares_type() {
                    break;
                }
                up = enclosing[j];
            }
            up
        })
        .collect()
}

/// Among `candidates`, a declaration and its arity twins
/// (`LaterReason::Arity`) each with the span of the type that declares
/// it, the one whose declaring type most closely encloses `reader`: a read
/// of `foo` inside `Foo1<T>` reads `Foo1<T>.foo`, not `Foo1.foo`. `None`
/// when no declaring type encloses `reader`. Spans of one file nest or are
/// disjoint, so the innermost one starts last.
pub fn twin_enclosing(
    candidates: impl IntoIterator<Item = (u32, Span)>,
    reader: Span,
) -> Option<u32> {
    candidates
        .into_iter()
        .filter(|&(_, scope)| span_contains(scope, reader))
        .max_by_key(|&(_, scope)| ((scope.0, scope.1), std::cmp::Reverse((scope.2, scope.3))))
        .map(|(id, _)| id)
}

/// The identity of each declaration of the file at `path`, in slice order:
/// [`assign_file`] over the language's lexical parents, every declaration's
/// identity container ([`container_parents_in`]: a member hashes its
/// container's anchor, so a Rust impl member hashes its `Impl` block and
/// `O1.H.run` is no repeat of `O2.H.run`), and, in a C# file, each
/// declaration's generic arity ([`splits_by_arity`]; 0 for every other
/// kind) and the type that encloses it ([`enclosing_types`]), so a member of
/// `Foo1<T>` is no duplicate of the same member of `Foo1`. A top-level
/// declaration has no container and keeps `uid::compute`. The inputs are
/// kind, owner, name and span only, which the base graph stores, so the
/// overlay re-derives every anchor from stored nodes. The full build,
/// incremental mode and the overlay view (its dirty parse and its base
/// nodes) all number a file through this one function, so a later overload
/// or a same-uid redefinition gets the same uid, and a declaration the same
/// anchor, on every path. The one-input case of [`path_identities`].
pub fn file_identities<'a, T>(
    path: &str,
    decls: &'a [T],
    fields: impl Fn(&'a T) -> DeclFields<'a>,
) -> Vec<DeclIdentity> {
    identities_of(path, &[decls], fields)
}

/// [`file_identities`] of several inputs that share the path `path` (two
/// `LocalGraph`s of one path), numbered as one file: one [`assign_file`]
/// over the inputs in order, so a declaration of a later input repeats the
/// earlier inputs' ones. Spans relate only rows of one input, so each
/// input's lexical parents, containers and enclosing types come from that
/// input alone. One identity list per input, in input order.
pub fn path_identities<'a, T>(
    path: &str,
    inputs: &[&'a [T]],
    fields: impl Fn(&'a T) -> DeclFields<'a>,
) -> Vec<Vec<DeclIdentity>> {
    let mut all = identities_of(path, inputs, fields).into_iter();
    inputs
        .iter()
        .map(|input| all.by_ref().take(input.len()).collect())
        .collect()
}

/// The order `GraphBuilder::build` sorts its inputs in, which
/// [`SharedPaths::of`] groups: the `Path` order of the file path, ties
/// broken by the stored spelling ([`os_path_to_slash`]).
/// `ZeroCopyGraph::file_idx_of` binary-searches the `Path` order of the
/// stored paths. The walked and the stored spelling have one `Path` order:
/// the stored one changes only a `\` on Windows, where both `\` and `/` are
/// separators (`std::path::is_separator`) and `Ord` compares components.
/// Two paths that `Path` order counts equal but that are spelled apart
/// (`a//b`, `a/b`) sort by spelling, so the inputs of one stored path are
/// adjacent.
pub fn stored_path_order(a: &LocalGraph, b: &LocalGraph) -> Ordering {
    a.file_path
        .cmp(&b.file_path)
        .then_with(|| os_path_to_slash(&a.file_path).cmp(&os_path_to_slash(&b.file_path)))
}

/// The paths that 2+ `LocalGraph`s share. `GraphBuilder::build` sorts its
/// inputs by [`stored_path_order`], stably, so the inputs of one path are
/// adjacent and in input order: each shared path is one run of file
/// indices. Pass 1 numbers a run as one file; the T7-6 skip gate skips
/// nothing of it.
#[derive(Debug)]
pub struct SharedPaths {
    /// Runs of 2+ file indices, ascending and disjoint.
    runs: Vec<Range<u32>>,
}

impl SharedPaths {
    /// The runs of adjacent inputs of one stored path ([`os_path_to_slash`])
    /// of `local_graphs`, sorted by [`stored_path_order`]. Empty for a build
    /// whose paths are unique; allocates only for a shared path.
    pub fn of(local_graphs: &[LocalGraph]) -> Self {
        debug_assert!(
            local_graphs.is_sorted_by(|a, b| stored_path_order(a, b).is_le()),
            "the inputs are sorted by `stored_path_order`"
        );
        let same = |a: &LocalGraph, b: &LocalGraph| {
            os_path_to_slash(&a.file_path) == os_path_to_slash(&b.file_path)
        };
        let mut start = 0;
        let runs = local_graphs
            .chunk_by(same)
            .filter_map(|run| {
                let inputs = start..start + run.len() as u32;
                start = inputs.end;
                (run.len() >= 2).then_some(inputs)
            })
            .collect();
        debug_assert!(
            {
                let mut paths: Vec<_> = local_graphs
                    .chunk_by(same)
                    .map(|run| os_path_to_slash(&run[0].file_path))
                    .collect();
                let runs = paths.len();
                paths.sort_unstable();
                paths.dedup();
                paths.len() == runs
            },
            "no stored path appears in two runs"
        );
        Self { runs }
    }

    /// The run of file `file`, `None` when no other input shares its path.
    pub fn run_of(&self, file: u32) -> Option<Range<u32>> {
        let at = self.runs.partition_point(|run| run.end <= file);
        self.runs.get(at).filter(|run| run.start <= file).cloned()
    }
}

/// T7-6: per raw node of `local_graph`, the input at `file`, whether its
/// unchanged body skips resolver work. `None` when the skip data has no
/// entry for the file: no diff data, or the file needs a full reanalyze. An
/// empty inner set means every symbol changed. The uids are Pass 1's,
/// overloads numbered. The one check of whether a file's resolver work
/// skips, for Pass 2 and class membership.
///
/// `None` too for an input of a path of `shared`: Pass 1 numbers its inputs
/// as one file, the skip data each input alone, so a changed body of a later
/// input could carry the uid of an unchanged one.
pub fn skipped_nodes(
    local_graph: &LocalGraph,
    file: u32,
    symbol_skip: Option<&FxHashMap<String, FxHashSet<u64>>>,
    shared: &SharedPaths,
) -> Option<Vec<bool>> {
    let symbol_skip = symbol_skip?;
    if shared.run_of(file).is_some() {
        return None;
    }
    let path = os_path_to_slash(&local_graph.file_path);
    let skip_uids = symbol_skip.get(path.as_ref())?;
    Some(
        file_uids(&path, &local_graph.nodes, raw_decl)
            .iter()
            .map(|uid| skip_uids.contains(uid))
            .collect(),
    )
}

fn identities_of<'a, T>(
    path: &str,
    inputs: &[&'a [T]],
    fields: impl Fn(&'a T) -> DeclFields<'a>,
) -> Vec<DeclIdentity> {
    let path = os_path_to_slash(path);
    let language = Language::from_path(&path);
    let mut rows: Vec<DeclFields<'a>> = Vec::with_capacity(inputs.iter().map(|i| i.len()).sum());
    for &input in inputs {
        rows.extend(input.iter().map(&fields));
    }
    let scoped = lexical_scope_kinds(language);
    // Two members of one uid share an owner name, so their C# arity paths
    // can differ only inside two same-named types. A path without such a
    // pair needs no arity scope: every scope is `None`, every member path
    // equal, as the sweep would find.
    let twin_types = language == Language::CSharp && {
        let mut type_names = FxHashSet::default();
        rows.iter()
            .filter(|r| r.0.declares_type())
            .any(|r| !type_names.insert(r.2))
    };
    // One span sweep per input feeds the three span rules. Spans relate
    // only rows of one input, so each input's indices move by its start.
    let n = rows.len();
    let (mut parents, mut containers, mut arity_scopes) =
        (vec![None; n], vec![None; n], vec![None; n]);
    let mut start = 0;
    for input in inputs {
        let end = start + input.len();
        let decls: Vec<(NodeKind, Span)> = rows[start..end].iter().map(|r| (r.0, r.3)).collect();
        let enclosing = decls_enclosing(&decls);
        let shift = |i: Option<u32>| i.map(|i| i + start as u32);
        if !scoped.is_empty() {
            for (out, p) in parents[start..end]
                .iter_mut()
                .zip(lexical_parents_in(&decls, &enclosing, scoped))
            {
                *out = shift(p);
            }
        }
        for (out, c) in containers[start..end]
            .iter_mut()
            .zip(container_parents_in(&decls, &enclosing))
        {
            *out = shift(c);
        }
        if twin_types {
            for (out, ty) in arity_scopes[start..end]
                .iter_mut()
                .zip(enclosing_types_in(&decls, &enclosing))
            {
                *out = shift(ty.map(|ty| ty as u32));
            }
        }
        start = end;
    }
    let decls: Vec<FileDecl<'a>> = rows
        .iter()
        .enumerate()
        .map(|(i, &(kind, owner, name, _, arity))| FileDecl {
            kind,
            owner,
            name,
            parent: parents[i],
            container: containers[i],
            discriminator: None,
            arity: (language == Language::CSharp).then_some(if splits_by_arity(language, kind) {
                arity
            } else {
                0
            }),
            arity_scope: arity_scopes[i],
        })
        .collect();
    // `os_path_to_slash` removes every `\` `check_path` rejects;
    // `lexical_parents_in` returns in-range strict containers, scopes no
    // kind that keeps overloads, and the parser clears each scoped node's
    // owner ([`clear_scoped_owners`]); `container_parents_in` and
    // `enclosing_types_in` walk the tree of one input's spans, so they
    // return in-range indices and no cycle: no `IdentityError` can arise.
    assign_file(&path, &decls).expect("a `/`-separated path and well-formed declarations")
}

/// The uid a node stores for `slot`: a [`UidSlot::Duplicate`] keeps its
/// position as a tombstone ([`uid::tombstone`]), any other slot its own uid.
pub fn stored_uid(slot: UidSlot) -> u64 {
    match slot {
        UidSlot::Duplicate { uid, .. } => uid::tombstone(uid),
        slot => slot.uid(),
    }
}

/// The node a walk up a lexical-parent table ends at: from `start`, follow
/// `parent` until it returns `None`. Each row of a table is one step, so a
/// walk of more than `rows` steps is a cycle, which only a corrupt table
/// holds: the walk stops and returns `None`. Every reader of a parent chain
/// walks it here, so all of them stop at the same step and read a cycle
/// the same way.
pub fn lexical_chain_end<T: Copy>(
    start: T,
    rows: usize,
    mut parent: impl FnMut(T) -> Option<T>,
) -> Option<T> {
    let mut current = start;
    for _ in 0..=rows {
        match parent(current) {
            Some(next) => current = next,
            None => return Some(current),
        }
    }
    None
}

/// The parts of `start`'s display owner (design A §4.5): the first stored
/// owner up its lexical-parent chain, and the names of the parents the walk
/// passes on the way, innermost first. `node(id)` gives the stored owner
/// (`None` for no owner) and the name of `id`; `parent(id)` its lexical
/// parent; `rows` the size of the parent table ([`lexical_chain_end`]).
///
/// `None` for a cycle and when `node` knows no `start` or no parent the walk
/// reaches: a display reader then prints no owner rather than a cut path.
pub fn display_chain<'s, T: Copy>(
    start: T,
    rows: usize,
    node: impl Fn(T) -> Option<(Option<&'s str>, &'s str)>,
    parent: impl Fn(T) -> Option<T>,
) -> Option<(Option<&'s str>, Vec<&'s str>)> {
    let mut parents: Vec<&'s str> = Vec::new();
    let mut head = None;
    let mut known = true;
    let mut at_start = true;
    lexical_chain_end(start, rows, |current| {
        let Some((owner, name)) = node(current) else {
            known = false;
            return None;
        };
        if !std::mem::take(&mut at_start) {
            parents.push(name);
        }
        if let Some(owner) = owner {
            head = Some(owner);
            return None;
        }
        parent(current)
    })?;
    known.then_some((head, parents))
}

/// The display owner text of a [`display_chain`]: the stored owner, then
/// each parent's name outermost first, joined by `::`. A nested `helper` in
/// `impl A { fn new }` gives `A::new`. `None` when the chain holds neither.
pub fn display_owner_text<'s>(
    (head, mut segments): (Option<&'s str>, Vec<&'s str>),
) -> Option<std::borrow::Cow<'s, str>> {
    segments.extend(head);
    segments.reverse();
    match segments.as_slice() {
        [] => None,
        [only] => Some(std::borrow::Cow::Borrowed(*only)),
        _ => Some(std::borrow::Cow::Owned(segments.join("::"))),
    }
}

/// [`stored_uid`] of each declaration of the file at `path`, in slice order.
pub fn file_uids<'a, T>(
    path: &str,
    decls: &'a [T],
    fields: impl Fn(&'a T) -> DeclFields<'a>,
) -> Vec<u64> {
    file_identities(path, decls, fields)
        .into_iter()
        .map(|id| stored_uid(id.slot))
        .collect()
}

/// Which functions the lexical tier of one file admits (design A §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexicalAdmission {
    /// JS / TS: every node [`lexical_parents`] scopes, plus every file-level
    /// function without an owner, whose scope is the whole file.
    ScopedAndFileLevel,
    /// Rust, Python: only the nodes [`lexical_parents`] scopes. An unscoped
    /// function stays with the name tiers, so a module member (`mod tests {
    /// fn helper() }`) or a Python module-level `helper` is picked there, as
    /// before.
    ScopedOnly,
}

/// One file's lexical candidates. Parent discovery runs once, not once per
/// call.
#[derive(Default)]
pub struct LexicalFunctions {
    by_name: FxHashMap<String, Vec<(Option<Span>, u32)>>,
    /// The file's `Module` declarations. A module inside a callable's body
    /// does not see that callable's own nested items.
    modules: Vec<Span>,
    /// `None` for a file no lexical tier covers.
    admission: Option<LexicalAdmission>,
}

impl LexicalFunctions {
    /// The index over `symbols`, one file's declarations in source order as
    /// `(kind, name, has_owner, span)`. The i-th declaration's id is
    /// `start + i`. Each node [`lexical_parents`] scopes is a candidate with
    /// its parent's span as its scope, so the index and the identity rule
    /// ([`file_identities`]) read one parent derivation. Under
    /// [`LexicalAdmission::ScopedAndFileLevel`] (JS / TS) a file-level
    /// function without an owner joins with the whole file as its scope.
    /// Under [`LexicalAdmission::ScopedOnly`] a name with no scoped node gets
    /// no entry, so [`Self::targets`] leaves it to the name tiers. A file
    /// whose language has no lexical tier gets an empty index.
    pub fn new<'a>(
        path: &Path,
        symbols: impl Iterator<Item = (NodeKind, &'a str, bool, Span)>,
        start: u32,
    ) -> Self {
        let language = Language::from_path(&path.to_string_lossy());
        let Some(admission) = lexical_admission(language) else {
            return Self::default();
        };
        let symbols: Vec<(NodeKind, &str, bool, Span)> = symbols.collect();
        let decls: Vec<(NodeKind, Span)> = symbols
            .iter()
            .map(|&(kind, _, _, span)| (kind, span))
            .collect();
        let parents = lexical_parents(&decls, lexical_scope_kinds(language));
        let mut by_name: FxHashMap<String, Vec<(Option<Span>, u32)>> = FxHashMap::default();
        for (offset, (&(kind, name, has_owner, _), parent)) in
            symbols.iter().zip(parents).enumerate()
        {
            let scope = parent.map(|parent| decls[parent as usize].1);
            let id = start + offset as u32;
            match admission {
                LexicalAdmission::ScopedOnly => {
                    if scope.is_some() {
                        by_name
                            .entry(name.to_string())
                            .or_default()
                            .push((scope, id));
                    }
                }
                LexicalAdmission::ScopedAndFileLevel if kind == NodeKind::Function => {
                    // The entry exists even when no candidate joins it: a
                    // class's own function named like the callee still
                    // keeps the call off the name tiers.
                    let candidates = by_name.entry(name.to_string()).or_default();
                    if scope.is_some() || !has_owner {
                        candidates.push((scope, id));
                    }
                }
                LexicalAdmission::ScopedAndFileLevel => {}
            }
        }
        let modules = symbols
            .iter()
            .filter(|&&(kind, ..)| kind == NodeKind::Module)
            .map(|&(.., span)| span)
            .collect();
        Self {
            by_name,
            modules,
            admission: Some(admission),
        }
    }

    fn scope_size(scope: Option<Span>) -> (u32, i64) {
        scope
            .map(|span| (span.2 - span.0, span.3 as i64 - span.1 as i64))
            .unwrap_or((u32::MAX, i64::MAX))
    }

    /// True when a module declared inside `scope` holds `caller`: the module
    /// does not see the items `scope` declares.
    fn walls_off(&self, scope: Span, caller: Span) -> bool {
        self.modules
            .iter()
            .any(|&m| m != scope && span_contains(scope, m) && span_contains(m, caller))
    }

    /// The lexical verdict of a bare call to `callee` from `caller`:
    /// [`Resolution::Bound`] to the innermost function in scope, at
    /// [`Tier::Lexical`]'s 1.0. [`Resolution::Unknown`]: the file declares no
    /// function of that name, so the name tiers decide; under
    /// [`LexicalAdmission::ScopedOnly`] also when no candidate's scope holds
    /// `caller`, since the name tiers never return a scoped node (invariant
    /// I5). [`Resolution::Shadowed`]: a candidate exists but none in reach
    /// binds (out of scope, or two in one scope); this prevents fallback to
    /// an inaccessible or ambiguous closure: only the import tiers may bind
    /// then.
    fn lookup(&self, caller: Span, callee: &str) -> Resolution {
        if callee.contains(['.', ':']) {
            return Resolution::Unknown;
        }
        let Some(candidates) = self.by_name.get(callee) else {
            return Resolution::Unknown;
        };
        let mut best = None;
        let mut ambiguous = false;
        for &(scope, target) in candidates {
            if scope
                .is_some_and(|span| !span_contains(span, caller) || self.walls_off(span, caller))
            {
                continue;
            }
            let size = Self::scope_size(scope);
            match best {
                Some((best_size, _)) if size > best_size => {}
                Some((best_size, _)) if size == best_size => ambiguous = true,
                _ => {
                    best = Some((size, target));
                    ambiguous = false;
                }
            }
        }
        match best {
            Some((_, target)) if !ambiguous => Resolution::bound(target, Tier::Lexical),
            None if self.admission == Some(LexicalAdmission::ScopedOnly) => Resolution::Unknown,
            _ => Resolution::Shadowed,
        }
    }

    /// The function a bare call to `callee` from `caller` binds, at 1.0.
    /// `None`: the name tiers decide ([`Resolution::Unknown`] of
    /// [`Self::lookup`]). An empty result is [`Resolution::Shadowed`].
    pub fn targets(&self, caller: Span, callee: &str) -> Option<Vec<(u32, f32)>> {
        match self.lookup(caller, callee) {
            Resolution::Unknown => None,
            Resolution::Shadowed => Some(Vec::new()),
            bound => Some(bound.edge().into_iter().collect()),
        }
    }

    /// [`Self::lookup`] for one call site, the verdict `ladder::call` reads
    /// ([`crate::analyzer::ladder::CallContext::lexical`]). Under
    /// [`LexicalAdmission::ScopedOnly`] only a bare call reaches a scoped
    /// function: a method-syntax site (`x.helper()`, a chain) names a
    /// method, so it is `Unknown` and the name tiers decide. JS / TS read
    /// the site's name, as before. `LexicalAdmission` stays an input of the
    /// verdict: the ladder's import-only path reads only its result.
    pub fn call_resolution(&self, caller: Span, site: CallSite<'_>) -> Resolution {
        match (self.admission, site) {
            (Some(LexicalAdmission::ScopedOnly), CallSite::Plain(name)) => {
                self.lookup(caller, name)
            }
            (Some(LexicalAdmission::ScopedOnly), _) => Resolution::Unknown,
            _ => self.lookup(caller, site.name()),
        }
    }

    /// The scoped function a bare name reference from `caller` reads
    /// (`xs.iter().for_each(helper)` inside `outer`, JS `setTimeout(helper)`
    /// inside `outer`): the one candidate [`Self::targets`] binds, when that
    /// candidate is scoped. `None` leaves the reference to the name tiers:
    /// no visible candidate, two in one scope, a constant reference (it
    /// never names a function), and a JS / TS file-level winner, which the
    /// name tiers bind as before. The name tiers never return a scoped node
    /// (invariant I5), so only this tier reaches one.
    pub fn name_ref_target(&self, caller: Span, name_ref: NameRef<'_>) -> Option<u32> {
        if self.admission.is_none() || name_ref.kind == NameRefKind::Constant {
            return None;
        }
        let target = match self.targets(caller, name_ref.name)?.as_slice() {
            [(target, _)] => *target,
            _ => return None,
        };
        self.by_name
            .get(name_ref.name)?
            .iter()
            .any(|&(scope, id)| id == target && scope.is_some())
            .then_some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::identity::{nested_uid, LaterReason, Uniqueness};

    /// Node `i` of `rows`: (stored owner, name, lexical parent).
    type ChainRow = (Option<&'static str>, &'static str, Option<usize>);

    fn chain(
        rows: &'static [ChainRow],
        start: usize,
    ) -> Option<(Option<&'static str>, Vec<&'static str>)> {
        display_chain(
            start,
            rows.len(),
            |i| rows.get(i).map(|&(owner, name, _)| (owner, name)),
            |i| rows.get(i).and_then(|row| row.2),
        )
    }

    #[test]
    fn test_display_chain_chain_of_three_names_parents_up_to_owner() {
        // 0 inner -> 1 pad -> 2 new, which `A` owns.
        const ROWS: &[ChainRow] = &[
            (None, "inner", Some(1)),
            (None, "pad", Some(2)),
            (Some("A"), "new", None),
        ];
        let parts = chain(ROWS, 0);
        assert_eq!(parts, Some((Some("A"), vec!["pad", "new"])));
        assert_eq!(
            display_owner_text(parts.unwrap()).as_deref(),
            Some("A::new::pad")
        );
        assert_eq!(lexical_chain_end(0, ROWS.len(), |i| ROWS[i].2), Some(2));
    }

    #[test]
    fn test_display_chain_cycle_returns_none() {
        const ROWS: &[ChainRow] = &[(None, "a", Some(1)), (None, "b", Some(0))];
        assert_eq!(chain(ROWS, 0), None);
        assert_eq!(lexical_chain_end(0, ROWS.len(), |i| ROWS[i].2), None);
    }

    #[test]
    fn test_display_chain_self_parent_returns_none() {
        const ROWS: &[ChainRow] = &[(None, "a", Some(0))];
        assert_eq!(chain(ROWS, 0), None);
        assert_eq!(lexical_chain_end(0, ROWS.len(), |i| ROWS[i].2), None);
    }

    #[test]
    fn test_display_chain_no_parent_reads_own_owner_or_none() {
        const ROWS: &[ChainRow] = &[(Some("K"), "m", None), (None, "f", None)];
        assert_eq!(chain(ROWS, 0), Some((Some("K"), Vec::new())));
        assert_eq!(
            display_owner_text(chain(ROWS, 0).unwrap()).as_deref(),
            Some("K")
        );
        assert_eq!(chain(ROWS, 1), Some((None, Vec::new())));
        assert_eq!(display_owner_text(chain(ROWS, 1).unwrap()), None);
        assert_eq!(lexical_chain_end(1, ROWS.len(), |i| ROWS[i].2), Some(1));
    }

    #[test]
    fn test_display_chain_unowned_parent_is_the_owner_text() {
        const ROWS: &[ChainRow] = &[(None, "helper", Some(1)), (None, "outer", None)];
        assert_eq!(
            display_owner_text(chain(ROWS, 0).unwrap()).as_deref(),
            Some("outer")
        );
    }

    #[test]
    fn test_display_chain_unknown_start_or_parent_returns_none() {
        const ROWS: &[ChainRow] = &[(None, "a", Some(7))];
        assert_eq!(chain(ROWS, 3), None);
        assert_eq!(chain(ROWS, 0), None);
    }

    fn index(path: &str, symbols: &[(NodeKind, &str, bool, Span)]) -> LexicalFunctions {
        LexicalFunctions::new(Path::new(path), symbols.iter().copied(), 10)
    }

    #[test]
    fn test_targets_two_top_level_twins_returns_empty() {
        let lex = index(
            "a.ts",
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 27)),
                (NodeKind::Function, "helper", false, (1, 0, 1, 27)),
                (NodeKind::Function, "run", false, (2, 0, 2, 40)),
            ],
        );
        assert_eq!(lex.targets((2, 0, 2, 40), "helper"), Some(Vec::new()));
    }

    #[test]
    fn test_targets_nested_function_shadows_top_level_returns_inner() {
        let lex = index(
            "a.js",
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 20)),
                (NodeKind::Function, "outer", false, (1, 0, 5, 1)),
                (NodeKind::Function, "helper", false, (2, 2, 2, 20)),
                (NodeKind::Function, "inner", false, (3, 2, 4, 3)),
            ],
        );
        assert_eq!(lex.targets((3, 2, 4, 3), "helper"), Some(vec![(12, 1.0)]));
        assert_eq!(lex.targets((6, 0, 6, 9), "helper"), Some(vec![(10, 1.0)]));
    }

    /// Stands for: a Rust `mod` inside a fn body is its own item namespace, so
    /// a bare `helper()` inside it never binds the enclosing fn's nested
    /// `helper`; outside the `mod` the nested one still wins.
    #[test]
    fn test_targets_rust_module_inside_fn_hides_outer_nested_fn_returns_none() {
        let lex = index(
            "src/a.rs",
            &[
                (NodeKind::Function, "outer", false, (0, 0, 7, 1)),
                (NodeKind::Function, "helper", false, (1, 4, 1, 20)),
                (NodeKind::Module, "m", false, (2, 4, 5, 5)),
                (NodeKind::Function, "helper", false, (3, 8, 3, 24)),
                (NodeKind::Function, "run", false, (4, 8, 4, 30)),
            ],
        );
        assert_eq!(lex.targets((4, 20, 4, 28), "helper"), None, "inside `m`");
        assert_eq!(lex.targets((6, 4, 6, 12), "helper"), Some(vec![(11, 1.0)]));
    }

    #[test]
    fn test_targets_owned_top_level_function_only_returns_empty() {
        let lex = index("a.ts", &[(NodeKind::Function, "make", true, (1, 2, 1, 20))]);
        assert_eq!(lex.targets((5, 0, 5, 9), "make"), Some(Vec::new()));
    }

    #[test]
    fn test_targets_qualified_or_unknown_callee_returns_none() {
        let lex = index("a.ts", &[(NodeKind::Function, "f", false, (0, 0, 0, 9))]);
        assert_eq!(lex.targets((1, 0, 1, 9), "o.f"), None);
        assert_eq!(lex.targets((1, 0, 1, 9), "g"), None);
    }

    fn uids(path: &str, decls: &[(NodeKind, Option<&str>, &str)]) -> Vec<u64> {
        file_uids(path, decls, |d| (d.0, d.1, d.2, (0, 0, 0, 0), 0))
    }

    /// Stands for: a method repeat is an overload, a repeated namespace (a
    /// scope container) a redefinition with a node of its own, and a merged
    /// field repeat a tombstone of its own overload uid (design uid §2.3).
    #[test]
    fn test_file_uids_overload_and_redefinition_returns_index_numbering() {
        let decls = [
            (NodeKind::Method, Some("A"), "save"),
            (NodeKind::Method, Some("A"), "save"),
            (NodeKind::Namespace, None, "Acme"),
            (NodeKind::Namespace, None, "Acme"),
            (NodeKind::Property, Some("A"), "x"),
            (NodeKind::Property, Some("A"), "x"),
        ];
        let save = uid::compute(NodeKind::Method, "src/A.cs", Some("A"), "save");
        let acme = uid::compute(NodeKind::Namespace, "src/A.cs", None, "Acme");
        let x = uid::compute(NodeKind::Property, "src/A.cs", Some("A"), "x");
        assert_eq!(
            uids("src/A.cs", &decls),
            [
                save,
                uid::overload(save, 1),
                acme,
                uid::overload(acme, 1),
                x,
                uid::tombstone(uid::overload(x, 1)),
            ],
        );
    }

    fn rows_f(lines: &[u32]) -> Vec<DeclFields<'static>> {
        lines
            .iter()
            .map(|&l| (NodeKind::Function, None, "f", (l, 0, l, 9), 0))
            .collect()
    }

    /// Stands for: two inputs of one path number as one file (design uid
    /// §2.3 "Two `LocalGraph`s of one path"): the second input's `f` is the
    /// third declaration of its base uid.
    #[test]
    fn test_path_identities_two_inputs_number_path_wide() {
        let (a, b) = (rows_f(&[0, 1]), rows_f(&[0]));
        let ids = path_identities("m.py", &[&a[..], &b[..]], |d| *d);
        assert_eq!(ids.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
        let base = uid::compute(NodeKind::Function, "m.py", None, "f");
        assert!(
            matches!(
                ids[1][0].slot,
                UidSlot::Later {
                    first: 0,
                    ordinal: 2,
                    reason: LaterReason::Repeat,
                    ..
                }
            ),
            "{:?}",
            ids[1][0]
        );
        let uids: Vec<u64> = ids.iter().flatten().map(|id| stored_uid(id.slot)).collect();
        assert_eq!(uids, [base, uid::overload(base, 1), uid::overload(base, 2)]);
    }

    /// Stands for: `file_identities` is the one-input case of
    /// `path_identities`.
    #[test]
    fn test_path_identities_one_input_equals_file_identities() {
        let rows = [
            (NodeKind::Function, None, "outer", (0, 0, 5, 1), 0),
            (NodeKind::Function, None, "h", (1, 4, 1, 20), 0),
            (NodeKind::Function, None, "h", (2, 4, 2, 20), 0),
            (NodeKind::Variable, None, "x", (6, 0, 6, 9), 0),
            (NodeKind::Variable, None, "x", (7, 0, 7, 9), 0),
        ];
        assert_eq!(
            path_identities("a.py", &[&rows[..]], |d| *d),
            [identities("a.py", &rows)]
        );
    }

    /// Stands for: each input's lexical parents index into that input
    /// only. An empty input between two others shifts nothing, and the
    /// span of one input never holds a row of another.
    #[test]
    fn test_path_identities_parents_per_input_empty_input_between() {
        let a = [
            (NodeKind::Function, None, "outer", (0, 0, 5, 1), 0),
            (NodeKind::Function, None, "h", (1, 4, 1, 20), 0),
        ];
        let empty: [DeclFields<'static>; 0] = [];
        let b = [(NodeKind::Function, None, "h", (1, 4, 1, 20), 0)];
        let ids = path_identities("a.py", &[&a[..], &empty[..], &b[..]], |d| *d);
        assert_eq!(ids.iter().map(Vec::len).collect::<Vec<_>>(), [2, 0, 1]);
        let single = identities("a.py", &a);
        assert_eq!(ids[0], single);
        assert_eq!(
            ids[1].len(),
            0,
            "an empty input gives an empty identity list"
        );
        assert_eq!(
            ids[2][0].slot,
            UidSlot::First(uid::compute(NodeKind::Function, "a.py", None, "h")),
            "input 2's `h` is top level in its own input"
        );
    }

    #[test]
    fn test_path_identities_no_inputs_or_empty_inputs_returns_empty() {
        let none: [&[DeclFields<'static>]; 0] = [];
        assert!(path_identities("a.py", &none, |d| *d).is_empty());
        let empty: [DeclFields<'static>; 0] = [];
        assert_eq!(
            path_identities("a.py", &[&empty[..], &empty[..]], |d| *d),
            vec![Vec::<DeclIdentity>::new(); 2]
        );
    }

    /// Stands for: a C# member's arity path reads the type twins of the
    /// whole path, so `Foo1<T>.foo` in input 2 is a declaration of its
    /// own beside input 1's `Foo1.foo`.
    #[test]
    fn test_path_identities_csharp_arity_twin_types_across_inputs() {
        let [c0, p0, c1, p1] = foo1_twins(0, 1);
        let (a, b) = ([c0, p0], [c1, p1]);
        let ids = path_identities("N/Foo1.cs", &[&a[..], &b[..]], |d| *d);
        assert!(
            matches!(
                ids[1][1].slot,
                UidSlot::Later {
                    first: 1,
                    reason: LaterReason::Arity,
                    ..
                }
            ),
            "{:?}",
            ids[1][1]
        );
    }

    #[test]
    fn test_file_uids_backslash_path_hashes_the_stored_spelling() {
        let decls = [(NodeKind::Function, None, "f")];
        let stored = match cfg!(windows) {
            true => "src/a.rs",
            false => "src\\a.rs",
        };
        assert_eq!(
            uids("src\\a.rs", &decls),
            [uid::compute(NodeKind::Function, stored, None, "f")],
            "the builder hashes the path as `os_path_to_slash` stores it"
        );
    }

    #[test]
    fn test_file_uids_dot_backslash_name_matches_stored_path() {
        let decls = [
            (NodeKind::Function, None, "f"),
            (NodeKind::Method, Some("A"), "g"),
        ];
        let stored = [
            uid::compute(NodeKind::Function, "./a.rs", None, "f"),
            uid::compute(NodeKind::Method, "./a.rs", Some("A"), "g"),
        ];
        assert_eq!(
            uids(".\\a.rs", &decls) == stored,
            cfg!(windows),
            "a Windows `.\\a.rs` is stored `./a.rs`; a Unix file named `.\\a.rs` keeps its name"
        );
        assert_eq!(uids("./a.rs", &decls), stored);
        assert_ne!(
            uids("./a.rs", &decls)[0],
            uid::compute(NodeKind::Function, "a.rs", None, "f"),
            "`./a.rs` and `a.rs` are two files"
        );
    }

    #[test]
    fn test_file_uids_doubled_dot_prefix_no_panic() {
        let decls = [(NodeKind::Function, None, "f")];
        let stored = [uid::compute(NodeKind::Function, "././a.rs", None, "f")];
        assert_eq!(uids(".\\.\\a.rs", &decls) == stored, cfg!(windows));
        assert_eq!(uids("././a.rs", &decls), stored);
    }

    #[test]
    fn test_file_uids_empty_file_returns_empty() {
        let decls: [(NodeKind, Option<&str>, &str); 0] = [];
        assert!(uids("src/a.rs", &decls).is_empty());
    }

    fn identities(path: &str, rows: &[DeclFields<'static>]) -> Vec<DeclIdentity> {
        file_identities(path, rows, |d| *d)
    }

    /// `impl <first> for A { fn fmt }` on lines 1-3, then `impl <second> for
    /// A { fn fmt }` on lines 4-6. A trait `""` is an inherent `impl`.
    fn two_impls(first: &'static str, second: &'static str) -> [DeclFields<'static>; 4] {
        [
            (NodeKind::Impl, Some(first), "A", (1, 0, 3, 1), 0),
            (NodeKind::Method, Some("A"), "fmt", (2, 4, 2, 40), 0),
            (NodeKind::Impl, Some(second), "A", (4, 0, 6, 1), 0),
            (NodeKind::Method, Some("A"), "fmt", (5, 4, 5, 40), 0),
        ]
    }

    /// The base uid of the `fmt` that `impl <trait_name> for A` declares in
    /// `path`: the `Impl` row is its container (design uid §2.5).
    fn fmt_in_impl(path: &str, trait_name: Option<&str>) -> u64 {
        let block = uid::compute(NodeKind::Impl, path, trait_name, "A");
        nested_uid(block, NodeKind::Method, Some("A"), "fmt")
    }

    /// Stands for: a Rust impl member hashes its `Impl` block, whose key
    /// holds the trait, so each `fmt` is the first of its own base uid and
    /// keeps its stored uid when the blocks swap. No discriminator enters.
    #[test]
    fn test_file_identities_rust_trait_impl_member_hashes_its_impl_block() {
        let before = identities("src/a.rs", &two_impls("Display", "Debug"));
        let after = identities("src/a.rs", &two_impls("Debug", "Display"));
        let (display, debug) = (
            fmt_in_impl("src/a.rs", Some("Display")),
            fmt_in_impl("src/a.rs", Some("Debug")),
        );
        assert_eq!(before[1].slot, UidSlot::First(display));
        assert_eq!(before[3].slot, UidSlot::First(debug));
        assert_eq!(
            (before[1].slot, before[3].slot),
            (after[3].slot, after[1].slot),
            "each `fmt` keeps its stored uid across the swap"
        );
        assert_eq!(before[1].anchor, display, "no discriminator");
        assert!(before.iter().all(|d| d.uniqueness == Uniqueness::Unique));
    }

    /// Stands for: an inherent `impl A` (owner `""`) is a container like a
    /// trait impl; its member hashes the block's `compute` uid.
    #[test]
    fn test_file_identities_rust_inherent_impl_member_hashes_inherent_block() {
        let ids = identities("src/a.rs", &two_impls("", "Display"));
        assert_eq!(ids[1].slot, UidSlot::First(fmt_in_impl("src/a.rs", None)));
        assert_eq!(ids[1].anchor, ids[1].slot.uid());
        assert_eq!(ids[3].anchor, fmt_in_impl("src/a.rs", Some("Display")));
        assert_eq!(ids[1].uniqueness, Uniqueness::Unique);
    }

    /// Stands for: a repeated `Impl` of the same trait for the same type
    /// (two inherent `impl A`) shares one anchor, so a same-named member of
    /// each shares one anchor too: `Shared`, the second a later node.
    #[test]
    fn test_file_identities_rust_two_inherent_impls_same_member_shared() {
        let ids = identities("src/a.rs", &two_impls("", ""));
        assert_eq!(ids[0].anchor, ids[2].anchor);
        assert_eq!(ids[1].anchor, ids[3].anchor);
        assert_eq!(
            ids[3].uniqueness,
            Uniqueness::Shared {
                position: 1,
                count: 2
            }
        );
        assert!(matches!(
            ids[3].slot,
            UidSlot::Later {
                first: 1,
                ordinal: 1,
                ..
            }
        ));
    }

    /// Stands for: a member outside every `Impl` span (a Go-style receiver
    /// method, a C++ out-of-line member) has no container and keeps
    /// `uid::compute`, owner text included.
    #[test]
    fn test_file_identities_member_outside_impl_span_keeps_compute() {
        let rows = [
            (NodeKind::Impl, Some("Display"), "A", (1, 0, 3, 1), 0),
            (NodeKind::Method, Some("A"), "fmt", (8, 4, 8, 40), 0),
        ];
        let ids = identities("src/a.rs", &rows);
        assert_eq!(
            ids[1].slot,
            UidSlot::First(uid::compute(NodeKind::Method, "src/a.rs", Some("A"), "fmt"))
        );
    }

    /// Stands for: the container chain is every language's (design uid
    /// §2.2), not Rust's: the same rows in a Swift file give each `fmt`
    /// the `Impl` row that holds it.
    #[test]
    fn test_file_identities_non_rust_impl_rows_chain_alike() {
        let ids = identities("A.swift", &two_impls("Display", "Debug"));
        assert_eq!(
            ids[1].slot,
            UidSlot::First(fmt_in_impl("A.swift", Some("Display")))
        );
        assert_ne!(ids[1].anchor, ids[3].anchor);
    }

    /// Stands for: design uid §2.4. An unrelated insert (a new top-level
    /// function above everything) and a swap of two same-named container
    /// blocks (`mod a`, `mod b`, each `struct S { fn m }`) change no other
    /// stored uid; in an empty file nothing is numbered.
    #[test]
    fn test_file_uids_unrelated_insert_and_block_swap_keep_other_uids() {
        type Block = [DeclFields<'static>; 3];
        let block = |module: &'static str, line: u32| -> Block {
            [
                (NodeKind::Module, None, module, (line, 0, line + 4, 1), 0),
                (
                    NodeKind::Struct,
                    Some(module),
                    "S",
                    (line + 1, 4, line + 3, 5),
                    0,
                ),
                (
                    NodeKind::Method,
                    Some("S"),
                    "m",
                    (line + 2, 8, line + 2, 30),
                    0,
                ),
            ]
        };
        // A row across edits: kind, name and the name of the module that
        // holds it; its lines move with the edit.
        type RowKey = (u8, &'static str, Option<&'static str>);
        let file = |rows: &[DeclFields<'static>]| -> FxHashMap<RowKey, u64> {
            let uids = file_uids("src/lib.rs", rows, |d| *d);
            rows.iter()
                .zip(uids)
                .map(|(r, uid)| {
                    let module = rows
                        .iter()
                        .find(|m| m.0 == NodeKind::Module && span_contains(m.3, r.3))
                        .map(|m| m.2);
                    ((r.0 as u8, r.2, module), uid)
                })
                .collect()
        };
        let (a, b) = (block("a", 1), block("b", 10));
        let before: Vec<DeclFields<'static>> = a.iter().chain(&b).copied().collect();
        let swapped: Vec<DeclFields<'static>> = block("b", 1)
            .iter()
            .chain(&block("a", 10))
            .copied()
            .collect();
        let inserted: Vec<DeclFields<'static>> = [(NodeKind::Function, None, "z", (0, 0, 0, 9), 0)]
            .iter()
            .chain(&before)
            .copied()
            .collect();
        let base = file(&before);
        assert_eq!(base.len(), 6, "every row keys apart: {base:?}");
        assert_eq!(file(&swapped), base, "a block swap");
        let mut grown = file(&inserted);
        assert!(grown
            .remove(&(NodeKind::Function as u8, "z", None))
            .is_some());
        assert_eq!(grown, base, "an unrelated insert");
        let ms: Vec<u64> = base
            .iter()
            .filter(|((kind, ..), _)| *kind == NodeKind::Method as u8)
            .map(|(_, &uid)| uid)
            .collect();
        assert_eq!(ms.len(), 2);
        assert_ne!(ms[0], ms[1], "`a::S::m` and `b::S::m` are two uids");
        assert!(file_uids("src/lib.rs", &[] as &[DeclFields<'static>], |d| *d).is_empty());
    }

    /// `Conv` of arity `first` on line 1, then `Conv` of arity `second` on
    /// line 2, both of `kind`.
    fn two_types(kind: NodeKind, first: u32, second: u32) -> [DeclFields<'static>; 2] {
        [
            (kind, None, "Conv", (1, 0, 1, 40), first),
            (kind, None, "Conv", (2, 0, 2, 40), second),
        ]
    }

    /// Stands for: a C# type of another generic arity takes overload uid 1
    /// in either declaration order and keeps its name, so every path that
    /// numbers the file through `file_uids` stores it (codex ruling P-a).
    #[test]
    fn test_file_uids_csharp_types_of_two_arities_both_orders_later_uid() {
        let base = uid::compute(NodeKind::Class, "N/Conv.cs", None, "Conv");
        for (first, second) in [(0, 1), (1, 0)] {
            let rows = two_types(NodeKind::Class, first, second);
            assert_eq!(
                file_uids("N/Conv.cs", &rows, |d| *d),
                [base, uid::overload(base, 1)],
                "arity {first} then {second}"
            );
        }
        let rows = two_types(NodeKind::Interface, 1, 2);
        let iface = uid::compute(NodeKind::Interface, "N/Conv.cs", None, "Conv");
        assert_eq!(
            file_uids("N/Conv.cs", &rows, |d| *d),
            [iface, uid::overload(iface, 1)]
        );
    }

    /// Stands for: two C# types of one name and one arity are a
    /// redefinition, a node of its own under overload uid 1 (design uid
    /// §2.3), no longer a tombstone.
    #[test]
    fn test_file_uids_csharp_types_of_one_arity_later_repeat() {
        let base = uid::compute(NodeKind::Class, "N/Conv.cs", None, "Conv");
        let rows = two_types(NodeKind::Class, 1, 1);
        assert_eq!(
            file_uids("N/Conv.cs", &rows, |d| *d),
            [base, uid::overload(base, 1)]
        );
    }

    /// Stands for: the arity rule is C#'s alone. Two same-named types in a
    /// Java or C++ file are a redefinition (`Repeat`), never an arity
    /// sibling, even when an arity is supplied, and a C# method takes no
    /// arity.
    #[test]
    fn test_file_identities_non_csharp_types_of_two_arities_later_repeat() {
        for path in ["N/Conv.java", "n/conv.hpp", "n/conv.ts"] {
            let ids = identities(path, &two_types(NodeKind::Class, 0, 1));
            assert!(
                matches!(
                    ids[1].slot,
                    UidSlot::Later {
                        first: 0,
                        reason: LaterReason::Repeat,
                        ..
                    }
                ),
                "{path}: {:?}",
                ids[1].slot
            );
        }
        assert!(!splits_by_arity(Language::CSharp, NodeKind::Method));
        assert!(splits_by_arity(Language::CSharp, NodeKind::Struct));
    }

    /// Stands for: C# delegates (`Function` nodes) of arity 0, 1 and 3 in
    /// one file are three declarations, in source order and reversed
    /// (Newtonsoft `LinqBridge.cs` `Func` / `Action`, codex ruling P-a2).
    /// Two of one arity are a redefinition under the next overload uid.
    #[test]
    fn test_file_uids_csharp_delegates_of_three_arities_both_orders_later_uids() {
        let base = uid::compute(NodeKind::Function, "N/Linq.cs", None, "Func");
        for arities in [[0, 1, 3], [3, 1, 0]] {
            let rows: Vec<DeclFields<'static>> = arities
                .iter()
                .enumerate()
                .map(|(i, &a)| {
                    (
                        NodeKind::Function,
                        None,
                        "Func",
                        (i as u32, 0, i as u32, 40),
                        a,
                    )
                })
                .collect();
            assert_eq!(
                file_uids("N/Linq.cs", &rows, |d| *d),
                [base, uid::overload(base, 1), uid::overload(base, 2)],
                "{arities:?}"
            );
        }
        let rows = two_types(NodeKind::Function, 2, 2);
        let conv = uid::compute(NodeKind::Function, "N/Conv.cs", None, "Conv");
        assert_eq!(
            file_uids("N/Conv.cs", &rows, |d| *d),
            [conv, uid::overload(conv, 1)]
        );
    }

    /// `class Foo1` (arity `first`, lines 1-4) with property `foo` on line
    /// 2, then `class Foo1` (arity `second`, lines 5-8) with property `foo`
    /// on line 6, in `path`.
    fn foo1_twins(first: u32, second: u32) -> [DeclFields<'static>; 4] {
        [
            (NodeKind::Class, None, "Foo1", (1, 4, 4, 5), first),
            (NodeKind::Property, Some("Foo1"), "foo", (2, 8, 2, 40), 0),
            (NodeKind::Class, None, "Foo1", (5, 4, 8, 5), second),
            (NodeKind::Property, Some("Foo1"), "foo", (6, 8, 6, 40), 0),
        ]
    }

    /// Stands for: `Foo1<T>.foo` beside `Foo1.foo` (Newtonsoft
    /// `TestObjects/Foo1.cs`) is a declaration of its own: overload uid 1 of
    /// the property's base uid, in either order, read off the spans alone,
    /// so the full build and the overlay view number it alike. A merge-class
    /// member of two classes of one arity stays a duplicate, whose tombstone
    /// hashes its own ordinal. The two classes share one anchor (the arity
    /// enters none), so both `foo` hash one container chain.
    #[test]
    fn test_file_uids_csharp_member_of_arity_twin_both_orders_later_uid() {
        let class = uid::compute(NodeKind::Class, "N/Foo1.cs", None, "Foo1");
        let foo = nested_uid(class, NodeKind::Property, Some("Foo1"), "foo");
        for (first, second) in [(0, 1), (1, 0)] {
            let uids = file_uids("N/Foo1.cs", &foo1_twins(first, second), |d| *d);
            assert_eq!(
                uids[3],
                uid::overload(foo, 1),
                "arity {first} then {second}"
            );
        }
        let uids = file_uids("N/Foo1.cs", &foo1_twins(0, 0), |d| *d);
        assert_eq!(
            uids[3],
            uid::tombstone(uid::overload(foo, 1)),
            "one arity: the second foo is a tombstone"
        );
    }

    /// Stands for: the member rule is C#'s alone. The same rows in a Java
    /// file keep the duplicate rule, whatever arity is supplied.
    #[test]
    fn test_file_identities_non_csharp_member_of_arity_twin_duplicate() {
        for path in ["N/Foo1.java", "n/foo1.ts", "n/foo1.hpp"] {
            let ids = identities(path, &foo1_twins(0, 1));
            assert!(
                matches!(ids[3].slot, UidSlot::Duplicate { first: 1, .. }),
                "{path}: {:?}",
                ids[3].slot
            );
        }
    }

    #[test]
    fn test_enclosing_types_skips_non_type_containers_returns_innermost_type() {
        let rows = [
            (NodeKind::Namespace, (0, 0, 20, 1)),
            (NodeKind::Class, (1, 4, 10, 5)),
            (NodeKind::Method, (2, 8, 5, 9)),
            (NodeKind::Function, (3, 12, 4, 13)),
            (NodeKind::Class, (6, 8, 9, 9)),
            (NodeKind::Property, (7, 12, 7, 30)),
            (NodeKind::Property, (12, 4, 12, 30)),
        ];
        assert_eq!(
            enclosing_types(&rows),
            [None, None, Some(1), Some(1), Some(1), Some(4), None]
        );
        assert!(enclosing_types(&[]).is_empty());
    }

    /// Stands for: a merged node stays body-local only when every twin
    /// merged into it is body-local too; a module-level twin makes it a
    /// module item, in any twin order.
    #[test]
    fn test_fold_twins_module_level_twin_clears_first_body_local_twins_keep_it() {
        let mut empty: [bool; 0] = [];
        fold_twins(&mut empty, []);
        let mut local = [true, false, true, true, false];
        fold_twins(&mut local, []);
        assert_eq!(local, [true, false, true, true, false], "no twins");
        fold_twins(&mut local, [(2, 0)]);
        assert_eq!(local, [true, false, true, true, false], "a body-local twin");
        fold_twins(&mut local, [(2, 0), (4, 0), (3, 0)]);
        assert_eq!(
            local,
            [false, false, true, true, false],
            "a module-level twin"
        );
        let mut module_level_first = [false, true];
        fold_twins(&mut module_level_first, [(1, 0)]);
        assert_eq!(module_level_first, [false, true]);
    }

    #[test]
    fn test_twin_enclosing_reader_inside_one_scope_returns_innermost_else_none() {
        let outer = (1, 0, 20, 1);
        let inner = (5, 4, 9, 5);
        let other = (21, 0, 30, 1);
        let twins = [(10, outer), (11, inner), (12, other)];
        assert_eq!(twin_enclosing(twins, (6, 8, 6, 20)), Some(11));
        assert_eq!(twin_enclosing(twins, (2, 8, 2, 20)), Some(10));
        assert_eq!(twin_enclosing(twins, (25, 8, 25, 20)), Some(12));
        assert_eq!(twin_enclosing(twins, (40, 0, 40, 9)), None);
        assert_eq!(twin_enclosing([], (6, 8, 6, 20)), None);
    }

    #[test]
    fn test_lexical_scope_kinds_before_opt_in_returns_empty() {
        for language in [
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Go,
            Language::Php,
            Language::Ruby,
            Language::Swift,
            Language::C,
            Language::Cpp,
            Language::Dart,
        ] {
            assert!(lexical_scope_kinds(language).is_empty(), "{language:?}");
        }
    }

    #[test]
    fn test_lexical_scope_kinds_rust_returns_function_only() {
        assert_eq!(lexical_scope_kinds(Language::Rust), [NodeKind::Function]);
    }

    /// Stands for: slice D opts Python, JS and TS in with `Function` only;
    /// JS / TS keep the file-level admission, Python takes Rust's.
    #[test]
    fn test_lexical_scope_kinds_python_js_ts_return_function_only() {
        for language in [Language::Python, Language::JavaScript, Language::TypeScript] {
            assert_eq!(
                lexical_scope_kinds(language),
                [NodeKind::Function],
                "{language:?}"
            );
        }
        assert_eq!(
            lexical_admission(Language::Python),
            Some(LexicalAdmission::ScopedOnly)
        );
        assert_eq!(
            lexical_admission(Language::JavaScript),
            Some(LexicalAdmission::ScopedAndFileLevel)
        );
        assert_eq!(
            lexical_admission(Language::TypeScript),
            Some(LexicalAdmission::ScopedAndFileLevel)
        );
        assert_eq!(lexical_admission(Language::Go), None);
    }

    /// `def helper` (line 0); `class A` (lines 1-9) with `def m` (lines
    /// 2-5) nesting `def helper` (line 3) and `def n` (lines 6-8); `def
    /// outer` (lines 10-14) nesting `def helper` (line 11) and a lambda
    /// node (line 12); in `path`, ids from 10.
    fn python_nested(path: &str) -> LexicalFunctions {
        index(
            path,
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 20)),
                (NodeKind::Class, "A", false, (1, 0, 9, 1)),
                (NodeKind::Method, "m", true, (2, 4, 5, 5)),
                (NodeKind::Function, "helper", false, (3, 8, 3, 30)),
                (NodeKind::Method, "n", true, (6, 4, 8, 5)),
                (NodeKind::Function, "outer", false, (10, 0, 14, 1)),
                (NodeKind::Function, "helper", false, (11, 4, 11, 30)),
                (
                    NodeKind::Function,
                    "<anonymous:13:8>",
                    false,
                    (12, 8, 12, 40),
                ),
            ],
        )
    }

    /// Stands for: Python is `ScopedOnly`. Inside `A.m` and inside `outer`
    /// (and its lambda) the nested `helper` wins; from `A.n`, which sees no
    /// nested `helper`, the tier returns `None` so the name tiers bind the
    /// module-level one; a method-syntax site never reaches a nested def.
    #[test]
    fn test_targets_python_scoped_only_nested_binds_other_method_returns_none() {
        let lex = python_nested("pkg/a.py");
        assert_eq!(lex.targets((4, 8, 4, 20), "helper"), Some(vec![(13, 1.0)]));
        assert_eq!(lex.targets((7, 8, 7, 20), "helper"), None, "A.n");
        assert_eq!(
            lex.targets((13, 4, 13, 20), "helper"),
            Some(vec![(16, 1.0)])
        );
        assert_eq!(
            lex.targets((12, 8, 12, 40), "helper"),
            Some(vec![(16, 1.0)]),
            "the lambda inside `outer`"
        );
        assert_eq!(lex.targets((15, 0, 15, 9), "helper"), None, "module level");
        assert_eq!(
            lex.targets((13, 4, 13, 20), "outer"),
            None,
            "unscoped: no entry"
        );
        assert_eq!(
            lex.call_resolution((4, 8, 4, 20), CallSite::UntypedMember("helper")),
            Resolution::Unknown
        );
        assert_eq!(
            python_nested("pkg/a.pyi").targets((4, 8, 4, 20), "helper"),
            Some(vec![(13, 1.0)]),
            "a stub file is Python too"
        );
    }

    /// Stands for: JS / TS read `lexical_parents`, the parent derivation the
    /// uid reads: the nested `helper` in `A.m` scopes to `m`, the one in
    /// `outer` to `outer` and is visible from the callback inside `outer`,
    /// and `A.n` keeps the file-level `helper` (`ScopedAndFileLevel`). Every
    /// extension the router maps to JS / TS gets the tier; a file the router
    /// maps to neither gets none.
    #[test]
    fn test_new_js_ts_extensions_scope_by_lexical_parents() {
        for path in ["a.js", "a.jsx", "a.mjs", "a.cjs", "a.ts", "a.tsx"] {
            let lex = python_nested(path);
            assert_eq!(
                lex.targets((4, 8, 4, 20), "helper"),
                Some(vec![(13, 1.0)]),
                "{path}"
            );
            assert_eq!(
                lex.targets((7, 8, 7, 20), "helper"),
                Some(vec![(10, 1.0)]),
                "{path}: `A.n` binds the file-level helper"
            );
            assert_eq!(
                lex.targets((12, 8, 12, 40), "helper"),
                Some(vec![(16, 1.0)]),
                "{path}"
            );
        }
        for path in ["a.vue", "a.svelte", "a.go"] {
            assert_eq!(
                python_nested(path).targets((4, 8, 4, 20), "helper"),
                None,
                "{path}: no lexical tier"
            );
        }
    }

    #[test]
    fn test_new_empty_file_or_whitespace_name_returns_none() {
        for path in ["a.py", "a.ts"] {
            let lex = index(path, &[]);
            assert_eq!(lex.targets((0, 0, 0, 9), "helper"), None, "{path}");
        }
        let lex = index(
            "a.py",
            &[
                (NodeKind::Function, "outer", false, (0, 0, 4, 1)),
                (NodeKind::Function, " ", false, (1, 4, 1, 20)),
            ],
        );
        assert_eq!(lex.targets((2, 4, 2, 9), " "), Some(vec![(11, 1.0)]));
        assert_eq!(lex.targets((2, 4, 2, 9), "helper"), None);
    }

    /// `fn helper` (line 0), `fn outer` (lines 1-5) with a nested `fn
    /// helper` (line 2), `fn b` (lines 6-8), in `path`; ids from 10.
    fn rust_nested(path: &str) -> LexicalFunctions {
        index(
            path,
            &[
                (NodeKind::Function, "helper", false, (0, 0, 0, 20)),
                (NodeKind::Function, "outer", false, (1, 0, 5, 1)),
                (NodeKind::Function, "helper", false, (2, 4, 2, 24)),
                (NodeKind::Function, "b", false, (6, 0, 8, 1)),
            ],
        )
    }

    /// Stands for: under `ScopedOnly` the nested `helper` wins inside
    /// `outer`, and a caller it is invisible to gets `None` (the name tiers
    /// decide) instead of JS's empty "import tiers only" answer, so `b`
    /// still reaches the file-level `helper`.
    #[test]
    fn test_targets_rust_scoped_only_visible_binds_invisible_returns_none() {
        let lex = rust_nested("src/a.rs");
        assert_eq!(lex.targets((3, 4, 3, 12), "helper"), Some(vec![(12, 1.0)]));
        assert_eq!(lex.targets((1, 0, 5, 1), "helper"), Some(vec![(12, 1.0)]));
        assert_eq!(lex.targets((7, 4, 7, 12), "helper"), None);
        assert_eq!(
            lex.targets((3, 4, 3, 12), "outer"),
            None,
            "unscoped: no entry"
        );
        let js = rust_nested("src/a.js");
        assert_eq!(
            js.targets((7, 4, 7, 12), "helper"),
            Some(vec![(10, 1.0)]),
            "control: JS admits the file-level function"
        );
    }

    #[test]
    fn test_targets_rust_two_scoped_in_one_scope_returns_empty() {
        let lex = index(
            "src/a.rs",
            &[
                (NodeKind::Function, "outer", false, (1, 0, 5, 1)),
                (NodeKind::Function, "helper", false, (2, 4, 2, 24)),
                (NodeKind::Function, "helper", false, (3, 4, 3, 24)),
            ],
        );
        assert_eq!(lex.targets((4, 4, 4, 12), "helper"), Some(Vec::new()));
    }

    /// Stands for: a Rust method-syntax site (`x.helper()`, an untyped
    /// member) never reaches a nested `fn helper`; a bare call does.
    #[test]
    fn test_call_resolution_rust_method_syntax_is_unknown_bare_binds() {
        let lex = rust_nested("src/a.rs");
        let caller = (3, 4, 3, 12);
        assert_eq!(
            lex.call_resolution(caller, CallSite::Plain("helper")),
            Resolution::bound(12, Tier::Lexical)
        );
        assert_eq!(
            lex.call_resolution(caller, CallSite::UntypedMember("helper")),
            Resolution::Unknown
        );
        assert_eq!(
            lex.call_resolution(caller, CallSite::Plain("A.helper")),
            Resolution::Unknown
        );
        assert_eq!(
            rust_nested("src/a.ts").call_resolution(caller, CallSite::UntypedMember("helper")),
            Resolution::bound(12, Tier::Lexical),
            "control: JS / TS keep reading the site's name"
        );
    }

    /// Stands for: a name a candidate declares but none in reach binds is
    /// `Shadowed`, not `Unknown`: JS / TS for a function out of reach and for
    /// a class's own function of the name, Rust and Python for two in one
    /// scope. A `ScopedOnly` caller with no candidate in reach is `Unknown`.
    #[test]
    fn test_call_resolution_declared_but_out_of_reach_is_shadowed() {
        let js = rust_nested("src/a.js");
        assert_eq!(
            js.call_resolution((7, 4, 7, 12), CallSite::Plain("helper")),
            Resolution::bound(10, Tier::Lexical),
            "control: the file-level helper is in reach of `b`"
        );
        let nested_elsewhere = index(
            "src/a.ts",
            &[
                (NodeKind::Function, "outer", false, (1, 0, 5, 1)),
                (NodeKind::Function, "helper", false, (2, 4, 2, 24)),
                (NodeKind::Function, "run", false, (6, 0, 8, 1)),
            ],
        );
        assert_eq!(
            nested_elsewhere.call_resolution((7, 4, 7, 12), CallSite::Plain("helper")),
            Resolution::Shadowed
        );
        let two_in_one_scope = index(
            "src/a.rs",
            &[
                (NodeKind::Function, "outer", false, (1, 0, 5, 1)),
                (NodeKind::Function, "helper", false, (2, 4, 2, 24)),
                (NodeKind::Function, "helper", false, (3, 4, 3, 24)),
            ],
        );
        assert_eq!(
            two_in_one_scope.call_resolution((4, 4, 4, 12), CallSite::Plain("helper")),
            Resolution::Shadowed
        );
        assert_eq!(
            rust_nested("src/a.rs").call_resolution((7, 4, 7, 12), CallSite::Plain("helper")),
            Resolution::Unknown,
            "ScopedOnly: no candidate in reach leaves the name tiers"
        );
    }

    #[test]
    fn test_name_ref_target_rust_value_binds_constant_returns_none() {
        let caller = (3, 4, 3, 12);
        let value = NameRef {
            name: "helper",
            kind: NameRefKind::Value,
        };
        let constant = NameRef {
            kind: NameRefKind::Constant,
            ..value
        };
        assert_eq!(
            rust_nested("src/a.rs").name_ref_target(caller, value),
            Some(12)
        );
        assert_eq!(
            rust_nested("src/a.rs").name_ref_target(caller, constant),
            None
        );
        assert_eq!(
            rust_nested("src/a.rs").name_ref_target((7, 4, 7, 12), value),
            None,
            "invisible: the name tiers decide"
        );
    }

    /// Stands for: a JS / TS / Python reference to a visible nested
    /// function (`setTimeout(helper)` inside `outer`) binds it here, since
    /// the name tiers never return a scoped node (I5); a JS file-level
    /// winner stays with the name tiers, as before R8. Before R8 the JS arm
    /// returned `None` for every reference: the nested function carried
    /// the owner `outer` and the name tiers could see it.
    #[test]
    fn test_name_ref_target_js_py_scoped_binds_file_level_returns_none() {
        let caller = (3, 4, 3, 12);
        let value = NameRef {
            name: "helper",
            kind: NameRefKind::Value,
        };
        for path in ["src/a.js", "src/a.ts", "src/a.py"] {
            assert_eq!(
                rust_nested(path).name_ref_target(caller, value),
                Some(12),
                "{path}"
            );
            assert_eq!(
                rust_nested(path).name_ref_target(
                    caller,
                    NameRef {
                        kind: NameRefKind::Constant,
                        ..value
                    }
                ),
                None,
                "{path}: a constant never names a function"
            );
        }
        assert_eq!(
            rust_nested("src/a.js").name_ref_target((7, 4, 7, 12), value),
            None,
            "the file-level winner: the name tiers decide"
        );
        assert_eq!(rust_nested("src/a.go").name_ref_target(caller, value), None);
    }

    fn raw(kind: NodeKind, name: &str, owner: Option<&str>, span: Span) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: owner.map(str::to_string),
            content_hash: 0,
        }
    }

    /// Stands for: invariant I1. The nested `fn` and the closure lose their
    /// owner; a module member, a method and a function a module puts
    /// between it and the enclosing callable keep theirs; a language that
    /// scopes nothing keeps every owner.
    #[test]
    fn test_clear_scoped_owners_rust_scoped_cleared_unscoped_kept() {
        let nodes = || {
            vec![
                raw(NodeKind::Module, "tests", None, (0, 0, 20, 1)),
                raw(NodeKind::Function, "helper", Some("tests"), (1, 4, 1, 20)),
                raw(NodeKind::Function, "t", Some("tests"), (2, 4, 10, 5)),
                raw(NodeKind::Function, "inner", Some("tests"), (3, 8, 3, 24)),
                raw(
                    NodeKind::Function,
                    "<anonymous:5:8>",
                    Some("tests"),
                    (4, 8, 4, 30),
                ),
                raw(NodeKind::Module, "m", Some("tests"), (5, 8, 7, 9)),
                raw(NodeKind::Function, "f", Some("m"), (6, 12, 6, 30)),
                raw(NodeKind::Impl, "A", Some(""), (11, 4, 15, 5)),
                raw(NodeKind::Method, "new", Some("A"), (12, 8, 14, 9)),
                raw(NodeKind::Function, "pad", Some("A"), (13, 12, 13, 30)),
            ]
        };
        let mut rust = nodes();
        clear_scoped_owners(Language::Rust, &mut rust);
        let owners: Vec<Option<&str>> = rust.iter().map(|n| n.owner_class.as_deref()).collect();
        assert_eq!(
            owners,
            [
                None,
                Some("tests"),
                Some("tests"),
                None,
                None,
                Some("tests"),
                Some("m"),
                Some(""),
                Some("A"),
                None,
            ]
        );
        let mut go = nodes();
        clear_scoped_owners(Language::Go, &mut go);
        assert!(go
            .iter()
            .zip(nodes())
            .all(|(a, b)| a.owner_class == b.owner_class));
        clear_scoped_owners(Language::Rust, &mut []);
    }

    /// Stands for: invariant I1 in Python, JS and TS. A def nested in a
    /// method loses the class owner the span pass gave it, a def nested in
    /// a def loses the function owner, two levels deep included; the
    /// method, a local class and a variable keep theirs.
    #[test]
    fn test_clear_scoped_owners_python_js_ts_scoped_cleared_unscoped_kept() {
        let nodes = || {
            vec![
                raw(NodeKind::Class, "A", None, (0, 0, 10, 1)),
                raw(NodeKind::Method, "m", Some("A"), (1, 4, 9, 5)),
                raw(NodeKind::Function, "helper", Some("A"), (2, 8, 8, 9)),
                raw(NodeKind::Function, "deep", Some("A"), (3, 12, 4, 13)),
                raw(NodeKind::Class, "Local", Some("helper"), (5, 12, 6, 13)),
                raw(NodeKind::Variable, "x", Some("helper"), (7, 12, 7, 20)),
            ]
        };
        for language in [Language::Python, Language::JavaScript, Language::TypeScript] {
            let mut cleared = nodes();
            clear_scoped_owners(language, &mut cleared);
            let owners: Vec<Option<&str>> =
                cleared.iter().map(|n| n.owner_class.as_deref()).collect();
            assert_eq!(
                owners,
                [None, Some("A"), None, None, Some("helper"), Some("helper")],
                "{language:?}"
            );
        }
    }

    #[test]
    fn test_new_non_lexical_language_path_returns_empty_index() {
        let lex = index("a.go", &[(NodeKind::Function, "f", false, (0, 0, 0, 9))]);
        assert_eq!(lex.targets((1, 0, 1, 9), "f"), None);
    }

    /// Stands for: a Python module-level function is unscoped, so the
    /// `ScopedOnly` tier leaves a call to it to the name tiers.
    #[test]
    fn test_new_python_module_level_function_returns_none() {
        let lex = index("a.py", &[(NodeKind::Function, "f", false, (0, 0, 0, 9))]);
        assert_eq!(lex.targets((1, 0, 1, 9), "f"), None);
    }

    /// `fn setup() { struct Foo; impl Foo { fn new() } }` beside a
    /// module-level `struct Bar` and `impl Bar { fn new() }`: the struct,
    /// the impl block and its method inside `setup` are body-local, the
    /// module-level items and `setup` itself are not. The span form agrees.
    #[test]
    fn test_body_locals_types_and_impl_members_inside_a_fn_body_are_body_local() {
        let decls = [
            (NodeKind::Function, (0, 0, 5, 1)),
            (NodeKind::Struct, (1, 4, 1, 15)),
            (NodeKind::Impl, (2, 4, 4, 5)),
            (NodeKind::Method, (3, 8, 3, 30)),
            (NodeKind::Struct, (6, 0, 6, 11)),
            (NodeKind::Impl, (7, 0, 9, 1)),
            (NodeKind::Method, (8, 4, 8, 26)),
        ];
        let want = [false, true, true, true, false, false, false];
        assert_eq!(body_locals(&decls, &decls_enclosing(&decls)), want);
        let callables: Vec<Span> = decls
            .iter()
            .filter(|(kind, _)| kind.is_callable())
            .map(|&(_, span)| span)
            .collect();
        for (&(_, span), want) in decls.iter().zip(want) {
            assert_eq!(inside_callable(&callables, span), want, "{span:?}");
        }
        assert!(body_locals(&[], &[]).is_empty());
    }
}
