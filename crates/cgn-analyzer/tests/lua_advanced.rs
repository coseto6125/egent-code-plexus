//! Advanced Lua semantics: require alias, metatable-as-inheritance, and
//! table-assigned methods.
//!
//! Covers Wave 3 / Matrix B2 (Lua row) from
//! `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.

use cgn_analyzer::lua::parser::LuaProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::{RawImport, RawNode};
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> (Vec<RawNode>, Vec<RawImport>) {
    let provider = LuaProvider::new().expect("LuaProvider init");
    let graph = provider
        .parse_file(Path::new("test.lua"), src.as_bytes())
        .expect("parse_file");
    (graph.nodes, graph.imports)
}

fn find_import_by_source<'a>(imports: &'a [RawImport], source: &str) -> &'a RawImport {
    imports
        .iter()
        .find(|i| i.source == source)
        .unwrap_or_else(|| panic!("missing import with source `{source}` in {imports:#?}"))
}

fn find_node<'a>(nodes: &'a [RawNode], name: &str) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("missing node `{name}` in {nodes:#?}"))
}

#[test]
fn require_with_local_alias_captures_binding_name() {
    let src = r#"local M = require("foo")
"#;
    let (_nodes, imports) = parse(src);

    // The aliased pattern is the source of truth — there must be exactly
    // one import for `foo`, with `alias = Some("M")`.
    let foos: Vec<_> = imports.iter().filter(|i| i.source == "foo").collect();
    assert_eq!(
        foos.len(),
        1,
        "aliased `require` must emit exactly one import, got: {imports:#?}"
    );
    assert_eq!(foos[0].alias.as_deref(), Some("M"));
}

#[test]
fn direct_require_has_no_alias() {
    let src = r#"require("foo")
"#;
    let (_nodes, imports) = parse(src);

    let imp = find_import_by_source(&imports, "foo");
    assert!(
        imp.alias.is_none(),
        "bare `require(\"foo\")` must not synthesize an alias, got: {imp:#?}"
    );
}

#[test]
fn metatable_inheritance_appends_parent_to_child_heritage() {
    // `Cat` becomes a Class via the PascalCase heuristic on `local Cat = {}`.
    // `setmetatable(Cat, {__index = Animal})` then declares Cat inherits from
    // Animal — append `Animal` to Cat's heritage list.
    let src = r#"local Animal = {}
local Cat = {}
setmetatable(Cat, {__index = Animal})
"#;
    let (nodes, _imports) = parse(src);

    let cat = find_node(&nodes, "Cat");
    assert_eq!(cat.kind, NodeKind::Class, "Cat must be promoted to Class");
    assert!(
        cat.heritage.iter().any(|h| h == "Animal"),
        "Cat's heritage must contain `Animal`, got: {:?}",
        cat.heritage
    );
}

#[test]
fn method_colon_syntax_emits_function_node_for_method() {
    let src = r#"local M = {}
function M:greet()
end
"#;
    let (nodes, _imports) = parse(src);

    let greet = find_node(&nodes, "greet");
    // Colon syntax uses the existing `function_declaration` pattern, which
    // emits NodeKind::Function (the parser does not yet distinguish Method
    // vs Function for table-attached `function_declaration` forms).
    assert!(
        matches!(greet.kind, NodeKind::Function | NodeKind::Method),
        "`function M:greet()` must emit a Function or Method node, got: {:?}",
        greet.kind
    );
}

#[test]
fn method_dot_syntax_emits_function_node() {
    let src = r#"local M = {}
function M.foo()
end
"#;
    let (nodes, _imports) = parse(src);

    let foo = find_node(&nodes, "foo");
    assert!(
        matches!(foo.kind, NodeKind::Function | NodeKind::Method),
        "`function M.foo()` must emit a Function or Method node, got: {:?}",
        foo.kind
    );
}

#[test]
fn method_via_table_assignment_emits_method_node() {
    // `M.foo = function() end` was previously silent because the
    // assignment-with-function-value pattern only matched bare identifiers,
    // not dot_index_expressions. Pinning: the table-assigned form now emits
    // a Method-kinded node whose name is the field (`foo`).
    let src = r#"local M = {}
M.foo = function()
end
"#;
    let (nodes, _imports) = parse(src);

    let foo = find_node(&nodes, "foo");
    assert_eq!(
        foo.kind,
        NodeKind::Method,
        "`M.foo = function() end` must emit a Method node (got: {:?})",
        foo.kind
    );
}
