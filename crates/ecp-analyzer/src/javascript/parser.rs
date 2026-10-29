use super::receiver_types::extract_js_calls_and_path_literals;
use super::spec::JavaScriptSpec;
use crate::calls::{NameRefKind, NameRefSpec};
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture,
    Op, ParseSpec, Step,
};
use crate::event_topic::{EventTopicConfig, KAFKA_NODE, RABBITMQ_JS, REDIS_JS, SQS_JS};
use crate::framework_confidence;
use crate::framework_helpers::{
    has_import_from, js_ts_first_arg_is_literal_string, js_ts_reexport_kind,
    mark_js_ts_import_bindings, node_span, stamp_js_ts_exports, SCALAR_VALUE_KINDS,
};
use crate::indirect_dispatch::{collect_js_param_names, detect_js_ts_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    LocalGraph, RawFrameworkRef, RawFunctionMeta, RawNode, RawRoute, RawRouteReceiver, Span,
};
use ecp_core::graph::{FileCategory, NodeKind};
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

/// Blind-spot kind/hint pairs: the first two are unconditional (`BLIND`),
/// the last three are pushed by a role only when its predicate holds.
const BLIND_SPEC: [(&str, &str); 5] = [
    (
        "js-eval",
        "eval(arg) — runtime JS execution; argument expression is not statically determinable as a callee",
    ),
    (
        "js-function-ctor",
        "Function(arg) / new Function(arg) — runtime function compilation; body source is not statically determinable",
    ),
    (
        "js-dynamic-import",
        "import(<expr>) with non-literal specifier — dynamic module loading; target module depends on runtime value",
    ),
    (
        "js-dynamic-require",
        "require(<expr>) with non-literal specifier — dynamic CommonJS load; target module depends on runtime value",
    ),
    (
        "js-object-freeze-enum",
        "const X = Object.freeze({...}) with ≥2 scalar entries — JS enum imitation pattern; verify before treating as plain Const",
    ),
];

/// Returns true if `node` is a `variable_declaration` or `lexical_declaration`
/// whose initializer is `Object.freeze(<obj>)` with ≥2 scalar-valued pairs.
///
/// Scalar values: number, string, true, false, null.
/// Non-scalar (excluded): arrow_function, function_expression, call_expression,
/// template_string with substitutions, identifier.
fn is_object_freeze_enum(node: &tree_sitter::Node) -> bool {
    check_object_freeze_enum(node).unwrap_or(false)
}

fn check_object_freeze_enum(node: &tree_sitter::Node) -> Option<bool> {
    // Walk: declaration → variable_declarator → call_expression → arguments → object
    let declarator = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == "variable_declarator")?;

    let call = (0..declarator.child_count())
        .filter_map(|i| declarator.child(i))
        .find(|c| c.kind() == "call_expression")?;

    let args = (0..call.child_count())
        .filter_map(|i| call.child(i))
        .find(|c| c.kind() == "arguments")?;

    // Find the object literal inside arguments (skip `(` and `)`)
    let obj = (0..args.child_count())
        .filter_map(|i| args.child(i))
        .find(|c| c.kind() == "object")?;

    // Count pair nodes with scalar values only
    let scalar_pairs = (0..obj.child_count())
        .filter_map(|i| obj.child(i))
        .filter(|c| c.kind() == "pair")
        .filter(|pair| {
            // value is the last named child of the pair
            let value = (0..pair.child_count())
                .filter_map(|i| pair.child(i))
                .next_back();
            value.is_some_and(|v| SCALAR_VALUE_KINDS.contains(&v.kind()))
        })
        .count();

    Some(scalar_pairs >= 2)
}

/// The identifier a route call is made on; `None` for any other receiver
/// (`this.router`, `express.Router()`, a call result).
fn route_receiver(call: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let object = call
        .child_by_field_name("function")?
        .child_by_field_name("object")?;
    if object.kind() != "identifier" {
        return None;
    }
    std::str::from_utf8(&source[object.start_byte()..object.end_byte()])
        .ok()
        .map(str::to_string)
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_javascript::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct JavaScriptProvider {
    compiled: Compiled<JavaScriptSpec>,
}

impl JavaScriptProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for JavaScriptProvider {
    fn name(&self) -> &'static str {
        "javascript"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures JavaScript handles itself. `Root` covers `@function`,
/// `@class`, `@method` and `@property`; `@route.call` writes the same root.
#[derive(Clone, Copy)]
pub enum JavaScriptRole {
    VariableName,
    Variable,
    ExportVariable,
    Heritage,
    Export,
    Decorator,
    ImportName,
    ImportSource,
    ImportAlias,
    ImportNamespace,
    ImportStar,
    RouteMethod,
    RoutePath,
    RouteCall,
    ExpressHandler,
    HapiHandler,
    DynamicImport,
    DynamicRequire,
    ObjectFreezeEnum,
    Root,
}

#[derive(Clone, Copy)]
pub enum JavaScriptHook {
    Calls,
    Frameworks,
    Indirect,
    Exports,
}

#[derive(Default)]
pub struct JavaScriptMatch<'t> {
    heritage: Vec<String>,
    is_exported: bool,
    decorators: Vec<String>,
    import_name: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    import_alias: Option<Node<'t>>,
    is_import_namespace: bool,
    is_import_star: bool,
    variable_name_node: Option<Node<'t>>,
    variable_root_node: Option<Node<'t>>,
    is_exported_variable: bool,
    route_method: Option<Node<'t>>,
    route_path: Option<Node<'t>>,
    is_route: bool,
}

#[derive(Default)]
pub struct JavaScriptFile {
    // Pending framework-handler captures: (handler_name, capture_span).
    // Enclosing function is resolved after all nodes are collected so the
    // `enclosing_function_name` span search sees the full node set.
    pending_express_handlers: Vec<(String, Span)>,
    pending_hapi_handlers: Vec<(String, Span)>,
    receivers: Vec<(Span, String)>,
}

const fn step(pass: Pass, effect: Effect, op: Op<JavaScriptHook>) -> Step<JavaScriptHook> {
    Step { pass, effect, op }
}

impl ParseSpec for JavaScriptSpec {
    type Role = JavaScriptRole;
    type Match<'t> = JavaScriptMatch<'t>;
    type File<'t> = JavaScriptFile;
    type Hook = JavaScriptHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// Anonymous callback (arg-position arrow / function expression). Emit an
    /// `<anonymous>` Function node only when the body holds a call, so
    /// attach_to_enclosing can host those calls instead of dropping them;
    /// empty callbacks stay out of the graph. `anon_spans` is also the span
    /// set the declarations insert into and the variables test.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        ("blind.eval", BLIND_SPEC[0]),
        ("blind.function_ctor", BLIND_SPEC[1]),
    ];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["member_expression"],
        names: NameRefSpec {
            ident_kinds: &[
                "identifier",
                "shorthand_property_identifier",
                "shorthand_property_identifier_pattern",
            ],
            kind: NameRefKind::Value,
            binding_fields: &[
                ("for_in_statement", "left"),
                ("pair_pattern", "value"),
                ("object_assignment_pattern", "left"),
            ],
            skip_kinds: &[
                "import_statement",
                "decorator",
                "type_annotation",
                "type_arguments",
            ],
            ..NameRefSpec::EMPTY
        },
    });
    const EVENT_TOPICS: &'static [EventTopicConfig] = &[REDIS_JS, KAFKA_NODE, RABBITMQ_JS, SQS_JS];
    const STEPS: &'static [Step<JavaScriptHook>] = &[
        step(Pass::Query, Effect::Nodes, Op::Query),
        step(Pass::Calls, Effect::Nodes, Op::Hook(JavaScriptHook::Calls)),
        step(Pass::Walk, Effect::Nodes, Op::FieldReads),
        step(
            Pass::Build,
            Effect::Neutral,
            Op::Hook(JavaScriptHook::Frameworks),
        ),
        step(
            Pass::Walk,
            Effect::Neutral,
            Op::Hook(JavaScriptHook::Indirect),
        ),
        step(Pass::Walk, Effect::Neutral, Op::FunctionMeta),
        step(
            Pass::Build,
            Effect::Nodes,
            Op::Hook(JavaScriptHook::Exports),
        ),
        step(Pass::Build, Effect::Nodes, Op::StampOwners),
        // The language `file_identities` reads off the same path, so a
        // nested function loses its owner exactly where its uid is scoped.
        step(Pass::Build, Effect::Nodes, Op::StampOwnerFns),
        // Last owner step: a nested function or callback node has a lexical
        // parent and no owner (invariant I1), whatever the class pass gave it.
        step(Pass::Build, Effect::Nodes, Op::ClearScopedOwners),
        step(Pass::Walk, Effect::Neutral, Op::EventTopicsRequery),
    ];

    fn query() -> anyhow::Result<Query> {
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        Ok(Query::new(
            &tree_sitter_javascript::LANGUAGE.into(),
            &query_source,
        )?)
    }

    fn role(capture: &str) -> Option<JavaScriptRole> {
        Some(match capture {
            "variable.name" => JavaScriptRole::VariableName,
            "variable" => JavaScriptRole::Variable,
            "export.variable" => JavaScriptRole::ExportVariable,
            "heritage" => JavaScriptRole::Heritage,
            "export" => JavaScriptRole::Export,
            "decorator" => JavaScriptRole::Decorator,
            "import.name" => JavaScriptRole::ImportName,
            "import.source" => JavaScriptRole::ImportSource,
            "import.alias" => JavaScriptRole::ImportAlias,
            "import.namespace" => JavaScriptRole::ImportNamespace,
            "import.star" => JavaScriptRole::ImportStar,
            "route.method" => JavaScriptRole::RouteMethod,
            "route.path" => JavaScriptRole::RoutePath,
            "route.call" => JavaScriptRole::RouteCall,
            "express.route.handler" => JavaScriptRole::ExpressHandler,
            "hapi.route.handler" => JavaScriptRole::HapiHandler,
            "blind.dynamic_import" => JavaScriptRole::DynamicImport,
            "blind.dynamic_require" => JavaScriptRole::DynamicRequire,
            "blind.object_freeze_enum" => JavaScriptRole::ObjectFreezeEnum,
            "function" | "class" | "method" | "property" => JavaScriptRole::Root,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: JavaScriptRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut JavaScriptMatch<'t>,
        f: &mut JavaScriptFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            // Variable emission has dedicated post-processing (arrow dedup,
            // const/let/var kind split, span dedup) in `end_match`.
            JavaScriptRole::VariableName => m.variable_name_node = Some(node),
            JavaScriptRole::Variable => m.variable_root_node = Some(node),
            JavaScriptRole::ExportVariable => {
                m.is_exported_variable = true;
                if m.variable_root_node.is_none() {
                    m.variable_root_node = Some(node);
                }
            }
            JavaScriptRole::Heritage => {
                if let Ok(h_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.heritage.push(h_str.to_string());
                }
            }
            JavaScriptRole::Export => m.is_exported = true,
            JavaScriptRole::Decorator => {
                if let Ok(d_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.decorators.push(d_str.to_string());
                }
            }
            JavaScriptRole::ImportName => m.import_name = Some(node),
            JavaScriptRole::ImportSource => m.import_src = Some(node),
            JavaScriptRole::ImportAlias => m.import_alias = Some(node),
            JavaScriptRole::ImportNamespace => m.is_import_namespace = true,
            JavaScriptRole::ImportStar => m.is_import_star = true,
            JavaScriptRole::RouteMethod => m.route_method = Some(node),
            JavaScriptRole::RoutePath => m.route_path = Some(node),
            JavaScriptRole::RouteCall => {
                m.is_route = true;
                d.root = Some(node);
            }
            JavaScriptRole::ExpressHandler => {
                // The handler can be an identifier, member access, or
                // an inline function/arrow expression (fixed: PR #2
                // review issue #2). Inline functions have no symbolic
                // target — emit "<anonymous>" so shape_check etc.
                // recognise the route exists but is unreferenceable.
                let kind = node.kind();
                let target_name = if kind == "arrow_function" || kind == "function_expression" {
                    "<anonymous>".to_string()
                } else if let Ok(text) =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    text.to_string()
                } else {
                    return;
                };
                f.pending_express_handlers
                    .push((target_name, node_span(&node)));
            }
            JavaScriptRole::HapiHandler => {
                if let Ok(handler_name) =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    f.pending_hapi_handlers
                        .push((handler_name.to_string(), node_span(&node)));
                }
            }
            JavaScriptRole::DynamicImport => {
                if !js_ts_first_arg_is_literal_string(&node) {
                    cx.push_blind(BLIND_SPEC[2], &node);
                }
            }
            JavaScriptRole::DynamicRequire => {
                if !js_ts_first_arg_is_literal_string(&node) {
                    cx.push_blind(BLIND_SPEC[3], &node);
                }
            }
            JavaScriptRole::ObjectFreezeEnum => {
                if is_object_freeze_enum(&node) {
                    cx.push_blind(BLIND_SPEC[4], &node);
                }
            }
            JavaScriptRole::Root => d.root = Some(node),
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut JavaScriptMatch<'t>,
        f: &mut JavaScriptFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let JavaScriptMatch {
            heritage,
            is_exported,
            decorators,
            import_name,
            import_src,
            import_alias,
            is_import_namespace,
            is_import_star,
            variable_name_node,
            variable_root_node,
            is_exported_variable,
            route_method,
            route_path,
            is_route,
        } = std::mem::take(m);
        let root_span_node = d.root;

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, root_span_node) {
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                let start = root.start_position();
                let end = root.end_position();
                let decl_span = (
                    start.row as u32,
                    start.column as u32,
                    end.row as u32,
                    end.column as u32,
                );
                // JS has no separate constructor_declaration node — constructors
                // are method_definition nodes whose property_identifier is literally
                // "constructor". Promote them here so the graph emits
                // NodeKind::Constructor (parity with Java / Dart / C#).
                let k = if k == NodeKind::Method && name_str == "constructor" {
                    NodeKind::Constructor
                } else {
                    k
                };

                let mut existing_found = false;
                for existing in &mut cx.nodes {
                    if existing.span == decl_span && existing.name == name_str {
                        if is_exported {
                            existing.is_exported = true;
                        }
                        if !heritage.is_empty() {
                            for h in &heritage {
                                if !existing.heritage.contains(h) {
                                    existing.heritage.push(h.clone());
                                }
                            }
                        }
                        if !decorators.is_empty() {
                            for dec in &decorators {
                                if !existing.decorators.contains(dec) {
                                    existing.decorators.push(dec.clone());
                                }
                            }
                        }
                        existing_found = true;
                        break;
                    }
                }

                if !existing_found {
                    cx.anon_spans.insert(decl_span);
                    cx.nodes.push(RawNode {
                        decorators,
                        is_exported,
                        heritage,
                        ..node::decl(
                            name_str.to_string(),
                            k,
                            decl_span,
                            &source[root.start_byte()..root.end_byte()],
                        )
                    });
                }
            }
        }

        // Variable / Const emission — module-level only via queries.scm's
        // `(program …)` anchor on the bare `lexical_declaration` /
        // `variable_declaration` patterns. Function-valued
        // declarators are already captured as `name.function` above and
        // produce a Function node; skip them here so we don't shadow it
        // with a duplicate Variable.
        if let (Some(vn), Some(vr)) = (variable_name_node, variable_root_node) {
            // `vr` is the lexical_declaration / variable_declaration node
            // itself, except for the `export_statement` wrapper which binds
            // `@variable` to the inner `variable_declarator` — in that case
            // we walk up one step to get the declaration for kind detection.
            let decl_node = if vr.kind() == "variable_declarator" {
                vr.parent().unwrap_or(vr)
            } else {
                vr
            };
            // Inspect this binding, including later declarators in a declaration.
            let is_function = vn
                .parent()
                .and_then(|declarator| declarator.child_by_field_name("value"))
                .is_some_and(|value| {
                    matches!(value.kind(), "arrow_function" | "function_expression")
                });

            if !is_function {
                if let Ok(name_str) = std::str::from_utf8(&source[vn.start_byte()..vn.end_byte()]) {
                    let start = decl_node.start_position();
                    let end = decl_node.end_position();
                    let var_span = (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    );
                    // Skip a span a declaration or an anonymous callback
                    // already holds: an arrow-function const declarator was
                    // pushed as Function via the @function.name capture.
                    if !cx.anon_spans.contains(&var_span) {
                        // `const` declarations map to NodeKind::Const so
                        // parity with ref-gitnexus is maintained; var/let → Variable.
                        let var_kind = if decl_node.kind() == "lexical_declaration" {
                            // lexical_declaration covers both `let` and `const`.
                            // Inspect the first token to distinguish them.
                            let is_const = decl_node
                                .child(0)
                                .and_then(|t| {
                                    std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                                        .ok()
                                        .map(|s| s == "const")
                                })
                                .unwrap_or(false);
                            if is_const {
                                NodeKind::Const
                            } else {
                                NodeKind::Variable
                            }
                        } else {
                            NodeKind::Variable
                        };
                        cx.nodes.push(RawNode {
                            is_exported: is_exported_variable || is_exported,
                            ..node::decl(
                                name_str.to_string(),
                                var_kind,
                                var_span,
                                &source[decl_node.start_byte()..decl_node.end_byte()],
                            )
                        });
                    }
                }
            }
        }

        if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
            if let (Ok(name_str), Ok(src_str)) = (
                std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
            ) {
                let alias = import_alias.and_then(|a| {
                    std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                        .ok()
                        .map(|s| s.to_string())
                });

                cx.imports.push(driver::imports::raw(
                    src_str.to_string(),
                    name_str.to_string(),
                    alias,
                    js_ts_reexport_kind(i_src),
                ));
            }
        }

        // Namespace re-export `export * as ns from 'lib'` — no `name`
        // identifier exists; emit with "*" sentinel as imported_name.
        if is_import_namespace {
            if let (Some(a), Some(i_src)) = (import_alias, import_src) {
                if let (Ok(alias_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[a.start_byte()..a.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    cx.imports.push(driver::imports::raw(
                        src_str.to_string(),
                        "*".to_string(),
                        Some(alias_str.to_string()),
                        None,
                    ));
                }
            }
        }

        // Star re-export `export * from 'lib'` — no name and no alias.
        if is_import_star {
            if let Some(src_str) = import_src
                .and_then(|s| std::str::from_utf8(&source[s.start_byte()..s.end_byte()]).ok())
            {
                cx.imports.push(driver::imports::raw(
                    src_str.to_string(),
                    "*".to_string(),
                    None,
                    None,
                ));
            }
        }

        if is_route {
            if let (Some(r_method), Some(r_path), Some(root)) =
                (route_method, route_path, root_span_node)
            {
                if let (Ok(method_str), Ok(path_str)) = (
                    std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                    std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                ) {
                    let start = root.start_position();
                    let end = root.end_position();
                    // The last argument is the handler Express runs;
                    // the ones before it are middleware.
                    let handler = crate::route_detector::last_argument_handler(
                        root.child_by_field_name("arguments"),
                        source,
                        &["arrow_function", "function_expression", "function"],
                        &["identifier", "member_expression"],
                    );
                    if let Some(receiver) = route_receiver(root, source) {
                        f.receivers.push((
                            (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                            receiver,
                        ));
                    }
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
        }
    }

    fn hook<'t>(
        h: JavaScriptHook,
        _c: &Compiled<Self>,
        f: &mut JavaScriptFile,
        cx: &mut FileCx<'t>,
    ) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            JavaScriptHook::Calls => {
                // Extract call sites with receiver-type binding:
                // - `this.method()` inside a class body → `ClassName.method`
                // - `obj.method()` (no type info in JS) → `obj.method` (qualified for resolver)
                // - `fn()` → `fn`
                let (raw_path_literals, raw_sql_refs) = extract_js_calls_and_path_literals(
                    root,
                    source,
                    &mut cx.nodes,
                    &mut cx.call_sites,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            JavaScriptHook::Frameworks => {
                // Framework-presence gates: only emit Express/Hapi refs when the file
                // actually imports the matching package. Each framework has its own
                // explicit signal list (rather than a shared regex) so adding more
                // frameworks stays a one-liner.
                const EXPRESS_REQUIRED: &[&str] = &["express"];
                const HAPI_REQUIRED: &[&str] = &["@hapi/hapi", "hapi"];
                let has_express = has_import_from(&cx.imports, EXPRESS_REQUIRED);
                let has_hapi = has_import_from(&cx.imports, HAPI_REQUIRED);

                // Path-shape filter for generic Route emission. Imports record ES
                // `import` statements and top-level `require('lib')` bindings only:
                // a router built by `require('express').Router()`, a `require`
                // inside a function, or an app handed in from another module
                // leaves no import, so a framework-presence gate would drop real
                // Node.js routes. The path-shape predicate alone removes the dominant FP class
                // (`Map.get("k")` / `headers.get("x-trace")` / `cache.get(id)`)
                // because none of those literals start with `/`. Spec:
                // `docs/superpowers/specs/2026-05-17-route-precision-design.md`.
                cx.routes
                    .retain_mut(|r| match crate::route_detector::clean_route_path(&r.path) {
                        Some(clean) => {
                            r.path = clean;
                            true
                        }
                        None => false,
                    });
                // Only the receivers of the calls the path-shape filter above kept.
                let routes = &cx.routes;
                cx.route_receivers = std::mem::take(&mut f.receivers)
                    .into_iter()
                    .filter(|(span, _)| routes.iter().any(|route| route.span == *span))
                    .map(|(span, receiver)| RawRouteReceiver { span, receiver })
                    .collect();

                // Resolve framework-ref enclosing functions via span containment.
                // Module-level captures use the MODULE_LEVEL_SOURCE sentinel (consistent
                // with TS Express and Actix).
                let nodes = &cx.nodes;
                if has_express {
                    let pending = std::mem::take(&mut f.pending_express_handlers);
                    cx.framework_refs
                        .extend(pending.into_iter().map(|(target, span)| RawFrameworkRef {
                            source_name: node::enclosing_or_module(nodes, span),
                            target_name: target,
                            confidence: framework_confidence::EXPRESS_ROUTE,
                            reason: "express-route".to_string(),
                            span,
                        }));
                }
                if has_hapi {
                    let pending = std::mem::take(&mut f.pending_hapi_handlers);
                    cx.framework_refs
                        .extend(pending.into_iter().map(|(target, span)| RawFrameworkRef {
                            source_name: node::enclosing_or_module(nodes, span),
                            target_name: target,
                            confidence: framework_confidence::HAPI_ROUTE,
                            reason: "hapi-route".to_string(),
                            span,
                        }));
                }
            }
            JavaScriptHook::Indirect => {
                let param_names = collect_js_param_names(root, source);
                cx.call_metas =
                    detect_js_ts_indirect(root, source, &cx.nodes, &param_names, &cx.call_sites);
            }
            JavaScriptHook::Exports => {
                cx.export_aliases = stamp_js_ts_exports(root, source, &mut cx.nodes);
                mark_js_ts_import_bindings(root, source, &mut cx.imports);
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::javascript::extract(root, source, nodes, category)
    }
}
