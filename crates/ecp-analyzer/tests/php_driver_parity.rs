//! `PhpProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order where the order is fixed by
//! source position.

mod driver_parity_support;

use driver_parity_support::{assert_empty_graph, names_kinds, refs, Span4};
use ecp_analyzer::framework_confidence::LARAVEL_ROUTE;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, NameRef, NameRefKind, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    PhpProvider::new()
        .expect("PhpProvider::new")
        .parse_file(Path::new("a.php"), src.as_bytes())
        .expect("parse_file")
}

fn node<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node `{name}`: {:?}", names_kinds(g)))
}

fn blind_kinds(g: &LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

fn reads_constant(n: &RawNode, name: &str) -> bool {
    n.field_reads
        .contains(&NameRef::encode(name, NameRefKind::Constant))
}

/// `(method, path, handler name, span, needs_include)`
type RouteRow<'g> = (&'g str, &'g str, Option<&'g str>, Span4, Option<bool>);

fn routes(g: &LocalGraph) -> Vec<RouteRow<'_>> {
    g.routes
        .iter()
        .map(|r| {
            (
                r.method.as_str(),
                r.path.as_str(),
                r.handler_name(),
                r.span,
                r.scope.as_deref().map(|s| s.needs_include),
            )
        })
        .collect()
}

#[test]
fn test_php_driver_zero_byte_file_returns_empty_graph() {
    let g = parse("");
    assert_empty_graph(&g, "a.php");
}

/// Imports keep source order, unsorted. A group clause records the joined
/// path with `imported_name` `*` and the bound name as its alias.
#[test]
fn test_php_driver_imports_only_file_keeps_source_order_and_group_alias() {
    let g = parse("<?php\nuse App\\Models\\User;\nuse App\\Http\\{A, B as Bee};\n");
    assert!(g.nodes.is_empty(), "{:?}", names_kinds(&g));
    let imports: Vec<(&str, &str, Option<&str>)> = g
        .imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        imports,
        [
            ("App\\Models\\User", "User", None),
            ("App\\Http\\A", "*", Some("A")),
            ("App\\Http\\B", "*", Some("Bee")),
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
    assert!(g.blind_spots.is_empty() && g.routes.is_empty());
}

/// The role tests the callable: a literal one is a static call and pushes
/// no blind spot; a variable one pushes one at its call.
#[test]
fn test_php_driver_call_user_func_literal_emits_none_variable_emits_one() {
    let g = parse(
        "<?php\nfunction go($f) {\n    call_user_func('handler');\n    call_user_func($f);\n}\n",
    );
    assert_eq!(blind_kinds(&g), ["php-call-user-func"]);
    assert_eq!(g.blind_spots[0].span, (3, 4, 3, 22));
    assert_eq!(g.blind_spots[0].file_path, Path::new("a.php"));
    assert!(!g.blind_spots[0].is_test);
}

/// Capture-loop blind spots come in source order, then the dynamic-call
/// spots of the calls step, then the enum-imitation walk.
#[test]
fn test_php_driver_blind_spots_keep_step_order() {
    let src = "<?php
class A {
    function f($c, $g, $m) {
        eval($c);
        call_user_func($c);
        $g();
        return Rows::$m(1);
    }
}
class Codes { const A = 1; const B = 2; }
";
    let g = parse(src);
    let got: Vec<(&str, Span4)> = g
        .blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span))
        .collect();
    assert_eq!(
        got,
        [
            ("php-eval", (3, 8, 3, 16)),
            ("php-call-user-func", (4, 8, 4, 26)),
            ("php-variable-call", (5, 8, 5, 12)),
            ("php-variable-method-call", (6, 15, 6, 26)),
            ("php-enum-imitation", (9, 0, 9, 41)),
        ]
    );
}

/// `self::ACTIVE` names the enclosing class, so it reads the constant even
/// in a namespaced file; the const owner step names the class first.
#[test]
fn test_php_driver_namespace_class_method_const_emit_in_order() {
    let src = "<?php
namespace App;

class Status {
    const ACTIVE = 1;
    public function check() { return self::ACTIVE; }
}
";
    let g = parse(src);
    assert_eq!(
        names_kinds(&g),
        [
            ("App", NodeKind::Namespace),
            ("Status", NodeKind::Class),
            ("ACTIVE", NodeKind::Const),
            ("check", NodeKind::Method),
        ]
    );
    assert_eq!(g.nodes[0].span, (1, 0, 1, 14));
    assert_eq!(g.nodes[1].span, (3, 0, 6, 1));
    let active = node(&g, "ACTIVE");
    assert_eq!(active.span, (4, 10, 4, 20));
    assert_eq!(active.owner_class.as_deref(), Some("Status"));
    let check = node(&g, "check");
    assert_eq!(check.span, (5, 4, 5, 52));
    assert!(check.is_exported);
    assert_eq!(check.owner_class.as_deref(), Some("Status"));
    assert!(
        reads_constant(check, "Status.ACTIVE"),
        "{:?}",
        check.field_reads
    );
}

/// A qualified `Type::CONST` read counts only in a file with no namespace:
/// the resolver cannot prove namespace visibility.
#[test]
fn test_php_driver_qualified_constant_read_only_without_namespace() {
    let plain = parse(
        "<?php\nclass Status { const ACTIVE = 1; }\nfunction check() { return Status::ACTIVE; }\n",
    );
    let check = node(&plain, "check");
    assert_eq!(check.kind, NodeKind::Function);
    assert!(
        reads_constant(check, "Status.ACTIVE"),
        "{:?}",
        check.field_reads
    );
    assert_eq!(
        node(&plain, "ACTIVE").owner_class.as_deref(),
        Some("Status")
    );

    let namespaced = parse(
        "<?php\nnamespace App;\nclass Status { const ACTIVE = 1; }\nfunction check() { return Status::ACTIVE; }\n",
    );
    let check = node(&namespaced, "check");
    assert!(
        !reads_constant(check, "Status.ACTIVE"),
        "{:?}",
        check.field_reads
    );
}

/// A constant in an anonymous class takes the body's position as its owner.
#[test]
fn test_php_driver_anonymous_class_const_owner_names_body_position() {
    let g = parse("<?php\n$x = new class { const K = 1; };\n");
    assert_eq!(names_kinds(&g), [("K", NodeKind::Const)]);
    assert_eq!(g.nodes[0].owner_class.as_deref(), Some("@anonymous_1_15"));
}

/// With the `Illuminate` import the route stays unmarked and the Laravel
/// ref emits from module level.
#[test]
fn test_php_driver_laravel_route_with_import_emits_route_and_ref() {
    let g = parse(
        "<?php\nuse Illuminate\\Support\\Facades\\Route;\nRoute::get('/x', [C::class, 'm']);\n",
    );
    assert_eq!(
        routes(&g),
        [("GET", "/x", Some("C@m"), (2, 0, 2, 33), Some(false))]
    );
    assert_eq!(
        refs(&g),
        [(
            "<module>",
            "C@m",
            "laravel-route",
            LARAVEL_ROUTE,
            (2, 0, 2, 33)
        )]
    );
}

/// Without a framework import the route stays, marked `needs_include`,
/// and the Laravel ref drops.
#[test]
fn test_php_driver_laravel_route_without_import_marks_needs_include() {
    let g = parse("<?php\nRoute::get('/x', [C::class, 'm']);\n");
    assert_eq!(
        routes(&g),
        [("GET", "/x", Some("C@m"), (1, 0, 1, 33), Some(true))]
    );
    assert!(g.framework_refs.is_empty(), "{:?}", g.framework_refs);
    assert!(g.imports.is_empty());
}

/// The bind step runs after the const-owner and constant-read steps, so
/// each scope indexes the decorated callable.
#[test]
fn test_php_driver_symfony_transactional_binds_method_and_function() {
    let src = "<?php
class OrderService {
    const LIMIT = 1;
    #[Transactional]
    public function placeOrder(): void { return self::LIMIT; }
    public function listOrders(): void {}
}
#[Transactional]
function run() {}
";
    let g = parse(src);
    let scopes = g.tx_scopes.as_deref().expect("tx scopes");
    let bound: Vec<(&str, FrameworkId)> = scopes
        .iter()
        .map(|s| (g.nodes[s.node_idx() as usize].name.as_str(), s.framework()))
        .collect();
    assert_eq!(
        bound,
        [
            ("placeOrder", FrameworkId::SymfonyTransactional),
            ("run", FrameworkId::SymfonyTransactional),
        ]
    );
    assert_eq!(node(&g, "placeOrder").decorators, ["#[Transactional]"]);
}

/// A callback with a call gets one `<anonymous>` node that hosts the call;
/// an empty arrow callback gets none.
#[test]
fn test_php_driver_anonymous_function_with_call_emits_one_node() {
    let src = "<?php
function run($xs) {
    $a = array_map(fn($x) => $x * 2, $xs);
    return array_map(function ($x) { return strlen($x); }, $xs);
}
";
    let g = parse(src);
    assert_eq!(
        names_kinds(&g),
        [
            ("run", NodeKind::Function),
            ("<anonymous:4:21>", NodeKind::Function),
        ]
    );
    let anon = &g.nodes[1];
    assert_eq!(anon.span, (3, 21, 3, 57));
    assert!(!anon.is_exported && anon.owner_class.is_none());
    assert!(anon.calls.iter().any(|c| c == "strlen"), "{:?}", anon.calls);
    assert!(g.nodes[0].calls.iter().any(|c| c == "array_map"));
}

fn api(consts: &str, body: &str) -> String {
    format!(
        "<?php\nnamespace App;\n\nclass Api\n{{\n    {consts}\n\n    public function dispatch(string $type, array $p)\n    {{\n        {body}\n    }}\n\n    public function a() {{}}\n    public function b() {{}}\n}}\n"
    )
}

/// A fan-out over a const map names every candidate; a static receiver
/// keeps the blind spot as well, a named class drops it.
#[test]
fn test_php_driver_dynamic_call_fanout_keeps_blind_spot_only_late_bound() {
    let named = parse(&api(
        "private const ROW_METHODS = ['H' => 'hero', 'S' => 'story'];",
        "$map = self::ROW_METHODS[$type] ?? null;\n        return Rows::$map($p);",
    ));
    let fanouts: Vec<(&str, &[String], &str)> = named
        .fanout_refs
        .iter()
        .map(|f| {
            (
                f.source_name.as_str(),
                f.candidates.as_slice(),
                f.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(fanouts.len(), 1, "{:?}", named.fanout_refs);
    assert_eq!(fanouts[0].0, "dispatch");
    assert_eq!(fanouts[0].1, ["Rows.hero", "Rows.story"]);
    assert_eq!(fanouts[0].2, "php-const-map-dispatch");
    assert!(blind_kinds(&named).is_empty(), "{:?}", named.blind_spots);

    let late = parse(&api(
        "protected const MAP = ['x' => 'a', 'y' => 'b'];",
        "$m = static::MAP[$type];\n        return static::$m($p);",
    ));
    assert_eq!(late.fanout_refs.len(), 1, "{:?}", late.fanout_refs);
    assert_eq!(late.fanout_refs[0].candidates, ["Api.a", "Api.b"]);
    assert_eq!(blind_kinds(&late), ["php-variable-method-call"]);
}
