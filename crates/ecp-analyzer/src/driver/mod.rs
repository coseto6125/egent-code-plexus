//! The shared parse driver: one tree parse, one query loop and an ordered
//! step list per file; a language supplies the rest as an `impl ParseSpec`.
//! A spec must reproduce its old `LocalGraph` byte for byte, record order
//! included: the parse cache serves graphs the old parser wrote.

mod cx;
pub mod imports;
pub mod node;
mod records;

pub use cx::{AnonKey, Anonymous, DeclSlot, FileCx, ScopeLanguage, TestRule};

use crate::calls::{self, NameRefSpec, VariantRefSpec};
use crate::event_topic::EventTopicConfig;
use crate::framework_helpers;
use crate::schema_field::SchemaFieldConfig;
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::lexical;
use ecp_core::analyzer::parse_prof::{self, Pass};
use ecp_core::analyzer::types::{LocalGraph, RawFunctionMeta, RawNode, RawTxScope};
use ecp_core::contracts::raw_records::ParsePhase;
use ecp_core::graph::{FileCategory, NodeKind};
use imports::ImportOrder;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

/// One language's half of a parse. Every hook and per-match method is
/// monomorphised into `parse::<Self>`, so the match loop makes no `dyn` call.
pub trait ParseSpec: LangSpec + Sized + 'static {
    /// A capture the spec handles itself, conditional blind spots included.
    type Role: Copy;
    /// Per-match buffers; a fresh default per match, as today's loop locals.
    type Match<'t>: Default;
    /// Per-file side tables that `finish` moves into the graph.
    type File<'t>: Default;
    type Hook: Copy;

    const PARSE_ERROR: &'static str = "Failed to parse file";
    const KIND_CAPTURE: KindCapture;
    const ANONYMOUS: Option<Anonymous> = None;
    /// `(capture, (kind, hint))` for blind spots pushed on every capture.
    /// A blind spot behind a predicate is a role.
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[];
    const TEST_RULE: TestRule = TestRule::Category;
    const SCOPE_LANGUAGE: ScopeLanguage = ScopeLanguage::FromPath;
    const FIELD_READS: Option<FieldReads> = None;
    const VARIANT_REFS: Option<VariantRefSpec> = None;
    const EVENT_TOPICS: &'static [EventTopicConfig] = &[];
    const SCHEMA_FIELDS: &'static [SchemaFieldConfig] = &[];
    const TX: TxRule = TxRule::None;
    /// Patches the bytes the tree parses; every step still reads the
    /// original source, so the patch may only overwrite bytes in place.
    const PREPARE_SOURCE: Option<PrepareSource> = None;
    const POST_TREE: Option<fn(Tree, &[u8]) -> Tree> = None;
    const STEPS: &'static [Step<Self::Hook>];

    fn query() -> anyhow::Result<Query>;
    fn role(capture: &str) -> Option<Self::Role>;
    fn parse_tree(source: &[u8]) -> Option<Tree>;

    fn capture<'t>(
        r: Self::Role,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut Self::Match<'t>,
        f: &mut Self::File<'t>,
        cx: &mut FileCx<'t>,
    );
    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut Self::Match<'t>,
        f: &mut Self::File<'t>,
        cx: &mut FileCx<'t>,
    );
    /// `c` gives a pre-query hook the query and the role table, so a
    /// pre-scan dispatches by capture index as the main loop does.
    fn hook<'t>(h: Self::Hook, c: &Compiled<Self>, f: &mut Self::File<'t>, cx: &mut FileCx<'t>);
    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta>;
    /// Runs after every step; it may only move `File` tables into `cx`
    /// fields that no step reads. `cx` (not the graph) keeps `'t` late-bound,
    /// so an impl may name its concrete `File` type.
    fn finish<'t>(_f: Self::File<'t>, _cx: &mut FileCx<'t>) {}
}

/// What a kind capture (a `CAPTURE_KIND` name) writes into the `DeclSlot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindCapture {
    /// `name` and `kind` on every capture.
    NameOverwrite,
    /// Rust: `name` on every capture, `kind` only while it is empty.
    NameKeepFirst,
    /// Ruby: `root` and `kind` on every capture, never `name`.
    RootOverwrite,
}

#[derive(Clone, Copy)]
pub struct Step<H> {
    pub pass: Pass,
    pub effect: Effect,
    pub op: Op<H>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// May push, remove, reorder or retag a node, or write a `RawNode` field.
    Nodes,
    /// Reads nodes and writes only its own record field.
    Neutral,
    Bind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op<H> {
    Query,
    Hook(H),
    SortDedupImports(ImportOrder),
    /// Consecutive duplicates only, as `Vec::dedup_by`.
    DedupNodes,
    FieldReads,
    VariantRefs,
    FunctionMeta,
    StampOwners,
    StampOwnerFns,
    ClearScopedOwners,
    TxScopes,
    EventTopicsRequery,
    SchemaFieldsRequery,
}

impl<H> Op<H> {
    /// The effect every fixed op has; `None` for a hook, which declares its own.
    pub const fn fixed_effect(&self) -> Option<Effect> {
        match self {
            Op::Hook(_) => None,
            Op::Query
            | Op::DedupNodes
            | Op::FieldReads
            | Op::VariantRefs
            | Op::StampOwners
            | Op::StampOwnerFns
            | Op::ClearScopedOwners => Some(Effect::Nodes),
            Op::TxScopes => Some(Effect::Bind),
            Op::SortDedupImports(_)
            | Op::FunctionMeta
            | Op::EventTopicsRequery
            | Op::SchemaFieldsRequery => Some(Effect::Neutral),
        }
    }
}

pub struct FieldReads {
    pub member_kinds: &'static [&'static str],
    pub names: NameRefSpec,
}

pub type PrepareSource = fn(&[u8]) -> Option<Vec<u8>>;
pub type TxFromNodes = fn(&[RawNode], &[NodeKind]) -> Option<Box<[RawTxScope]>>;
pub type TxFromTree = fn(Node<'_>, &[u8], &[RawNode]) -> Option<Box<[RawTxScope]>>;

#[derive(Clone, Copy)]
pub enum TxRule {
    None,
    /// `kinds` is each caller's accepted-kind list, passed unchanged.
    Nodes {
        collect: TxFromNodes,
        kinds: &'static [NodeKind],
    },
    Tree(TxFromTree),
    /// Python: bind `FileCx::tx_sites` to the innermost enclosing callable.
    Sites,
}

/// No `Nodes` step after a `Bind` step, every fixed op declares its fixed
/// effect, and exactly one step runs the query. A hook's real writes are not
/// checked here; the debug node-count assertion and the dump catch them.
pub const fn steps_ok<H>(steps: &[Step<H>]) -> bool {
    let mut bound = false;
    let mut queries = 0;
    let mut i = 0;
    while i < steps.len() {
        let step = &steps[i];
        if let Some(fixed) = step.op.fixed_effect() {
            if fixed as u8 != step.effect as u8 {
                return false;
            }
        }
        match step.effect {
            Effect::Nodes if bound => return false,
            Effect::Bind => bound = true,
            _ => {}
        }
        if matches!(step.op, Op::Query) {
            queries += 1;
        }
        i += 1;
    }
    queries == 1
}

/// A `FieldReads` or `VariantRefs` step with a `None` constant would
/// silently extract nothing.
const fn params_ok<S: ParseSpec>() -> bool {
    let mut i = 0;
    while i < S::STEPS.len() {
        match S::STEPS[i].op {
            Op::FieldReads if S::FIELD_READS.is_none() => return false,
            Op::VariantRefs if S::VARIANT_REFS.is_none() => return false,
            _ => i += 1,
        }
    }
    true
}

#[derive(Clone, Copy)]
enum Slot<R> {
    Kind(NodeKind),
    Blind(usize),
    Anon,
    Lang(R),
    Skip,
}

pub struct Compiled<S: ParseSpec> {
    query: Query,
    table: Box<[Slot<S::Role>]>,
}

impl<S: ParseSpec> Compiled<S> {
    /// # Errors
    /// The query does not compile, or one capture name resolves to two of
    /// kind, blind, anonymous and role: today the if-chain order would pick
    /// one silently.
    pub fn new() -> anyhow::Result<Self> {
        const {
            assert!(
                steps_ok(S::STEPS),
                "STEPS: a node-changing step follows a bind step, a fixed op declares the wrong effect, or the query step is not exactly one"
            );
            assert!(
                params_ok::<S>(),
                "STEPS lists FieldReads or VariantRefs with no spec"
            );
        }
        let query = S::query()?;
        let table = query
            .capture_names()
            .iter()
            .map(slot_for::<S>)
            .collect::<anyhow::Result<_>>()?;
        Ok(Self { query, table })
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn role_at(&self, capture_index: u32) -> Option<S::Role> {
        match self.table.get(capture_index as usize) {
            Some(Slot::Lang(r)) => Some(*r),
            _ => None,
        }
    }
}

fn slot_for<S: ParseSpec>(name: &&str) -> anyhow::Result<Slot<S::Role>> {
    let kind = S::CAPTURE_KIND.get(name).copied().map(Slot::Kind);
    let name: &str = name;
    let blind = S::BLIND
        .iter()
        .position(|(n, _)| *n == name)
        .map(Slot::Blind);
    let anon = S::ANONYMOUS.and_then(|a| (a.capture == name).then_some(Slot::Anon));
    let lang = S::role(name).map(Slot::Lang);
    let mut found = [kind, blind, anon, lang].into_iter().flatten();
    let slot = found.next().unwrap_or(Slot::Skip);
    if found.next().is_some() {
        anyhow::bail!(
            "{}: capture `@{name}` resolves to two of kind, blind, anonymous and role",
            S::NAME
        );
    }
    Ok(slot)
}

/// Parses one file: the tree phase, then `S::STEPS` in order, then the
/// assembly and `S::finish`.
///
/// # Errors
/// The tree parse fails or exceeds its budget (`S::PARSE_ERROR`).
pub fn parse<S: ParseSpec>(
    c: &Compiled<S>,
    path: &Path,
    source: &[u8],
) -> anyhow::Result<LocalGraph> {
    let tree = tree_phase::<S>(source)?;
    let mut cx = FileCx::new(&tree, source, path, S::TEST_RULE);
    let mut file: S::File<'_> = Default::default();
    let bound_len = run_steps(c, &mut file, &mut cx);
    S::finish(file, &mut cx);
    if let Some(len) = bound_len {
        debug_assert_eq!(
            cx.nodes.len(),
            len,
            "{}: finish changed the node count after a bind step",
            S::NAME
        );
    }
    Ok(cx.into_graph())
}

fn tree_phase<S: ParseSpec>(source: &[u8]) -> anyhow::Result<Tree> {
    let patched = S::PREPARE_SOURCE.and_then(|prepare| {
        let _p = parse_prof::span(Pass::Walk);
        prepare(source)
    });
    let tree = {
        let _p = parse_prof::span(Pass::Tree);
        let tree = S::parse_tree(patched.as_deref().unwrap_or(source));
        drop(patched);
        tree.ok_or_else(|| anyhow::Error::msg(S::PARSE_ERROR))?
    };
    Ok(match S::POST_TREE {
        Some(fix) => {
            let _p = parse_prof::span(Pass::Walk);
            fix(tree, source)
        }
        None => tree,
    })
}

/// Returns the node count at the first bind step, when one ran in a debug build.
fn run_steps<'t, S: ParseSpec>(
    c: &Compiled<S>,
    f: &mut S::File<'t>,
    cx: &mut FileCx<'t>,
) -> Option<usize> {
    let mut phase = ParsePhase::CollectCaptures;
    let mut bound_len = None;
    for step in S::STEPS {
        let first_bind = phase == ParsePhase::CollectCaptures && step.effect == Effect::Bind;
        if cfg!(debug_assertions) && first_bind {
            phase = phase
                .advance(ParsePhase::FinalizeNodes)
                .and_then(|p| p.advance(ParsePhase::BindRecords))
                .unwrap_or_else(|e| panic!("{}: {e}", S::NAME));
            bound_len = Some(cx.nodes.len());
        }
        {
            let _p = parse_prof::span(step.pass);
            run_op(&step.op, c, f, cx);
        }
        if let Some(len) = bound_len {
            debug_assert_eq!(
                cx.nodes.len(),
                len,
                "{}: a step after a bind step changed the node count",
                S::NAME
            );
        }
    }
    bound_len
}

fn run_op<'t, S: ParseSpec>(
    op: &Op<S::Hook>,
    c: &Compiled<S>,
    f: &mut S::File<'t>,
    cx: &mut FileCx<'t>,
) {
    match *op {
        Op::Query => run_query(c, f, cx),
        Op::Hook(h) => S::hook(h, c, f, cx),
        Op::SortDedupImports(order) => order.sort_dedup(&mut cx.imports),
        Op::DedupNodes => node::dedup_consecutive(&mut cx.nodes),
        Op::FieldReads => {
            if let Some(spec) = &S::FIELD_READS {
                let root = cx.tree.root_node();
                let (source, kinds) = (cx.source, spec.member_kinds);
                calls::extract_field_reads(root, source, &mut cx.nodes, kinds, &spec.names);
            }
        }
        Op::VariantRefs => {
            if let Some(spec) = &S::VARIANT_REFS {
                calls::extract_variant_refs(cx.tree.root_node(), cx.source, &mut cx.nodes, spec);
            }
        }
        Op::FunctionMeta => {
            cx.raw_function_metas =
                S::function_meta(cx.tree.root_node(), cx.source, &cx.nodes, cx.category);
        }
        Op::StampOwners => framework_helpers::stamp_owner_class_by_span(&mut cx.nodes),
        Op::StampOwnerFns => framework_helpers::stamp_owner_fn_by_span(
            S::SCOPE_LANGUAGE.resolve(cx.path),
            &mut cx.nodes,
        ),
        Op::ClearScopedOwners => {
            lexical::clear_scoped_owners(S::SCOPE_LANGUAGE.resolve(cx.path), &mut cx.nodes)
        }
        Op::TxScopes => records::tx_scopes(S::TX, cx),
        Op::EventTopicsRequery => records::event_topics_requery(&c.query, S::EVENT_TOPICS, cx),
        Op::SchemaFieldsRequery => records::schema_fields_requery(&c.query, S::SCHEMA_FIELDS, cx),
    }
}

/// The hot loop. Each capture costs one table load and one `match`; a
/// `DeclSlot` and a `Match` start fresh per match, as the copies' locals do.
fn run_query<'t, S: ParseSpec>(c: &Compiled<S>, f: &mut S::File<'t>, cx: &mut FileCx<'t>) {
    let tree: &'t Tree = cx.tree;
    let source: &'t [u8] = cx.source;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&c.query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let mut d = DeclSlot::default();
        let mut mb: S::Match<'t> = Default::default();
        for cap in m.captures() {
            match c.table[cap.index as usize] {
                Slot::Kind(kind) => d.write_kind(S::KIND_CAPTURE, cap.node, kind),
                Slot::Blind(i) => cx.push_blind(S::BLIND[i].1, &cap.node),
                Slot::Anon => {
                    if let Some(a) = S::ANONYMOUS {
                        cx.emit_anonymous(cap.node, &a);
                    }
                }
                Slot::Lang(role) => S::capture(role, cap.node, &mut d, &mut mb, f, cx),
                Slot::Skip => {}
            }
        }
        S::end_match(d, &mut mb, f, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::Span;

    const fn step(pass: Pass, effect: Effect, op: Op<()>) -> Step<()> {
        Step { pass, effect, op }
    }

    const QUERY: Step<()> = step(Pass::Query, Effect::Nodes, Op::Query);

    /// Rust grammar: each `fn` item becomes a node, each `use` path an import.
    /// `OVERLAP` also claims the kind capture `function.name` as a role.
    struct TestSpec<const OVERLAP: bool>;

    impl<const OVERLAP: bool> LangSpec for TestSpec<OVERLAP> {
        const NAME: &'static str = "driver-test";
        const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
            "function.name" => NodeKind::Function,
        };
    }

    #[derive(Clone, Copy)]
    enum Role {
        FnRoot,
        Import,
    }

    impl<const OVERLAP: bool> ParseSpec for TestSpec<OVERLAP> {
        type Role = Role;
        type Match<'t> = Option<Node<'t>>;
        type File<'t> = ();
        type Hook = ();

        const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
        const STEPS: &'static [Step<()>] = &[
            QUERY,
            step(
                Pass::Build,
                Effect::Neutral,
                Op::SortDedupImports(ImportOrder::SourceName),
            ),
        ];

        fn query() -> anyhow::Result<Query> {
            let q = "(function_item name: (identifier) @function.name) @function
                     (use_declaration argument: (_) @import.source)";
            Ok(Query::new(&tree_sitter_rust::LANGUAGE.into(), q)?)
        }

        fn role(capture: &str) -> Option<Role> {
            match capture {
                "function" => Some(Role::FnRoot),
                "import.source" => Some(Role::Import),
                "function.name" if OVERLAP => Some(Role::FnRoot),
                _ => None,
            }
        }

        fn parse_tree(source: &[u8]) -> Option<Tree> {
            Some(node::rust_tree(source))
        }

        fn capture<'t>(
            r: Role,
            node: Node<'t>,
            d: &mut DeclSlot<'t>,
            import: &mut Option<Node<'t>>,
            _f: &mut (),
            _cx: &mut FileCx<'t>,
        ) {
            match r {
                Role::FnRoot => d.root = Some(node),
                Role::Import => *import = Some(node),
            }
        }

        fn end_match<'t>(
            d: DeclSlot<'t>,
            import: &mut Option<Node<'t>>,
            _f: &mut (),
            cx: &mut FileCx<'t>,
        ) {
            if let (Some(n), Some(kind), Some(root)) = (d.name, d.kind, d.root) {
                let name = n.utf8_text(cx.source).unwrap_or_default().to_string();
                let hashed = &cx.source[root.start_byte()..root.end_byte()];
                cx.nodes
                    .push(node::decl(name, kind, node::span(root), hashed));
            }
            if let Some(Ok(path)) = import.map(|i| i.utf8_text(cx.source)) {
                cx.imports
                    .push(imports::raw(path.to_string(), String::new(), None, None));
            }
        }

        fn hook<'t>(_h: (), _c: &Compiled<Self>, _f: &mut (), _cx: &mut FileCx<'t>) {}

        fn function_meta(
            _: Node<'_>,
            _: &[u8],
            _: &[RawNode],
            _: FileCategory,
        ) -> Vec<RawFunctionMeta> {
            Vec::new()
        }
    }

    fn parse_test(source: &[u8]) -> LocalGraph {
        let c = Compiled::<TestSpec<false>>::new().unwrap_or_else(|e| panic!("{e}"));
        parse(&c, Path::new("src/lib.rs"), source).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn test_compiled_capture_in_two_slots_returns_err() {
        let Err(e) = Compiled::<TestSpec<true>>::new() else {
            panic!("an overlapping capture must fail the constructor")
        };
        assert!(e.to_string().contains("`@function.name`"), "{e}");
    }

    #[test]
    fn test_steps_ok_node_step_after_bind_returns_false() {
        let tx = step(Pass::Build, Effect::Bind, Op::TxScopes);
        let stamp = step(Pass::Build, Effect::Nodes, Op::StampOwners);
        let neutral = step(Pass::Walk, Effect::Neutral, Op::EventTopicsRequery);
        assert!(steps_ok(&[QUERY, stamp, tx, neutral]));
        assert!(!steps_ok(&[QUERY, tx, stamp]));
        assert!(!steps_ok(&[
            QUERY,
            tx,
            step(Pass::Walk, Effect::Nodes, Op::Hook(()))
        ]));
    }

    #[test]
    fn test_steps_ok_tx_not_bind_returns_false() {
        assert!(!steps_ok(&[
            QUERY,
            step(Pass::Build, Effect::Neutral, Op::TxScopes)
        ]));
        assert!(!steps_ok(&[
            QUERY,
            step(Pass::Build, Effect::Neutral, Op::StampOwners)
        ]));
        assert!(!steps_ok::<()>(&[]));
        assert!(!steps_ok(&[QUERY, QUERY]));
        assert!(steps_ok(&[
            QUERY,
            step(Pass::Build, Effect::Bind, Op::Hook(()))
        ]));
    }

    #[test]
    fn test_parse_empty_source_returns_empty_graph() {
        let g = parse_test(b"");
        assert_eq!(g.file_path, Path::new("src/lib.rs"));
        assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
        assert!(g.event_topics.is_none() && g.schema_fields.is_none() && g.tx_scopes.is_none());
        assert!(g.path_literals.is_none() && g.sql_refs.is_none() && g.route_receivers.is_none());
    }

    #[test]
    fn test_parse_imports_only_source_emits_no_nodes() {
        let g = parse_test(b"use b::y;\nuse a::x;\nuse a::x;\n");
        assert!(g.nodes.is_empty());
        let sources: Vec<&str> = g.imports.iter().map(|i| i.source.as_str()).collect();
        assert_eq!(sources, ["a::x", "b::y"]);
    }

    #[test]
    fn test_parse_profiled_spans_stay_disjoint() {
        parse_prof::set_enabled(true);
        parse_prof::reset();
        let g = parse_test(b"fn a() {}\nfn b() { a(); }\n");
        let totals = parse_prof::take();
        parse_prof::set_enabled(false);
        let got: Vec<(&str, Span)> = g.nodes.iter().map(|n| (n.name.as_str(), n.span)).collect();
        assert_eq!(got, [("a", (0, 0, 0, 9)), ("b", (1, 0, 1, 15))]);
        assert!(totals[Pass::Tree as usize] > 0 && totals[Pass::Query as usize] > 0);
    }
}
