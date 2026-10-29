use super::receiver_types::{extract_ruby_calls_and_path_literals, literal_send_target};
use super::spec::RubySpec;
use crate::calls::{NameRefKind, NameRefSpec};
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture,
    Op, ParseSpec, Step, TxRule,
};
use crate::framework_confidence;
use crate::framework_helpers::{
    detect_ast_framework_patterns, enclosing_fn_idx_by_span, push_blind_spot, FrameworkPatternSpec,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    BlindSpot, FrameworkId, LocalGraph, RawFunctionMeta, RawImport, RawNode, RawRoute, RawTxScope,
};
use ecp_core::graph::{FileCategory, NodeKind};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

/// `<expr>.send(<var>, ...)`: pushed only when the target is not a literal
/// symbol or string, so it is a role, not a `BLIND` row.
const BLIND_SEND: (&str, &str) = (
    "rb-send",
    "<expr>.send(<var>, ...) — dynamic method dispatch through a non-literal name; target method resolved at runtime",
);

/// BlindSpot spec for a routing block nested past the Rails walker's cap.
const BLIND_RAILS_ROUTES_DEPTH: (&str, &str) = (
    "rb-rails-routes-depth",
    "routes.draw block nested past the walker's cap \u{2014} routes inside it carry no namespace/scope prefix; read the block directly",
);

/// BlindSpot spec for the module-as-enum pattern.
const BLIND_MODULE_AS_ENUM: (&str, &str) = (
    "ruby-module-as-enum",
    "module with \u{2265}2 constant assignments and no methods \u{2014} Ruby enum imitation pattern; verify before treating as plain Module",
);
/// Receivers a Ruby route DSL sends its verb to. Hanami builds routes on a
/// `router` object and Roda on the request object `r`; `map` and `app` cover
/// Rack::Builder and modular Sinatra. Anything else holding a `get` with a
/// path is an HTTP client.
const ROUTER_RECEIVERS: &[&str] = &["r", "router", "map", "app"];

/// Per upstream `ruby.ts:156-178` `astFrameworkPatterns`.
const RUBY_FRAMEWORKS: &[FrameworkPatternSpec] = &[
    FrameworkPatternSpec {
        framework: "rails",
        reason: "rails-pattern",
        confidence: framework_confidence::RAILS_HINT,
        patterns: &[
            "ApplicationController",
            "ApplicationRecord",
            "ActiveRecord::Base",
            "before_action",
            "after_action",
            "has_many",
            "belongs_to",
            "has_one",
            "validates",
        ],
    },
    FrameworkPatternSpec {
        framework: "sinatra",
        reason: "sinatra-pattern",
        confidence: framework_confidence::SINATRA_HINT,
        patterns: &["Sinatra::Base", "Sinatra::Application"],
    },
];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_ruby::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
/// Walks a `body_statement` (or any block node) and builds a map of
/// `start_row → is_exported` for every `method` / `singleton_method` child.
///
/// Ruby visibility rules: methods are `public` by default.  A bare call to
/// `private`, `protected`, or `public` (an `identifier` node in tree-sitter)
/// changes the visibility for every method that follows it within the same
/// `body_statement`, until the next visibility marker or end-of-scope.
fn build_visibility_map(node: Node<'_>, source: &[u8]) -> HashMap<u32, bool> {
    let mut map = HashMap::new();
    let mut is_public = true;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                if let Ok(text) = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                {
                    match text {
                        "private" | "protected" => is_public = false,
                        "public" => is_public = true,
                        _ => {}
                    }
                }
            }
            "method" | "singleton_method" => {
                map.insert(child.start_position().row as u32, is_public);
                // Recurse into nested body_statements (nested classes/modules).
                let mut c2 = child.walk();
                for sub in child.children(&mut c2) {
                    if sub.kind() == "body_statement" {
                        map.extend(build_visibility_map(sub, source));
                    }
                }
            }
            "class" | "module" => {
                // Recurse into nested class/module body.
                let mut c2 = child.walk();
                for sub in child.children(&mut c2) {
                    if sub.kind() == "body_statement" {
                        map.extend(build_visibility_map(sub, source));
                    }
                }
            }
            _ => {}
        }
    }
    map
}

/// Append a Ruby named binding (`alias` keyword, `alias_method`, or constant
/// alias) as a `RawImport` with `alias = Some(new_name)`. De-duplicates on
/// (imported_name, source) to keep repeated parses idempotent.
fn push_alias_binding(imports: &mut Vec<RawImport>, new_name: &str, source: &str) {
    let exists = imports
        .iter()
        .any(|i| i.imported_name == new_name && i.source == source);
    if !exists {
        imports.push(RawImport {
            alias: Some(new_name.to_string()),
            imported_name: new_name.to_string(),
            source: source.to_string(),
            binding_kind: None,
        });
    }
}

/// Strip a leading `:` (symbol prefix) and a leading `@` (instance-var prefix)
/// from a `def_delegator` / `delegate` argument so the result is a plain
/// receiver / method name suitable for `RawImport.source` composition.
///
/// `:@songs` → `songs`, `:method` → `method`, `:customer` → `customer`.
fn strip_symbol_prefix(s: &str) -> &str {
    let after_colon = s.strip_prefix(':').unwrap_or(s);
    after_colon.strip_prefix('@').unwrap_or(after_colon)
}

/// Returns `true` if `node` is a scalar literal (integer, float, string,
/// symbol, boolean, nil). Method calls (e.g. `"a".freeze`) are NOT scalar.
fn is_scalar_rhs(node: &Node<'_>) -> bool {
    matches!(
        node.kind(),
        "integer"
            | "float"
            | "string"
            | "simple_symbol"
            | "bare_symbol"
            | "delimited_symbol"
            | "true"
            | "false"
            | "nil"
    )
}

/// Walk the tree rooted at `root` and emit one BlindSpot per `module`
/// declaration that matches the enum-imitation heuristic:
/// - ≥ 2 `CAPS = scalar_literal` constant assignments at module top level
/// - 0 `method` / `singleton_method` children in the body
/// - 0 `class` children in the body
///
/// Known false-negative: `X = "a".freeze` is skipped because `.freeze` makes
/// the RHS a `call` node, not a scalar literal. This is conservative by design.
fn detect_module_as_enum(
    root: Node<'_>,
    path: &Path,
    is_test_file: bool,
    out: &mut Vec<BlindSpot>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "module" {
            let body_opt = node.child_by_field_name("body");
            if let Some(body) = body_opt {
                let mut caps_count: u32 = 0;
                let mut has_def = false;
                let mut has_nested_class = false;

                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    match child.kind() {
                        "method" | "singleton_method" => {
                            has_def = true;
                        }
                        "class" => {
                            has_nested_class = true;
                        }
                        "assignment" => {
                            // lhs must be a constant (uppercase identifier)
                            let lhs_ok = child
                                .child_by_field_name("left")
                                .is_some_and(|l| l.kind() == "constant");
                            // rhs must be a scalar literal
                            let rhs_ok = child
                                .child_by_field_name("right")
                                .is_some_and(|r| is_scalar_rhs(&r));
                            if lhs_ok && rhs_ok {
                                caps_count += 1;
                            }
                        }
                        _ => {}
                    }
                }

                if caps_count >= 2 && !has_def && !has_nested_class {
                    push_blind_spot(out, BLIND_MODULE_AS_ENUM, &node, path, is_test_file);
                }
            }
            // Do NOT recurse into module children — nested modules are
            // independent and will be visited via the stack below.
        }

        // Push all named children for DFS traversal.
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Walk the Ruby AST for `call` nodes whose method is `transaction` and that
/// have a `do_block` child (the `Model.transaction do ... end` / Sequel
/// `DB.transaction do ... end` idiom). Emits one `RawTxScope` per enclosing
/// function — multiple `transaction do` blocks in the same function are
/// deduplicated to one scope.
///
/// Conservative match: receiver is not validated. `User.transaction do`,
/// `ActiveRecord::Base.transaction do`, and even `obj.transaction do` all
/// match. False positives from non-ActiveRecord `transaction` methods are
/// acceptable as v1 noise per the design spec.
///
/// `transaction(some_proc)` without a `do_block` (the rare proc-form) is
/// intentionally NOT matched — no `do_block` child → no scope emitted.
fn collect_ruby_transaction_scopes(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
) -> Option<Box<[RawTxScope]>> {
    // Distinct enclosing function indices — HashSet matches Go / Dart detectors
    // (avoid the O(K²) Vec::contains pattern flagged by /simplify review).
    let mut seen_fn_idxs: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut stack: Vec<tree_sitter::Node<'_>> = vec![root];

    while let Some(n) = stack.pop() {
        if n.kind() == "call" && is_transaction_do_block_call(n, source) {
            // Shared helper: smallest-area enclosing fn (was first-match before
            // FU-034 — nested-fn scenarios resolved to the OUTER fn).
            let row = n.start_position().row as u32;
            let col = n.start_position().column as u32;
            if let Some(fn_idx) = enclosing_fn_idx_by_span(nodes, row, col) {
                seen_fn_idxs.insert(fn_idx);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }

    if seen_fn_idxs.is_empty() {
        return None;
    }
    let scopes: Box<[RawTxScope]> = seen_fn_idxs
        .into_iter()
        .map(|idx| RawTxScope::new(idx, FrameworkId::RubyActiveRecordTransaction))
        .collect();
    Some(scopes)
}

/// True iff `call_node` has method name `"transaction"` AND a `do_block`
/// direct child (the block-form idiom). The `do_block` is not a named field
/// in tree-sitter-ruby; it's an unnamed child that follows the argument list.
///
/// `transaction(some_proc)` without a `do_block` returns false — the proc-form
/// is intentionally excluded from TransactionScope emission.
#[inline]
fn is_transaction_do_block_call(call_node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let method_ok = call_node
        .child_by_field_name("method")
        .and_then(|m| std::str::from_utf8(&source[m.start_byte()..m.end_byte()]).ok())
        .is_some_and(|s| s == "transaction");
    if !method_ok {
        return false;
    }
    let mut c = call_node.walk();
    let has_do_block = call_node
        .children(&mut c)
        .any(|child| child.kind() == "do_block");
    has_do_block
}

/// The smallest-span class or module RawNode whose rows contain `line`, by
/// its index in `nodes`. Shared by mixin application and the delegator
/// Forwardable-scope check.
fn enclosing_class_idx(nodes: &[RawNode], line: u32) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_span: u32 = u32::MAX;
    for (i, n) in nodes.iter().enumerate() {
        if n.kind != NodeKind::Class && n.kind != NodeKind::Trait {
            continue;
        }
        if n.span.0 <= line && n.span.2 >= line {
            let width = n.span.2 - n.span.0;
            if width < best_span {
                best_span = width;
                best = Some(i);
            }
        }
    }
    best
}

pub struct RubyProvider {
    compiled: Compiled<RubySpec>,
}

impl RubyProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for RubyProvider {
    fn name(&self) -> &'static str {
        "ruby"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Ruby handles itself. The root-kind captures (`class`,
/// `module`, `method`, `const`) set root and kind; `Name` sets the name.
#[derive(Clone, Copy)]
pub enum RubyRole {
    Name,
    Heritage,
    ImportName,
    RouteMethod,
    RoutePath,
    Route,
    AttrArgs,
    MixinModule,
    AliasNew,
    AliasOld,
    AliasMethodArgs,
    ConstAliasNew,
    ConstAliasSource,
    DelegatorMethod,
    DelegatorArgs,
    Send,
}

#[derive(Clone, Copy)]
pub enum RubyHook {
    Visibility,
    ModuleAsEnum,
    Bindings,
    Calls,
    Frameworks,
    Routes,
}

#[derive(Default)]
pub struct RubyMatch<'t> {
    heritage: Vec<String>,
    import_name: Option<Node<'t>>,
    route_method: Option<Node<'t>>,
    route_path: Option<Node<'t>>,
    route_root: Option<Node<'t>>,
    attr_args_node: Option<Node<'t>>,
    mixin_module_node: Option<Node<'t>>,
    alias_new_node: Option<Node<'t>>,
    alias_old_node: Option<Node<'t>>,
    alias_method_args: Option<Node<'t>>,
    const_alias_new_node: Option<Node<'t>>,
    const_alias_source_node: Option<Node<'t>>,
    delegator_method_node: Option<Node<'t>>,
    delegator_args_node: Option<Node<'t>>,
}

#[derive(Default)]
pub struct RubyFile {
    /// Method start row → is_exported, from the visibility markers in
    /// class bodies; built before the query.
    visibility_map: HashMap<u32, bool>,
    /// Mixin module additions, applied after primary node emission. Each
    /// entry is (module_name, call_line) — we attach to the smallest
    /// enclosing class node by span containment. Document-order traversal
    /// of tree-sitter matches preserves source ordering (M1 before M2).
    pending_mixins: Vec<(String, u32)>,
    /// Pending delegator emissions: (target, method, line). Applied after
    /// the match loop so we can cross-check against `pending_mixins` to
    /// require the enclosing class to `extend`/`include Forwardable`.
    /// Without a Forwardable mixin we fall back to "low-confidence" emit
    /// (still pushed, documented false-positive on user-defined methods
    /// named `def_delegator` / `delegate`).
    pending_delegators: Vec<(String, String, u32)>,
}

const fn step(pass: Pass, effect: Effect, op: Op<RubyHook>) -> Step<RubyHook> {
    Step { pass, effect, op }
}

impl ParseSpec for RubySpec {
    type Role = RubyRole;
    type Match<'t> = RubyMatch<'t>;
    type File<'t> = RubyFile;
    type Hook = RubyHook;

    const KIND_CAPTURE: KindCapture = KindCapture::RootOverwrite;
    /// Anonymous block / do_block / lambda attached to a call. Emit an
    /// `<anonymous>` Function node only when the block body holds a call, so
    /// attach_to_enclosing can host those calls instead of dropping them;
    /// empty blocks (e.g. `arr.map { |x| x * 2 }`) stay out of the graph. The
    /// same block can appear in several matches, so the span dedupes it.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.eval",
            (
                "rb-eval",
                "eval(<expr>) — runtime Ruby code execution; argument is not statically determinable as a callable",
            ),
        ),
        (
            "blind.instance_eval",
            (
                "rb-instance-eval",
                "<expr>.instance_eval { ... } — runtime code execution in receiver context; block contents bound to receiver at call time",
            ),
        ),
    ];
    /// Ruby has no field-read capture (`obj.x` is a call); the walk records
    /// the constants a method reads and its `method(:name)` references.
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &[],
        names: NameRefSpec {
            ident_kinds: &["constant"],
            kind: NameRefKind::Constant,
            symbol_method_refs: true,
            ..NameRefSpec::EMPTY
        },
    });
    /// Block-form transaction detection: walk AST for `call` nodes with
    /// method=`transaction` + `do_block` child. Must run after `nodes` is
    /// fully populated (stamp_owner_class_by_span finalises spans) so
    /// span-containment lookup for the enclosing function is accurate.
    const TX: TxRule = TxRule::Tree(collect_ruby_transaction_scopes);
    const STEPS: &'static [Step<RubyHook>] = &[
        step(Pass::Walk, Effect::Neutral, Op::Hook(RubyHook::Visibility)),
        step(Pass::Query, Effect::Nodes, Op::Query),
        step(
            Pass::Walk,
            Effect::Neutral,
            Op::Hook(RubyHook::ModuleAsEnum),
        ),
        step(Pass::Build, Effect::Nodes, Op::Hook(RubyHook::Bindings)),
        step(Pass::Calls, Effect::Nodes, Op::Hook(RubyHook::Calls)),
        step(Pass::Walk, Effect::Nodes, Op::FieldReads),
        step(Pass::Walk, Effect::Neutral, Op::Hook(RubyHook::Frameworks)),
        step(Pass::Build, Effect::Neutral, Op::Hook(RubyHook::Routes)),
        step(Pass::Walk, Effect::Neutral, Op::FunctionMeta),
        step(Pass::Build, Effect::Nodes, Op::StampOwners),
        step(Pass::Walk, Effect::Bind, Op::TxScopes),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_ruby::LANGUAGE.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<RubyRole> {
        Some(match capture {
            "name" => RubyRole::Name,
            "heritage" => RubyRole::Heritage,
            "import.name" => RubyRole::ImportName,
            "route.method" => RubyRole::RouteMethod,
            "route.path" => RubyRole::RoutePath,
            "route" => RubyRole::Route,
            "attr_args" => RubyRole::AttrArgs,
            "mixin_module" => RubyRole::MixinModule,
            "alias.new" => RubyRole::AliasNew,
            "alias.old" => RubyRole::AliasOld,
            "alias_method.args" => RubyRole::AliasMethodArgs,
            "const_alias.new" => RubyRole::ConstAliasNew,
            "const_alias.source" => RubyRole::ConstAliasSource,
            "delegator_method" => RubyRole::DelegatorMethod,
            "delegator_args" => RubyRole::DelegatorArgs,
            "blind.send" => RubyRole::Send,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: RubyRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut RubyMatch<'t>,
        _f: &mut RubyFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            RubyRole::Name => d.name = Some(node),
            RubyRole::Heritage => {
                if let Ok(h_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.heritage.push(h_str.to_string());
                }
            }
            RubyRole::ImportName => m.import_name = Some(node),
            RubyRole::RouteMethod => m.route_method = Some(node),
            RubyRole::RoutePath => m.route_path = Some(node),
            RubyRole::Route => m.route_root = Some(node),
            RubyRole::AttrArgs => m.attr_args_node = Some(node),
            RubyRole::MixinModule => m.mixin_module_node = Some(node),
            RubyRole::AliasNew => m.alias_new_node = Some(node),
            RubyRole::AliasOld => m.alias_old_node = Some(node),
            RubyRole::AliasMethodArgs => m.alias_method_args = Some(node),
            RubyRole::ConstAliasNew => m.const_alias_new_node = Some(node),
            RubyRole::ConstAliasSource => m.const_alias_source_node = Some(node),
            RubyRole::DelegatorMethod => m.delegator_method_node = Some(node),
            RubyRole::DelegatorArgs => m.delegator_args_node = Some(node),
            RubyRole::Send => {
                if literal_send_target(node, source).is_none() {
                    cx.push_blind(BLIND_SEND, &node);
                }
            }
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut RubyMatch<'t>,
        f: &mut RubyFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let RubyMatch {
            heritage,
            import_name,
            route_method,
            route_path,
            route_root,
            attr_args_node,
            mixin_module_node,
            alias_new_node,
            alias_old_node,
            alias_method_args,
            const_alias_new_node,
            const_alias_source_node,
            delegator_method_node,
            delegator_args_node,
        } = std::mem::take(m);

        if let (Some(name_node), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
            {
                let start = root.start_position();
                // Methods: respect visibility markers. Classes/modules are always exported.
                let is_exported = if k == NodeKind::Method {
                    *f.visibility_map.get(&(start.row as u32)).unwrap_or(&true)
                } else {
                    // Classes and Traits (modules) are always exported.
                    true
                };
                // Ruby's constructor convention is `initialize`; the spec
                // table maps it as Method, so promote here.
                let k = if k == NodeKind::Method && name_str == "initialize" {
                    NodeKind::Constructor
                } else {
                    k
                };
                cx.nodes.push(RawNode {
                    is_exported,
                    heritage,
                    ..node::decl(
                        name_str.to_string(),
                        k,
                        node::span(root),
                        &source[root.start_byte()..root.end_byte()],
                    )
                });
            }
        }

        if let Some(i_node) = import_name {
            if let Ok(name_str) =
                std::str::from_utf8(&source[i_node.start_byte()..i_node.end_byte()])
            {
                cx.imports.push(driver::imports::raw(
                    name_str.to_string(),
                    name_str.to_string(),
                    None,
                    None,
                ));
            }
        }

        // A route DSL sends its verb to the mapper: Rails `get "/x"` and
        // Sinatra `get "/x" do` have no receiver; Hanami `router.get` and
        // Roda `r.get` send it to a router object. Any other receiver
        // means an HTTP client — `connection.get "/openapi/v1/tag"` names
        // an endpoint this application calls, not one it serves. Measured
        // 2026-09-02 on a Rails 8.1 app: ten such calls under
        // `app/services/client/**` reached `ecp routes` before this check.
        let declares_route = route_root.is_some_and(|n| match n.child_by_field_name("receiver") {
            None => true,
            Some(r) => r
                .utf8_text(source)
                .is_ok_and(|t| ROUTER_RECEIVERS.contains(&t)),
        });

        if let (true, Some(r_method), Some(r_path), Some(r_root)) =
            (declares_route, route_method, route_path, route_root)
        {
            if let (Ok(method_str), Ok(path_str)) = (
                std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
            ) {
                let start = r_root.start_position();
                let end = r_root.end_position();
                // Sinatra `get "/x" do ... end` and Roda `r.get "x" do`
                // serve the route with the block written in place.
                let handler = r_root.child_by_field_name("block").map(|block| {
                    ecp_core::contracts::raw_records::RawEndpoint::Unresolved(
                        ecp_core::contracts::raw_records::Unbound::Inline(
                            crate::framework_helpers::node_span(&block),
                        ),
                    )
                });
                cx.routes.push(RawRoute {
                    method: method_str.to_string(),
                    path: path_str.to_string(),
                    handler,
                    span: (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    ),
                    scope: None,
                });
            }
        }

        // attr_reader / attr_writer / attr_accessor → emit one Property per symbol.
        // is_exported=true unconditionally; private-block detection is punted for MVP
        // because tree-sitter parses `private` as just another bareword call without
        // a structural block — distinguishing "below a private call" from "above"
        // requires a stateful AST sweep that's out of scope for this pass.
        if let Some(args) = attr_args_node {
            let mut walker = args.walk();
            for child in args.named_children(&mut walker) {
                if child.kind() != "simple_symbol" {
                    continue;
                }
                let Ok(sym_text) =
                    std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                else {
                    continue;
                };
                let prop_name = sym_text.strip_prefix(':').unwrap_or(sym_text);
                if prop_name.is_empty() {
                    continue;
                }
                cx.nodes.push(RawNode {
                    is_exported: true,
                    ..node::decl(
                        prop_name.to_string(),
                        NodeKind::Property,
                        node::span(child),
                        &source[child.start_byte()..child.end_byte()],
                    )
                });
            }
        }

        // include / extend → queue the module name for attachment to the
        // enclosing class's heritage after all class nodes are emitted.
        if let Some(mm) = mixin_module_node {
            if let Ok(mm_str) = std::str::from_utf8(&source[mm.start_byte()..mm.end_byte()]) {
                let line = mm.start_position().row as u32;
                f.pending_mixins.push((mm_str.to_string(), line));
            }
        }

        // `alias new old` keyword → named binding.
        if let (Some(n), Some(o)) = (alias_new_node, alias_old_node) {
            if let (Ok(new_name), Ok(old_name)) = (
                std::str::from_utf8(&source[n.start_byte()..n.end_byte()]),
                std::str::from_utf8(&source[o.start_byte()..o.end_byte()]),
            ) {
                push_alias_binding(&mut cx.imports, new_name, old_name);
            }
        }

        // `alias_method :new, :old` metaprogramming → named binding.
        // Walk the argument_list and grab the first two simple_symbols.
        if let Some(args) = alias_method_args {
            let mut walker = args.walk();
            let symbols: Vec<&str> = args
                .named_children(&mut walker)
                .filter(|c| c.kind() == "simple_symbol")
                .filter_map(|c| std::str::from_utf8(&source[c.start_byte()..c.end_byte()]).ok())
                .map(|s| s.strip_prefix(':').unwrap_or(s))
                .filter(|s| !s.is_empty())
                .take(2)
                .collect();
            if let [new_name, old_name] = symbols.as_slice() {
                push_alias_binding(&mut cx.imports, new_name, old_name);
            }
        }

        // `MyConst = OtherModule::Const` → named binding.
        // lhs is guaranteed `(constant)` by the query, so the assignment
        // is a constant alias (not a local variable assignment).
        if let (Some(lhs), Some(rhs)) = (const_alias_new_node, const_alias_source_node) {
            if let (Ok(new_name), Ok(source_path)) = (
                std::str::from_utf8(&source[lhs.start_byte()..lhs.end_byte()]),
                std::str::from_utf8(&source[rhs.start_byte()..rhs.end_byte()]),
            ) {
                push_alias_binding(&mut cx.imports, new_name, source_path);
            }
        }

        // `def_delegator :target, :method` / `def_delegators :target, :m1, ...` /
        // `delegate :m1, :m2, to: :target` — parse argument list shape per
        // method name. Each delegated method is queued for emission; the
        // Forwardable-mixin check runs after the match loop because the
        // enclosing class span and `pending_mixins` aren't both finalised
        // until then.
        if let (Some(method_node), Some(args)) = (delegator_method_node, delegator_args_node) {
            let method_name =
                std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()])
                    .unwrap_or("");
            let call_line = method_node.start_position().row as u32;
            let mut walker = args.walk();
            let children: Vec<Node<'_>> = args.named_children(&mut walker).collect();

            match method_name {
                "def_delegator" | "def_delegators" => {
                    // First simple_symbol = target; rest = delegated methods.
                    let mut symbols = children
                        .iter()
                        .filter(|c| c.kind() == "simple_symbol")
                        .filter_map(|c| {
                            std::str::from_utf8(&source[c.start_byte()..c.end_byte()]).ok()
                        });
                    if let Some(target_raw) = symbols.next() {
                        let target = strip_symbol_prefix(target_raw).to_string();
                        if !target.is_empty() {
                            for m_raw in symbols {
                                let m = strip_symbol_prefix(m_raw);
                                if !m.is_empty() {
                                    f.pending_delegators.push((
                                        target.clone(),
                                        m.to_string(),
                                        call_line,
                                    ));
                                }
                            }
                        }
                    }
                }
                "delegate" => {
                    // simple_symbol* (methods), then `pair` with key=`to`,
                    // value=simple_symbol (target). Walk in order, collect
                    // method names until the `to:` pair is hit.
                    let mut methods: Vec<String> = Vec::new();
                    let mut target: Option<String> = None;
                    for child in &children {
                        match child.kind() {
                            "simple_symbol" => {
                                if let Ok(s) = std::str::from_utf8(
                                    &source[child.start_byte()..child.end_byte()],
                                ) {
                                    let stripped = strip_symbol_prefix(s);
                                    if !stripped.is_empty() {
                                        methods.push(stripped.to_string());
                                    }
                                }
                            }
                            "pair" => {
                                // Look for key=hash_key_symbol with text "to"
                                // and value=simple_symbol/string.
                                let key = child.child_by_field_name("key");
                                let value = child.child_by_field_name("value");
                                let key_text = key.and_then(|k| {
                                    std::str::from_utf8(&source[k.start_byte()..k.end_byte()]).ok()
                                });
                                if key_text == Some("to") {
                                    if let Some(v) = value {
                                        if let Ok(t) = std::str::from_utf8(
                                            &source[v.start_byte()..v.end_byte()],
                                        ) {
                                            target = Some(strip_symbol_prefix(t).to_string());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(t) = target {
                        if !t.is_empty() {
                            for m in methods {
                                f.pending_delegators.push((t.clone(), m, call_line));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn hook<'t>(h: RubyHook, _c: &Compiled<Self>, f: &mut RubyFile, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            RubyHook::Visibility => {
                // Build method-row → is_exported map from visibility markers in class bodies.
                f.visibility_map = build_visibility_map(root, source);
            }
            RubyHook::ModuleAsEnum => {
                // Detect module-as-enum imitation: walk AST for qualifying module nodes
                // and emit BlindSpotRecord. Done after the query loop so it is
                // independent of tree-sitter query captures.
                detect_module_as_enum(root, cx.path, cx.is_test, &mut cx.blind_spots);
            }
            RubyHook::Bindings => {
                let nodes = &mut cx.nodes;
                // Apply mixins: for each (module, line), find the smallest enclosing
                // class RawNode by span containment and append the module to its
                // heritage. Mixins outside any class are dropped (matches Ruby
                // semantics — bare top-level `include` is rare and out of scope).
                for (module_name, line) in &f.pending_mixins {
                    if let Some(i) = enclosing_class_idx(nodes, *line) {
                        nodes[i].heritage.push(module_name.clone());
                    }
                }

                // Apply delegators: `def_delegator/s` / `delegate` add a method
                // binding on the enclosing class. We require `extend Forwardable`
                // (or `include Forwardable`) in the same enclosing class as a
                // sanity check; without it we still emit (Option-A fallback per
                // spec §4) at the cost of a known false positive when the user
                // defines their own method named `def_delegator`.
                //
                // The emitted RawImport mirrors the alias-keyword shape:
                // `{ alias: Some(method), imported_name: method, source: "target.method" }`
                // so downstream rename / resolution code reuses the existing path.
                for (target, method, line) in std::mem::take(&mut f.pending_delegators) {
                    let enclosing = enclosing_class_idx(nodes, line);
                    let _has_forwardable = enclosing.is_some_and(|idx| {
                        let span = nodes[idx].span;
                        f.pending_mixins
                            .iter()
                            .any(|(m, ml)| m == "Forwardable" && *ml >= span.0 && *ml <= span.2)
                    });
                    // Emit regardless of `_has_forwardable` — Option-A low-confidence
                    // fallback per docs/specs/2026-05-16-ruby-receiver-aware-resolver.md.
                    // The flag is retained as a future hook for telemetry / a
                    // BindingKind discriminant when one becomes available.
                    let source_path = format!("{target}.{method}");
                    push_alias_binding(&mut cx.imports, &method, &source_path);
                    // Also materialise the delegator as a Method RawNode so cross-file
                    // mixin chains resolve via Tier 2.75 (HeritageScoped): without a
                    // real node in the originating module the alias is only visible
                    // to same-file lookups, leaving `class Bar; include Foo; end`
                    // callers unable to reach `Foo`'s delegated methods.
                    nodes.push(RawNode {
                        is_exported: true,
                        content_hash: 0,
                        ..node::decl(method, NodeKind::Method, (line, 0, line, 0), &[])
                    });
                }
            }
            RubyHook::Calls => {
                // Extract call sites with receiver-type binding.
                // Handles self.method → EnclosingClass.method, Constant.method → Constant.method.
                // Same DFS also collects path-shaped string literals.
                let (raw_path_literals, raw_sql_refs) =
                    extract_ruby_calls_and_path_literals(root, source, &mut cx.nodes);
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            RubyHook::Frameworks => {
                cx.framework_refs = detect_ast_framework_patterns(source, RUBY_FRAMEWORKS);
            }
            RubyHook::Routes => {
                // Path-shape filter — drop generic route captures whose first
                // string arg doesn't look like an HTTP route. Same rationale as
                // the JS/TS/Python parsers; spec:
                // `docs/superpowers/specs/2026-05-17-route-precision-design.md`.
                cx.routes
                    .retain_mut(|r| match crate::route_detector::clean_route_path(&r.path) {
                        Some(clean) => {
                            r.path = clean;
                            true
                        }
                        None => false,
                    });

                // Rails states most of its routing surface through `resources`, which
                // the generic matcher above cannot see because it names a symbol
                // rather than a path. Expand the `routes.draw` DSL, and drop the
                // generic captures inside those blocks — the expansion reports the
                // same lines with the enclosing `namespace` / `scope` prefix applied.
                let rails = crate::ruby::rails_routes::extract(root, source);
                if !rails.draw_rows.is_empty() {
                    cx.routes.retain(|r| !rails.covers(r.span.0));
                    cx.routes.extend(rails.routes);
                    // A block past the walker's nesting cap keeps its generic captures
                    // (`covers` excludes it) and is reported as a place the graph
                    // stopped looking, the same way `send` and `eval` are.
                    for span in rails.truncated {
                        cx.blind_spots.push(BlindSpot {
                            kind: BLIND_RAILS_ROUTES_DEPTH.0.to_string(),
                            file_path: cx.path.to_path_buf(),
                            span,
                            hint: BLIND_RAILS_ROUTES_DEPTH.1.to_string(),
                            is_test: cx.is_test,
                        });
                    }
                }
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::ruby::extract(root, source, nodes, category)
    }
}
