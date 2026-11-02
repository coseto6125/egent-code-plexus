//! Go has no constructors: the `NewA()` factory idiom is an ordinary
//! function call. A composite literal `A{}` constructs the struct, so it
//! calls the `Struct` node (FU-2026-10-30-c0bc9a616111): `ecp impact` on the
//! struct lists the literal sites.

mod instantiation_calls_support;
mod name_ref_support;

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::graph::{NodeKind, ZeroCopyGraph};
use instantiation_calls_support::{
    assert_accesses_type, assert_calls_type, assert_only_call, graph_of,
};
use name_ref_support::{calls, graph};

const WIDGET: (&str, &str) = (
    "pkg/widget.go",
    "package shop\n\ntype Widget struct {\n\tX int\n}\n\nfunc NewWidget(x int) *Widget {\n\treturn &Widget{X: x}\n}\n",
);

fn provider() -> GoProvider {
    GoProvider::new().expect("GoProvider::new")
}

/// The `Accesses` edge comes from the declared return type; the literal
/// adds one `Calls` edge into the struct.
#[test]
fn test_go_cross_file_composite_literal_calls_struct() {
    let app = (
        "pkg/app.go",
        "package shop\n\nfunc MakeLiteral() Widget {\n\treturn Widget{X: 1}\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_type(
        &graph,
        "MakeLiteral",
        "Widget",
        NodeKind::Struct,
        "pkg/widget.go",
    );
    assert_accesses_type(&graph, "MakeLiteral", "Widget");
}

#[test]
fn test_go_same_file_composite_literal_calls_struct() {
    let local = (
        "pkg/local.go",
        "package shop\n\ntype Local struct {\n\tReady bool\n}\n\nfunc MakeLocal() Local {\n\treturn Local{Ready: true}\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_type(
        &graph,
        "MakeLocal",
        "Local",
        NodeKind::Struct,
        "pkg/local.go",
    );
}

#[test]
fn test_go_factory_function_call_targets_function() {
    let app = (
        "pkg/app.go",
        "package shop\n\nfunc MakeViaFactory() *Widget {\n\treturn NewWidget(1)\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_only_call(&graph, "MakeViaFactory", "NewWidget", "pkg/widget.go");
}

// FU-2026-10-30-c0bc9a616111 part B: a Go composite literal of a named
// type constructs it, so `ecp impact` on the struct lists the functions that
// build it. A slice, map or array literal names no type.

const FOO: &str = "package p\n\
                   type Foo struct{ n int }\n\
                   type Box[T any] struct{ v T }\n\
                   func NewFoo() *Foo { return &Foo{n: 1} }\n";

fn build(user: &str) -> ZeroCopyGraph {
    graph(
        &GoProvider::new().unwrap(),
        &[("p/foo.go", FOO), ("p/use.go", user)],
    )
}

fn called(g: &ZeroCopyGraph, caller: &str, name: &str) -> bool {
    calls(g, caller)
        .iter()
        .any(|(file, callee)| file == "p/foo.go" && callee == name)
}

/// Contract: `Foo{n: 2}` is a `Calls` edge from the builder to `Foo`.
#[test]
fn test_composite_literal_binds_struct() {
    let g = build("package p\nfunc UseLit() int { return Foo{n: 2}.n }\n");
    assert!(called(&g, "UseLit", "Foo"), "{:?}", calls(&g, "UseLit"));
}

/// Contract: `&Foo{}` constructs `Foo`, in the file that declares it too.
#[test]
fn test_address_of_literal_binds_struct() {
    let g = build("package p\n");
    assert!(called(&g, "NewFoo", "Foo"), "{:?}", calls(&g, "NewFoo"));
}

/// Contract: slice, map and array literals name no type.
#[test]
fn test_slice_map_and_array_literals_bind_nothing() {
    let g =
        build("package p\nfunc Plain() { _ = []int{1}; _ = map[string]int{}; _ = [2]int{1, 2} }\n");
    assert_eq!(calls(&g, "Plain"), Vec::<(String, String)>::new());
}

/// Contract: a literal of a type the project does not declare binds nothing.
#[test]
fn test_literal_of_undeclared_type_binds_nothing() {
    let g = build("package p\nfunc Missing() { _ = Nope{n: 1} }\n");
    assert_eq!(calls(&g, "Missing"), Vec::<(String, String)>::new());
}

/// Contract: a generic instantiation `Box[int]{}` is skipped, not bound to
/// a guessed type.
#[test]
fn test_generic_literal_binds_nothing() {
    let g = build("package p\nfunc Gen() { _ = Box[int]{v: 1} }\n");
    assert_eq!(calls(&g, "Gen"), Vec::<(String, String)>::new());
}
