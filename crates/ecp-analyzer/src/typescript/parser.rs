use super::receiver_types::{collect_local_types, extract_ts_calls_and_path_literals};
use super::schema_extractors::TS_INTERFACE_CONFIG;
use super::spec::{TsSpec, TsxSpec, TypeScriptSpec};
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture,
    Op, ParseSpec, Step, TxRule,
};
use crate::event_topic::{EventTopicConfig, KAFKA_NODE, RABBITMQ_TS, REDIS_TS, SQS_TS};
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_typeorm_transactional_scopes, has_import_from, js_ts_first_arg_is_literal_string,
    node_span, push_blind_spot, SCALAR_VALUE_KINDS,
};
use crate::indirect_dispatch::{collect_js_param_names, detect_js_ts_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use crate::schema_field::SchemaFieldConfig;
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    BlindSpot, LocalGraph, RawFrameworkRef, RawFunctionMeta, RawNode, RawRoute, Span,
};
use ecp_core::graph::{FileCategory, NodeKind};
use std::path::Path;
use tree_sitter::{Node, Parser, Query, Tree};

/// Blind-spot kind/hint pairs pushed only when the first argument is not a
/// literal string; the unconditional pairs live in `TypeScriptSpec::BLIND`.
const BLIND_DYNAMIC_IMPORT: (&str, &str) = (
    "ts-dynamic-import",
    "import(<expr>) with non-literal specifier — dynamic module loading; target module depends on runtime value",
);
const BLIND_DYNAMIC_REQUIRE: (&str, &str) = (
    "ts-dynamic-require",
    "require(<expr>) with non-literal specifier — dynamic CommonJS load; target module depends on runtime value",
);

/// True iff the `object` node has ≥ `min` `pair` children where every
/// value is a scalar literal (number / string / bool / null).
fn object_has_min_scalar_pairs(object_node: &tree_sitter::Node, min: usize) -> bool {
    let mut scalar_count = 0usize;
    let mut cursor = object_node.walk();
    for child in object_node.named_children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }
        let Some(val) = child.named_child(1) else {
            return false;
        };
        if !SCALAR_VALUE_KINDS.contains(&val.kind()) {
            return false;
        }
        scalar_count += 1;
    }
    scalar_count >= min
}

/// The `const X = …;` statement around a declarator's value, else the value.
fn declaration_of(value: tree_sitter::Node) -> tree_sitter::Node {
    value
        .parent()
        .filter(|p| p.kind() == "variable_declarator")
        .and_then(|vd| vd.parent())
        .filter(|p| matches!(p.kind(), "lexical_declaration" | "variable_declaration"))
        .unwrap_or(value)
}

/// Emit `ts-object-freeze-enum` BlindSpots for every `Object.freeze({…})`
/// call with ≥2 scalar-literal pair entries found anywhere in `root`.
/// Traversal is depth-first; the enclosing declaration gives the span.
fn collect_freeze_enum_spots(
    root: tree_sitter::Node,
    source: &[u8],
    path: &Path,
    is_test_file: bool,
    out: &mut Vec<BlindSpot>,
) {
    // Iterative DFS — avoids recursion depth limits on deeply nested sources.
    let mut stack: Vec<tree_sitter::Node> = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            if let Some(func) = node.child_by_field_name("function") {
                if func.kind() == "member_expression" {
                    let obj_ok = func.child_by_field_name("object").is_some_and(|o| {
                        matches!(
                            std::str::from_utf8(&source[o.start_byte()..o.end_byte()]),
                            Ok("Object")
                        )
                    });
                    let prop_ok = func.child_by_field_name("property").is_some_and(|p| {
                        matches!(
                            std::str::from_utf8(&source[p.start_byte()..p.end_byte()]),
                            Ok("freeze")
                        )
                    });
                    if obj_ok && prop_ok {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            if let Some(obj_arg) = args.named_child(0) {
                                if obj_arg.kind() == "object"
                                    && object_has_min_scalar_pairs(&obj_arg, 2)
                                {
                                    let span_node = declaration_of(node);
                                    push_blind_spot(
                                        out,
                                        (
                                            "ts-object-freeze-enum",
                                            "const X = Object.freeze({...}) or X as const with ≥2 scalar entries \
                                             — TS enum imitation; verify before treating as plain Const",
                                        ),
                                        &span_node,
                                        path,
                                        is_test_file,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        } else if node.kind() == "as_expression" {
            // `{ ... } as const` — `const` keyword is an unnamed child of
            // `as_expression`; check it before inspecting the object.
            let has_const_type = {
                let mut c = node.walk();
                let result = node
                    .children(&mut c)
                    .any(|ch| !ch.is_named() && ch.kind() == "const");
                result
            };
            if has_const_type {
                if let Some(obj_node) = node.named_child(0) {
                    if obj_node.kind() == "object" && object_has_min_scalar_pairs(&obj_node, 2) {
                        let span_node = declaration_of(node);
                        push_blind_spot(
                            out,
                            (
                                "ts-object-freeze-enum",
                                "const X = Object.freeze({...}) or X as const with ≥2 scalar entries \
                                 — TS enum imitation; verify before treating as plain Const",
                            ),
                            &span_node,
                            path,
                            is_test_file,
                        );
                    }
                }
                // Don't recurse into as_expression children — the object literal
                // nested inside cannot be a separate enum-imitation.
                continue;
            }
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// The `@Controller` prefixes of the class around a route decorator: `None`
/// when the class carries no `@Controller`, an empty list when it carries
/// one without a path (`@Controller()`), and one entry per path otherwise
/// ([`crate::route_detector::nest_controller_prefixes`], the reader the
/// route decorator pass uses too). The grammar puts a class's decorators on
/// the class declaration, or on the export statement for
/// `@Controller() export class`.
fn controller_prefixes(verb: tree_sitter::Node, source: &[u8]) -> Option<Vec<String>> {
    let class = std::iter::successors(verb.parent(), |n| n.parent()).find(|n| {
        matches!(
            n.kind(),
            "class_declaration" | "abstract_class_declaration" | "class"
        )
    })?;
    let controller_call = |decorator: tree_sitter::Node| -> Option<Vec<String>> {
        let expr = decorator.named_child(0)?;
        let callee = match expr.kind() {
            "call_expression" => expr.child_by_field_name("function"),
            _ => Some(expr),
        };
        let name = callee.map(|c| match c.kind() {
            "member_expression" => c.child_by_field_name("property").unwrap_or(c),
            _ => c,
        });
        if name.and_then(|n| n.utf8_text(source).ok()) != Some("Controller") {
            return None;
        }
        let text = decorator.utf8_text(source).ok()?;
        Some(crate::route_detector::nest_controller_prefixes(text))
    };
    let find = |n: tree_sitter::Node| {
        let mut cursor = n.walk();
        let found = n
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "decorator")
            .find_map(controller_call);
        found
    };
    find(class).or_else(|| {
        class
            .parent()
            .filter(|p| p.kind() == "export_statement")
            .and_then(find)
    })
}

/// A NestJS route path under one `@Controller` prefix. An empty prefix
/// leaves a method path as written; any other prefix, or an empty method
/// path (`@Get()`), goes through the one prefix rule, C1 `join_route_path`,
/// so `@Controller() @Get()` is `/`.
fn nestjs_route_path(prefix: &str, path: &str) -> String {
    match prefix.trim().is_empty() && !path.trim().is_empty() {
        true => path.to_string(),
        false => ecp_core::contracts::identity::join_route_path(prefix, path),
    }
}

/// The name of the method a route decorator decorates, from the decorator's
/// verb identifier (`Get` in `@Get(':id') find() {}`). The grammar puts a
/// member's decorators before it in the class body; the method is the first
/// following sibling that is not another decorator.
fn decorated_method_name(verb: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let decorator = verb
        .parent()?
        .parent()
        .filter(|d| d.kind() == "decorator")?;
    let mut next = decorator.next_named_sibling();
    while let Some(n) = next.filter(|n| n.kind() == "decorator" || n.is_extra()) {
        next = n.next_named_sibling();
    }
    let member = next
        .filter(|n| n.kind() == "method_definition")
        .or_else(|| {
            decorator
                .parent()
                .filter(|p| p.kind() == "method_definition")
        })?;
    let name = member.child_by_field_name("name")?.utf8_text(source).ok()?;
    Some(name.to_string())
}

/// A query is bound to its grammar, so each grammar compiles its own.
fn grammar<const TSX: bool>() -> tree_sitter::Language {
    match TSX {
        true => tree_sitter_typescript::LANGUAGE_TSX.into(),
        false => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    }
}

pub struct TypeScriptProvider {
    compiled: Compiled<TypeScriptSpec>,
    compiled_tsx: Compiled<TsxSpec>,
}

impl TypeScriptProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
            compiled_tsx: Compiled::new()?,
        })
    }
}

impl LanguageProvider for TypeScriptProvider {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        match path.extension().is_some_and(|e| e == "tsx") {
            true => driver::parse(&self.compiled_tsx, path, source),
            false => driver::parse(&self.compiled, path, source),
        }
    }
}

/// The captures TypeScript handles itself. A root capture sets the
/// declaration root; the `.name` kind capture of the same match sets name
/// and kind. `route.call` sets the same root: a match has at most one root.
#[derive(Clone, Copy)]
pub enum TypeScriptRole {
    Root,
    Export,
    Heritage,
    Type,
    Decorator,
    ImportName,
    ImportAlias,
    ImportSource,
    Import,
    /// `export * as ns from 'lib'` — no `name` identifier; `imported_name`
    /// is set to the "*" sentinel.
    ImportNamespace,
    /// `export * from 'lib'` — no name and no alias; emitted as a "*" import
    /// with `alias: None`.
    ImportStar,
    RouteMethod,
    RoutePath,
    RouteCall,
    ExpressHandler,
    NestJsClass,
    NestJsMethod,
    /// `@Get('users')` / `@Post(':id')` decorator-route captures — separate
    /// pattern in frameworks.scm because the decorator is independent of the
    /// `@Controller` shape (any method-level decorator carrying a path-shaped
    /// string literal is a route, gated by `has_nestjs` import presence).
    NestJsDecoratorVerb,
    NestJsDecoratorPath,
    /// The argument list of a route decorator: `@Get()` has an empty one,
    /// which names the controller prefix itself.
    NestJsDecoratorArgs,
    DynamicImport,
    DynamicRequire,
}

#[derive(Clone, Copy)]
pub enum TypeScriptHook {
    FreezeEnums,
    Calls,
    Frameworks,
    Indirect,
    Exports,
}

/// `(class_name, method_name, method_span)`
type NestJsHandler = (String, String, Span);
/// `(HTTP_METHOD, raw_path_with_quotes_stripped_by_capture, decorator_span,
/// decorated method name)`
type NestJsDecoratorRoute = (String, String, Span, Option<String>);

#[derive(Default)]
pub struct TypeScriptMatch<'t> {
    is_exported: bool,
    heritage: Vec<String>,
    type_annotation: Option<String>,
    decorators: Vec<String>,
    import_name: Option<Node<'t>>,
    import_alias: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    is_import: bool,
    is_import_namespace: bool,
    is_import_star: bool,
    route_method: Option<Node<'t>>,
    route_path: Option<Node<'t>>,
    is_route: bool,
    nestjs_class_node: Option<Node<'t>>,
    nestjs_method_node: Option<Node<'t>>,
    nestjs_decorator_verb_node: Option<Node<'t>>,
    nestjs_decorator_path_node: Option<Node<'t>>,
    nestjs_decorator_args_node: Option<Node<'t>>,
}

#[derive(Default)]
pub struct TypeScriptFile {
    /// `(handler_name, capture_span)`; the enclosing function is resolved
    /// after all nodes are collected.
    pending_express_handlers: Vec<(String, Span)>,
    pending_nestjs_handlers: Vec<NestJsHandler>,
    /// Gated on `has_nestjs`, which is known only once the query has
    /// collected every import.
    pending_nestjs_decorator_routes: Vec<NestJsDecoratorRoute>,
}

impl<const TSX: bool> ParseSpec for TsSpec<TSX> {
    type Role = TypeScriptRole;
    type Match<'t> = TypeScriptMatch<'t>;
    type File<'t> = TypeScriptFile;
    type Hook = TypeScriptHook;

    const PARSE_ERROR: &'static str = "Failed to parse typescript file";
    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// Anonymous callback (arg-position arrow / function expression). Emit an
    /// `<anonymous>` Function node only when the body holds a call, so
    /// attach_to_enclosing can host those calls instead of dropping them;
    /// empty callbacks (e.g. `arr.map(x => x * 2)`) stay out of the graph.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.eval",
            (
                "ts-eval",
                "eval(arg) — runtime JS execution; argument expression is not statically determinable as a callee",
            ),
        ),
        (
            "blind.function_ctor",
            (
                "ts-function-ctor",
                "Function(arg) / new Function(arg) — runtime function compilation; body source is not statically determinable",
            ),
        ),
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
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        path_kinds: &["member_expression"],
        label_parents: &["switch_case"],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: false,
    });
    const EVENT_TOPICS: &'static [EventTopicConfig] = &[REDIS_TS, KAFKA_NODE, RABBITMQ_TS, SQS_TS];
    const SCHEMA_FIELDS: &'static [SchemaFieldConfig] = &[TS_INTERFACE_CONFIG];
    const TX: TxRule = TxRule::Nodes {
        collect: collect_typeorm_transactional_scopes,
        kinds: &[NodeKind::Method, NodeKind::Function],
    };
    const STEPS: &'static [Step<TypeScriptHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Walk, Effect::Neutral, TypeScriptHook::FreezeEnums),
        Step::hook(Pass::Calls, Effect::Nodes, TypeScriptHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Build, Effect::Neutral, TypeScriptHook::Frameworks),
        Step::hook(Pass::Walk, Effect::Neutral, TypeScriptHook::Indirect),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::hook(Pass::Build, Effect::Nodes, TypeScriptHook::Exports),
        Step::fixed(Pass::Build, Op::StampOwners),
        // The language `file_identities` reads off the same path, so a
        // nested function loses its owner exactly where its uid is scoped.
        Step::fixed(Pass::Build, Op::StampOwnerFns),
        // Last owner step: a nested function or callback node has a lexical
        // parent and no owner (invariant I1), whatever the class pass gave it.
        Step::fixed(Pass::Build, Op::ClearScopedOwners),
        Step::fixed(Pass::Build, Op::TxScopes),
        Step::fixed(Pass::Walk, Op::SchemaFields),
        Step::fixed(Pass::Walk, Op::EventTopicsRequery),
    ];

    fn query() -> anyhow::Result<Query> {
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        Ok(Query::new(&grammar::<TSX>(), &query_source)?)
    }

    fn role(capture: &str) -> Option<TypeScriptRole> {
        use TypeScriptRole::*;
        Some(match capture {
            "function" | "class" | "method" | "constructor" | "interface" | "property"
            | "const" | "variable" | "typedef" | "enum" | "enum_member_node" => Root,
            "export" => Export,
            "heritage" => Heritage,
            "type" => Type,
            "decorator" => Decorator,
            "import.name" => ImportName,
            "import.alias" => ImportAlias,
            "import.source" => ImportSource,
            "import" => Import,
            "import.namespace" => ImportNamespace,
            "import.star" => ImportStar,
            "route.method" => RouteMethod,
            "route.path" => RoutePath,
            "route.call" => RouteCall,
            "express.route.handler" => ExpressHandler,
            "nestjs.controller.class" => NestJsClass,
            "nestjs.method.name" => NestJsMethod,
            "nestjs.decorator.verb" => NestJsDecoratorVerb,
            "nestjs.decorator.path" => NestJsDecoratorPath,
            "nestjs.decorator.args" => NestJsDecoratorArgs,
            "blind.dynamic_import" => DynamicImport,
            "blind.dynamic_require" => DynamicRequire,
            _ => return None,
        })
    }

    /// A fresh parser per file, on the grammar the spec names.
    fn parse_tree(source: &[u8]) -> Option<Tree> {
        let mut parser = Parser::new();
        parser.set_language(&grammar::<TSX>()).ok()?;
        parse_with_budget(&mut parser, source, ParseBudget::DEFAULT)
    }

    fn capture<'t>(
        r: TypeScriptRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut TypeScriptMatch<'t>,
        f: &mut TypeScriptFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            TypeScriptRole::Root => d.root = Some(node),
            TypeScriptRole::Export => m.is_exported = true,
            TypeScriptRole::Heritage => {
                if let Ok(h) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.heritage.push(h.to_string());
                }
            }
            TypeScriptRole::Type => {
                if let Ok(t) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.type_annotation = Some(t.to_string());
                }
            }
            TypeScriptRole::Decorator => {
                if let Ok(text) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.decorators.push(text.to_string());
                }
            }
            TypeScriptRole::ImportName => m.import_name = Some(node),
            TypeScriptRole::ImportAlias => m.import_alias = Some(node),
            TypeScriptRole::ImportSource => m.import_src = Some(node),
            TypeScriptRole::Import => m.is_import = true,
            TypeScriptRole::ImportNamespace => m.is_import_namespace = true,
            TypeScriptRole::ImportStar => m.is_import_star = true,
            TypeScriptRole::RouteMethod => m.route_method = Some(node),
            TypeScriptRole::RoutePath => m.route_path = Some(node),
            TypeScriptRole::RouteCall => {
                m.is_route = true;
                d.root = Some(node);
            }
            TypeScriptRole::ExpressHandler => {
                if let Ok(handler_name) =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    f.pending_express_handlers
                        .push((handler_name.to_string(), node_span(&node)));
                }
            }
            TypeScriptRole::NestJsClass => m.nestjs_class_node = Some(node),
            TypeScriptRole::NestJsMethod => m.nestjs_method_node = Some(node),
            TypeScriptRole::NestJsDecoratorVerb => m.nestjs_decorator_verb_node = Some(node),
            TypeScriptRole::NestJsDecoratorPath => m.nestjs_decorator_path_node = Some(node),
            TypeScriptRole::NestJsDecoratorArgs => m.nestjs_decorator_args_node = Some(node),
            TypeScriptRole::DynamicImport => {
                if !js_ts_first_arg_is_literal_string(&node) {
                    cx.push_blind(BLIND_DYNAMIC_IMPORT, &node);
                }
            }
            TypeScriptRole::DynamicRequire => {
                if !js_ts_first_arg_is_literal_string(&node) {
                    cx.push_blind(BLIND_DYNAMIC_REQUIRE, &node);
                }
            }
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut TypeScriptMatch<'t>,
        f: &mut TypeScriptFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let TypeScriptMatch {
            is_exported,
            heritage,
            type_annotation,
            decorators,
            import_name,
            import_alias,
            import_src,
            is_import,
            is_import_namespace,
            is_import_star,
            route_method,
            route_path,
            is_route,
            nestjs_class_node,
            nestjs_method_node,
            nestjs_decorator_verb_node,
            nestjs_decorator_path_node,
            nestjs_decorator_args_node,
        } = std::mem::take(m);
        let root_span_node = d.root;

        // Process definitions
        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, root_span_node) {
            // A function-valued binding is the `@function` match's node; this
            // match ends here, skipping every record below.
            if matches!(k, NodeKind::Const | NodeKind::Variable)
                && n.parent()
                    .and_then(|declarator| declarator.child_by_field_name("value"))
                    .is_some_and(|value| {
                        matches!(value.kind(), "arrow_function" | "function_expression")
                    })
            {
                return;
            }
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                let span = node_span(&root);

                let mut existing_found = false;
                for node in &mut cx.nodes {
                    if node.span == span && node.name == name_str {
                        // Function is more specific than Const/Variable: an
                        // arrow-function assigned to a const matches both the
                        // @function and @const patterns; the @const pattern fires
                        // first in tree-sitter's match order, so we upgrade the
                        // kind here when the later Function match arrives.
                        if k == NodeKind::Function
                            && matches!(node.kind, NodeKind::Const | NodeKind::Variable)
                        {
                            node.kind = NodeKind::Function;
                        }
                        if is_exported {
                            node.is_exported = true;
                        }
                        if !heritage.is_empty() {
                            for h in &heritage {
                                if !node.heritage.contains(h) {
                                    node.heritage.push(h.clone());
                                }
                            }
                        }
                        if type_annotation.is_some() {
                            node.type_annotation = type_annotation.clone();
                        }
                        if !decorators.is_empty() {
                            for d in &decorators {
                                if !node.decorators.contains(d) {
                                    node.decorators.push(d.clone());
                                }
                            }
                        }
                        existing_found = true;
                        break;
                    }
                }

                if !existing_found {
                    cx.nodes.push(RawNode {
                        decorators: decorators.clone(),
                        is_exported,
                        heritage: heritage.clone(),
                        type_annotation: type_annotation.clone(),
                        ..node::decl(
                            name_str.to_string(),
                            k,
                            span,
                            &source[root.start_byte()..root.end_byte()],
                        )
                    });
                }
            }
        }

        // Process imports
        if is_import {
            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    let alias_str = import_alias.and_then(|a| {
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });

                    cx.imports.push(driver::imports::raw(
                        src_str.to_string(),
                        name_str.to_string(),
                        alias_str,
                        crate::framework_helpers::js_ts_reexport_kind(i_src),
                    ));
                }
            }
        }

        // Process namespace re-export `export * as ns from 'lib'`.
        // No `name` identifier exists; emit with "*" sentinel as imported_name
        // and the local namespace binding as the alias.
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

        // Process NestJS @Controller method handler pairs.
        if let (Some(cls), Some(mth)) = (nestjs_class_node, nestjs_method_node) {
            if let (Ok(class_name), Ok(method_name)) = (
                std::str::from_utf8(&source[cls.start_byte()..cls.end_byte()]),
                std::str::from_utf8(&source[mth.start_byte()..mth.end_byte()]),
            ) {
                f.pending_nestjs_handlers.push((
                    class_name.to_string(),
                    method_name.to_string(),
                    node_span(&mth),
                ));
            }
        }

        // Process routes
        if is_route {
            if let (Some(r_method), Some(r_path), Some(root)) =
                (route_method, route_path, root_span_node)
            {
                if let (Ok(method_str), Ok(path_str)) = (
                    std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                    std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                ) {
                    // The last argument is the handler Express runs;
                    // the ones before it are middleware.
                    let handler = crate::route_detector::last_argument_handler(
                        root.child_by_field_name("arguments"),
                        source,
                        &["arrow_function", "function_expression", "function"],
                        &["identifier", "member_expression"],
                    );
                    cx.routes.push(RawRoute {
                        method: method_str.to_string(),
                        path: path_str.to_string(),
                        handler,
                        span: node_span(&root),
                        scope: None,
                    });
                }
            }
        }

        // NestJS decorator-route — `@Get('users')` / `@Post(':id')`.
        // Path string is captured separately from the generic Express
        // matcher, and the relaxed `clean_route_path_lax` accepts bare
        // names (NestJS convention) in the frameworks step. Stash as a
        // pending decorator-route; the frameworks step resolves it once the
        // `has_nestjs` import flag is known.
        // `@Get()` names no path: its route is the controller prefix.
        let path_text = match (nestjs_decorator_path_node, nestjs_decorator_args_node) {
            (Some(path_node), _) => {
                std::str::from_utf8(&source[path_node.start_byte()..path_node.end_byte()]).ok()
            }
            (None, Some(args)) if args.named_child_count() == 0 => Some(""),
            _ => None,
        };
        if let (Some(verb_node), Some(path_str)) = (nestjs_decorator_verb_node, path_text) {
            if let (Some(prefixes), Ok(verb_str)) = (
                controller_prefixes(verb_node, source),
                std::str::from_utf8(&source[verb_node.start_byte()..verb_node.end_byte()]),
            ) {
                let prefixes = match prefixes.is_empty() {
                    true => vec![String::new()],
                    false => prefixes,
                };
                for prefix in prefixes {
                    f.pending_nestjs_decorator_routes.push((
                        verb_str.to_uppercase(),
                        nestjs_route_path(&prefix, path_str),
                        node_span(&verb_node),
                        decorated_method_name(verb_node, source),
                    ));
                }
            }
        }
    }

    fn hook<'t>(
        h: TypeScriptHook,
        _c: &Compiled<Self>,
        f: &mut TypeScriptFile,
        cx: &mut FileCx<'t>,
    ) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            // Object.freeze({...}) and `{...} as const` enum-imitation detection.
            // Runs as a separate AST walk (not query-based) because the heuristic
            // requires counting pair children and checking value kinds — logic that
            // can't be expressed as a single tree-sitter predicate.
            TypeScriptHook::FreezeEnums => {
                collect_freeze_enum_spots(root, source, cx.path, cx.is_test, &mut cx.blind_spots)
            }
            // Extract call sites with receiver-type binding:
            // - `this.method()` → resolved to `ClassName.method` via enclosing class
            // - `obj.method()` where `obj` has a type annotation → `Type.method`
            // - everything else falls back to the bare/qualified method name.
            TypeScriptHook::Calls => {
                let local_types = collect_local_types(root, source);
                let (raw_path_literals, raw_sql_refs) = extract_ts_calls_and_path_literals(
                    root,
                    source,
                    &mut cx.nodes,
                    &local_types,
                    &mut cx.call_sites,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            TypeScriptHook::Frameworks => {
                // Framework-presence gates: only emit Express/NestJS refs when the file
                // actually imports the matching package.
                const EXPRESS_REQUIRED: &[&str] = &["express"];
                const NESTJS_REQUIRED: &[&str] = &["@nestjs"];
                let has_express = has_import_from(&cx.imports, EXPRESS_REQUIRED);
                let has_nestjs = has_import_from(&cx.imports, NESTJS_REQUIRED);

                // Path-shape filter for generic Route emission — see
                // `docs/superpowers/specs/2026-05-17-route-precision-design.md`.
                // No framework-presence gate here: imports record ES `import`
                // statements and top-level `require('lib')` bindings only, so a
                // router built by `require('express').Router()`, a `require` inside
                // a function, or an app handed in from another module leaves no
                // import and gating would drop real routes. The path-shape
                // predicate alone removes
                // the dominant FP class (Map/headers/cache `.get("key")`).
                cx.routes
                    .retain_mut(|r| match crate::route_detector::clean_route_path(&r.path) {
                        Some(clean) => {
                            r.path = clean;
                            true
                        }
                        None => false,
                    });

                // NestJS decorator-routes — `@Get('users')` shape. Gated on
                // `@nestjs/*` import presence so a custom `@Get(...)` decorator in
                // an unrelated codebase does not surface a false Route. Uses the
                // lax path helper because NestJS conventionally elides the leading
                // `/` (the framework prepends the controller prefix at runtime).
                if has_nestjs {
                    for (verb, raw_path, span, method) in
                        std::mem::take(&mut f.pending_nestjs_decorator_routes)
                    {
                        if let Some(cleaned) =
                            crate::route_detector::clean_route_path_lax(&raw_path)
                        {
                            cx.routes.push(RawRoute {
                                method: verb,
                                path: cleaned,
                                handler: RawRoute::named_handler(method.as_deref()),
                                span,
                                scope: None,
                            });
                        }
                    }
                }

                // Resolve framework-ref enclosing functions via span containment.
                // Module-level captures use the MODULE_LEVEL_SOURCE sentinel (consistent with Actix).
                let mut framework_refs: Vec<RawFrameworkRef> = if has_express {
                    std::mem::take(&mut f.pending_express_handlers)
                        .into_iter()
                        .map(|(target, cap_span)| RawFrameworkRef {
                            source_name: node::enclosing_or_module(&cx.nodes, cap_span),
                            target_name: target,
                            confidence: framework_confidence::EXPRESS_ROUTE,
                            reason: "express-route-handler".to_string(),
                            span: cap_span,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                // NestJS @Controller → @Get/@Post/... method bindings.
                if has_nestjs {
                    for (class_name, method_name, span) in
                        std::mem::take(&mut f.pending_nestjs_handlers)
                    {
                        framework_refs.push(RawFrameworkRef {
                            source_name: class_name,
                            target_name: method_name,
                            confidence: framework_confidence::NESTJS_ROUTE,
                            reason: "nestjs-route-handler".to_string(),
                            span,
                        });
                    }
                }
                cx.framework_refs = framework_refs;
            }
            TypeScriptHook::Indirect => {
                let param_names = collect_js_param_names(root, source);
                cx.call_metas =
                    detect_js_ts_indirect(root, source, &cx.nodes, &param_names, &cx.call_sites);
            }
            TypeScriptHook::Exports => {
                cx.export_aliases =
                    crate::framework_helpers::stamp_js_ts_exports(root, source, &mut cx.nodes);
                crate::framework_helpers::mark_js_ts_import_bindings(root, source, &mut cx.imports);
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::typescript::extract(root, source, nodes, category)
    }
}
