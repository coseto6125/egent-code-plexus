//! The shared parse driver: a tree parse, the main query loop and an
//! ordered step list per file; a language supplies the rest as an
//! `impl ParseSpec`. A hook may pre-scan the tree and a `*Requery` op runs
//! the query again, so one file can cost more than one query pass.
//! A spec must reproduce its old `LocalGraph` byte for byte, record order
//! included: the parse cache serves graphs the old parser wrote.

mod cx;
pub mod imports;
pub mod node;
mod records;
mod tap;

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
use tap::{TapMatch, TapRange, TapTable};
use tree_sitter::{Node, Query, QueryCursor, Tree};

/// One language's half of a parse. Every hook and per-match method is
/// monomorphised into `parse::<Self>`, so the match loop makes no `dyn` call.
pub trait ParseSpec: LangSpec + Sized + 'static {
    /// A capture the spec handles itself, conditional blind spots included.
    type Role: Copy;
    /// Per-match buffers; a fresh default per match, so no state carries
    /// from one match into the next.
    type Match<'t>: Default;
    /// Per-file side tables that the hooks and per-match methods share.
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
/// Built only through [`Step::fixed`] and [`Step::hook`], so a fixed op
/// always carries its own effect.
pub struct Step<H> {
    pass: Pass,
    effect: Effect,
    op: Op<H>,
}

impl<H> Step<H> {
    /// A fixed op; its effect is the op's own.
    ///
    /// # Panics
    /// `op` is a hook, which declares its own effect through [`Step::hook`].
    /// In a `const` item the panic is a compile error.
    pub const fn fixed(pass: Pass, op: Op<H>) -> Self {
        let effect = op
            .fixed_effect()
            .expect("a hook declares its own effect: use Step::hook");
        Self { pass, effect, op }
    }

    pub const fn hook(pass: Pass, effect: Effect, hook: H) -> Self {
        Self {
            pass,
            effect,
            op: Op::Hook(hook),
        }
    }
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
    /// Builds the records from the rows the query pass tapped.
    SchemaFields,
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
            | Op::SchemaFields
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

/// No `Nodes` step after a `Bind` step, exactly one step runs the query,
/// the tap step follows it, and
/// at most one step builds schema fields: the tap step drains the tapped
/// rows, so a second schema step would overwrite the records.
/// A hook's real writes are not checked here; the debug node-count
/// assertion and the dump catch them.
pub const fn steps_ok<H>(steps: &[Step<H>]) -> bool {
    let mut bound = false;
    let mut queries = 0;
    let mut schemas = false;
    let mut i = 0;
    while i < steps.len() {
        let step = &steps[i];
        match step.effect {
            Effect::Nodes if bound => return false,
            Effect::Bind => bound = true,
            _ => {}
        }
        match step.op {
            Op::Query => queries += 1,
            Op::SchemaFields if queries == 0 => return false,
            _ => {}
        }
        if matches!(step.op, Op::SchemaFields | Op::SchemaFieldsRequery) {
            if schemas {
                return false;
            }
            schemas = true;
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

/// Whether a spec's steps build its schema fields from the tap.
const fn taps_schemas<H>(steps: &[Step<H>]) -> bool {
    let mut i = 0;
    while i < steps.len() {
        if matches!(steps[i].op, Op::SchemaFields) {
            return true;
        }
        i += 1;
    }
    false
}

#[derive(Clone, Copy)]
enum Slot<R> {
    Kind(NodeKind),
    Blind(usize),
    Anon,
    Lang(R),
    Skip,
}

/// One capture index: its dispatch slot, and the tap targets it also feeds.
#[derive(Clone, Copy)]
struct Entry<R> {
    slot: Slot<R>,
    tap: TapRange,
}

pub struct Compiled<S: ParseSpec> {
    query: Query,
    table: Box<[Entry<S::Role>]>,
    taps: TapTable,
}

impl<S: ParseSpec> Compiled<S> {
    /// # Errors
    /// The query does not compile, or one capture name resolves to two of
    /// kind, blind, anonymous and role: the slot table has no priority rule.
    pub fn new() -> anyhow::Result<Self> {
        const {
            assert!(
                steps_ok(S::STEPS),
                "STEPS: a node-changing step follows a bind step, the query step is not exactly one, a tap step precedes the query, or two steps build schema fields"
            );
            assert!(
                params_ok::<S>(),
                "STEPS lists FieldReads or VariantRefs with no spec"
            );
            assert!(
                !taps_schemas(S::STEPS) || S::SCHEMA_FIELDS.len() <= tap::MAX_CONFIGS,
                "SCHEMA_FIELDS exceeds the tap's slot table"
            );
        }
        let query = S::query()?;
        // A spec that re-queries its schema fields gets no targets, so its
        // matches pay nothing in the main loop.
        let tapped = const { taps_schemas(S::STEPS) };
        let schemas: &[SchemaFieldConfig] = if tapped { S::SCHEMA_FIELDS } else { &[] };
        let (taps, ranges) = TapTable::new(&query, schemas);
        let table = query
            .capture_names()
            .iter()
            .zip(ranges)
            .map(|(name, tap)| {
                Ok(Entry {
                    slot: slot_for::<S>(name)?,
                    tap,
                })
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Self { query, table, taps })
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn role_at(&self, capture_index: u32) -> Option<S::Role> {
        match self.table.get(capture_index as usize) {
            Some(Entry {
                slot: Slot::Lang(r),
                ..
            }) => Some(*r),
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
/// assembly.
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
    run_steps(c, &mut file, &mut cx);
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

/// A debug build checks that no step after the first bind step changes
/// the node count.
fn run_steps<'t, S: ParseSpec>(c: &Compiled<S>, f: &mut S::File<'t>, cx: &mut FileCx<'t>) {
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
        Op::SchemaFields => tap::schema_fields(S::SCHEMA_FIELDS, cx),
        Op::SchemaFieldsRequery => records::schema_fields_requery(&c.query, S::SCHEMA_FIELDS, cx),
    }
}

/// The hot loop. Each capture costs one table load, one tap-range test and
/// one `match`; a `DeclSlot` and a `Match` start fresh per match, so no
/// capture carries into the next match. The tap closes a match before `end_match`, so a spec's
/// early return cannot drop a tap row.
fn run_query<'t, S: ParseSpec>(c: &Compiled<S>, f: &mut S::File<'t>, cx: &mut FileCx<'t>) {
    let tree: &'t Tree = cx.tree;
    let source: &'t [u8] = cx.source;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&c.query, tree.root_node(), source);
    let mut tap = TapMatch::default();
    while let Some(m) = matches.next() {
        let mut d = DeclSlot::default();
        let mut mb: S::Match<'t> = Default::default();
        for cap in m.captures() {
            let entry = &c.table[cap.index as usize];
            if !entry.tap.is_empty() {
                tap.write(&c.taps, entry.tap, cap.node);
            }
            match entry.slot {
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
        tap.close(&c.taps, m.captures(), &mut cx.tap_rows);
        S::end_match(d, &mut mb, f, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::analyzer::types::Span;

    const QUERY: Step<()> = Step::fixed(Pass::Query, Op::Query);

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
            Step::fixed(Pass::Build, Op::SortDedupImports(ImportOrder::SourceName)),
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
        let tx = Step::fixed(Pass::Build, Op::TxScopes);
        let stamp = Step::fixed(Pass::Build, Op::StampOwners);
        let neutral = Step::fixed(Pass::Walk, Op::EventTopicsRequery);
        assert!(steps_ok(&[QUERY, stamp, tx, neutral]));
        assert!(!steps_ok(&[QUERY, tx, stamp]));
        assert!(!steps_ok(&[
            QUERY,
            tx,
            Step::hook(Pass::Walk, Effect::Nodes, ())
        ]));
    }

    #[test]
    fn test_steps_ok_query_count_and_tap_order_returns_expected() {
        assert!(!steps_ok::<()>(&[]));
        assert!(!steps_ok(&[QUERY, QUERY]));
        let schemas = Step::fixed(Pass::Walk, Op::SchemaFields);
        assert!(!steps_ok(&[schemas, QUERY]));
        assert!(steps_ok(&[QUERY, schemas]));
        assert!(steps_ok(&[
            QUERY,
            Step::hook(Pass::Build, Effect::Bind, ())
        ]));
    }

    #[test]
    fn test_steps_ok_second_schema_fields_returns_false() {
        let schemas = Step::fixed(Pass::Walk, Op::SchemaFields);
        assert!(!steps_ok(&[QUERY, schemas, schemas]));
    }

    #[test]
    fn test_steps_ok_schema_fields_with_requery_returns_false() {
        let schemas = Step::fixed(Pass::Walk, Op::SchemaFields);
        let requery = Step::fixed(Pass::Walk, Op::SchemaFieldsRequery);
        assert!(steps_ok(&[QUERY, requery]));
        assert!(!steps_ok(&[QUERY, schemas, requery]));
        assert!(!steps_ok(&[QUERY, requery, schemas]));
    }

    #[test]
    #[should_panic(expected = "Step::hook")]
    fn test_step_fixed_hook_op_panics() {
        let _ = Step::fixed(Pass::Walk, Op::Hook(()));
    }

    #[test]
    fn test_step_fixed_takes_op_effect() {
        let tx = Step::<()>::fixed(Pass::Build, Op::TxScopes);
        assert!(tx.effect == Effect::Bind);
        let stamp = Step::<()>::fixed(Pass::Build, Op::StampOwners);
        assert!(stamp.effect == Effect::Nodes);
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

    /// The profiled form of this parse lives in `tests/parse_prof_coverage.rs`:
    /// the profiler flag is process-wide, and that binary holds its only toggle.
    #[test]
    fn test_parse_two_fns_emits_decl_spans() {
        let g = parse_test(b"fn a() {}\nfn b() { a(); }\n");
        let got: Vec<(&str, Span)> = g.nodes.iter().map(|n| (n.name.as_str(), n.span)).collect();
        assert_eq!(got, [("a", (0, 0, 0, 9)), ("b", (1, 0, 1, 15))]);
    }
}
