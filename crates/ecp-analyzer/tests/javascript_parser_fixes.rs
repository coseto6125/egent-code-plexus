//! Parser fixes for JavaScript: Express handler anchoring, CommonJS exports.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &[u8]) -> LocalGraph {
    JavaScriptProvider::new()
        .expect("JavaScriptProvider::new")
        .parse_file(Path::new("a.js"), src)
        .expect("parse_file")
}

fn express_targets(g: &LocalGraph) -> Vec<&str> {
    g.framework_refs
        .iter()
        .filter(|r| r.reason == "express-route")
        .map(|r| r.target_name.as_str())
        .collect()
}

fn exported(g: &LocalGraph, name: &str) -> bool {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == NodeKind::Function)
        .unwrap_or_else(|| panic!("no function {name}: {:?}", g.nodes))
        .is_exported
}

const EXPRESS_HEAD: &str = "const express = require('express');\nconst app = express();\n";

/// FU-2026-10-30-855c06cfb700. Express runs the last argument as the
/// handler: middleware before it gets no express-route ref.
#[test]
fn test_javascript_express_middleware_identifier_gets_no_route_ref() {
    let src = format!("{EXPRESS_HEAD}app.get('/users', auth, listUsers);\n");
    assert_eq!(express_targets(&parse(src.as_bytes())), ["listUsers"]);
}

/// FU-2026-10-30-855c06cfb700. Several middleware and a member-access
/// handler: only the last argument is a handler.
#[test]
fn test_javascript_express_member_handler_after_two_middleware_is_sole_ref() {
    let src = format!("{EXPRESS_HEAD}app.post('/u', auth, validate, ctrl.create);\n");
    assert_eq!(express_targets(&parse(src.as_bytes())), ["ctrl.create"]);
}

/// FU-2026-10-30-855c06cfb700. An inline handler after middleware is the
/// only ref, as the `<anonymous>` target.
#[test]
fn test_javascript_express_inline_handler_after_middleware_is_sole_ref() {
    let src = format!("{EXPRESS_HEAD}app.get('/u', auth, (req, res) => {{ res.send(1); }});\n");
    assert_eq!(express_targets(&parse(src.as_bytes())), ["<anonymous>"]);
}

/// FU-2026-10-30-855c06cfb700 guard: a route with a single handler keeps
/// its one ref.
#[test]
fn test_javascript_express_single_handler_keeps_one_ref() {
    let src = format!("{EXPRESS_HEAD}app.get('/u', listUsers);\n");
    assert_eq!(express_targets(&parse(src.as_bytes())), ["listUsers"]);
}

/// FU-2026-10-30-855c06cfb700. `module.exports = name` exports that
/// declaration and no other.
#[test]
fn test_javascript_commonjs_module_exports_identifier_marks_declaration() {
    let g = parse(b"function a() {}\nfunction b() {}\nmodule.exports = a;\n");
    assert!(exported(&g, "a"));
    assert!(!exported(&g, "b"));
}

/// FU-2026-10-30-855c06cfb700. `module.exports = { a, k: b }` exports the
/// shorthand and the renamed value, not an unlisted declaration.
#[test]
fn test_javascript_commonjs_module_exports_object_marks_listed_declarations() {
    let g = parse(
        b"function a() {}\nfunction b() {}\nfunction c() {}\nmodule.exports = { a, k: b };\n",
    );
    assert!(exported(&g, "a"));
    assert!(exported(&g, "b"));
    assert!(!exported(&g, "c"));
}

/// FU-2026-10-30-855c06cfb700. `exports.x = name` and
/// `module.exports.y = name` export the assigned declaration.
#[test]
fn test_javascript_commonjs_property_assignment_marks_declaration() {
    let g = parse(b"function a() {}\nfunction b() {}\nfunction c() {}\nexports.a = a;\nmodule.exports.b = b;\n");
    assert!(exported(&g, "a"));
    assert!(exported(&g, "b"));
    assert!(!exported(&g, "c"));
}

/// FU-2026-10-30-855c06cfb700 guard: an `exports` property of another
/// object and an assignment inside a function body export nothing.
#[test]
fn test_javascript_non_module_exports_assignment_marks_nothing() {
    let g = parse(b"function a() {}\nfunction b() {}\nother.exports = a;\nfunction init() { module.exports = b; }\n");
    assert!(!exported(&g, "a"));
    assert!(!exported(&g, "b"));
}

/// FU-2026-10-30-855c06cfb700 guard: ES `export` still marks its declaration.
#[test]
fn test_javascript_es_export_still_marks_declaration() {
    let g = parse(b"export function a() {}\nfunction b() {}\n");
    assert!(exported(&g, "a"));
    assert!(!exported(&g, "b"));
}

// FU-2026-10-30 round 2, item C3. Shape from express `lib/express.js:27`:
// a chained assignment exports the declaration its last value names.
#[test]
fn test_javascript_commonjs_exports_then_module_exports_chain_marks_declaration() {
    let g = parse(b"function createApplication() {}\nfunction other() {}\nexports = module.exports = createApplication;\n");
    assert!(exported(&g, "createApplication"));
    assert!(!exported(&g, "other"));
}

// C3 atypical: `module.exports` on the left, `exports` in the middle.
#[test]
fn test_javascript_commonjs_module_exports_then_exports_chain_marks_declaration() {
    let g = parse(b"function a() {}\nfunction other() {}\nmodule.exports = exports = a;\n");
    assert!(exported(&g, "a"));
    assert!(!exported(&g, "other"));
}

// C3 atypical: three levels, with `module.exports` in the middle.
#[test]
fn test_javascript_commonjs_three_level_chain_marks_declaration() {
    let g =
        parse(b"var p, q;\nfunction a() {}\nfunction other() {}\np = module.exports = q = a;\n");
    assert!(exported(&g, "a"));
    assert!(!exported(&g, "other"));
}

// C3 atypical: two property targets share one value.
#[test]
fn test_javascript_commonjs_chained_property_targets_mark_declaration() {
    let g = parse(b"function a() {}\nfunction other() {}\nexports.k = module.exports.k = a;\n");
    assert!(exported(&g, "a"));
    assert!(!exported(&g, "other"));
}

// C3 atypical: a chain whose value is an object literal exports the
// listed declarations, as the direct form does.
#[test]
fn test_javascript_commonjs_chain_with_object_value_marks_listed_declarations() {
    let g = parse(
        b"function a() {}\nfunction b() {}\nfunction c() {}\nexports = module.exports = { a, k: b };\n",
    );
    assert!(exported(&g, "a"));
    assert!(exported(&g, "b"));
    assert!(!exported(&g, "c"));
}

// C3 atypical: a chain whose value names no declaration exports nothing.
#[test]
fn test_javascript_commonjs_chain_with_function_expression_marks_nothing() {
    let g = parse(b"function a() {}\nmodule.exports = exports = function () {};\n");
    assert!(!exported(&g, "a"));
}

// C3 guard: a chain with no `exports` target stays unexported.
#[test]
fn test_javascript_non_export_chained_assignment_marks_nothing() {
    let g = parse(b"var p, q;\nfunction a() {}\np = q = a;\nother.exports = module.other = a;\n");
    assert!(!exported(&g, "a"));
}

// C3 guard: a bare `exports = a` rebinds the local `exports`; Node still
// exports the old `module.exports`, so nothing is exported.
#[test]
fn test_javascript_bare_exports_reassignment_marks_nothing() {
    let g = parse(b"function a() {}\nexports = a;\n");
    assert!(!exported(&g, "a"));
}

// C3 guard: a chain inside a function body is no top-level export.
#[test]
fn test_javascript_chained_exports_inside_function_body_marks_nothing() {
    let g = parse(b"function a() {}\nfunction init() { exports = module.exports = a; }\n");
    assert!(!exported(&g, "a"));
}

// C3 guard: an empty file has no nodes.
#[test]
fn test_javascript_empty_file_has_no_nodes() {
    assert!(parse(b"").nodes.is_empty());
}

fn exported_by_name(g: &LocalGraph, name: &str) -> bool {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node {name}: {:?}", g.nodes))
        .is_exported
}

// Round 3, C-R5. Shape from express `lib/application.js:40`: a declaration
// whose value is an export chain names the exported object.
#[test]
fn test_javascript_declaration_with_export_chain_value_marks_declared_name() {
    let g = parse(b"var app = exports = module.exports = {};\nfunction other() {}\n");
    assert!(exported_by_name(&g, "app"));
    assert!(!exported(&g, "other"));
}

// C-R5 atypical: a `let` declaration, and a chain whose value is a declared
// function: the value is exported as well.
#[test]
fn test_javascript_let_with_export_chain_of_identifier_marks_both_names() {
    let g = parse(b"function make() {}\nlet app = module.exports = make;\nfunction other() {}\n");
    assert!(exported_by_name(&g, "app"));
    assert!(exported(&g, "make"));
    assert!(!exported(&g, "other"));
}

// C-R5 atypical: two declarators side by side; only the chained one exports.
#[test]
fn test_javascript_second_declarator_with_export_chain_marks_only_that_name() {
    let g = parse(b"var a = 1, app = module.exports = {};\n");
    assert!(exported_by_name(&g, "app"));
    assert!(!exported_by_name(&g, "a"));
}

// C-R5 guard: chains with no `module.exports` target export no declared name.
#[test]
fn test_javascript_declaration_with_other_chain_value_marks_nothing() {
    let g = parse(b"var a = b = 5;\nvar app = exports = {};\nvar p = other.exports = {};\n");
    assert!(!exported_by_name(&g, "a"));
    assert!(!exported_by_name(&g, "app"));
    assert!(!exported_by_name(&g, "p"));
}

/// A whole `module.exports = X` replaces the earlier one: Node exports only
/// the last assigned value.
#[test]
fn test_javascript_commonjs_module_exports_reassigned_exports_last_value_only() {
    let g = parse(b"function obsolete() {}\nfunction current() {}\nmodule.exports = obsolete;\nmodule.exports = current;\n");
    assert!(!exported(&g, "obsolete"));
    assert!(exported(&g, "current"));
}

/// A whole `module.exports = X` also drops the property exports made before it.
#[test]
fn test_javascript_commonjs_module_exports_reassigned_drops_earlier_property_exports() {
    let g = parse(b"function a() {}\nfunction b() {}\nexports.a = a;\nmodule.exports = b;\n");
    assert!(!exported(&g, "a"));
    assert!(exported(&g, "b"));
}

/// A `module.exports.k = f` after a whole `module.exports = {...}` adds to the
/// replaced state; both stay exported.
#[test]
fn test_javascript_commonjs_module_exports_object_then_property_keeps_both() {
    let g = parse(
        b"function a() {}\nfunction b() {}\nmodule.exports = { a };\nmodule.exports.b = b;\n",
    );
    assert!(exported(&g, "a"));
    assert!(exported(&g, "b"));
}

/// An object reassignment replaces an earlier identifier export.
#[test]
fn test_javascript_commonjs_module_exports_object_replaces_earlier_identifier() {
    let g =
        parse(b"function a() {}\nfunction b() {}\nmodule.exports = a;\nmodule.exports = { b };\n");
    assert!(!exported(&g, "a"));
    assert!(exported(&g, "b"));
}

/// A replacement by a value that names no declaration leaves nothing exported.
#[test]
fn test_javascript_commonjs_module_exports_reassigned_to_literal_exports_nothing() {
    let g = parse(b"function a() {}\nmodule.exports = a;\nmodule.exports = 42;\n");
    assert!(!exported(&g, "a"));
}

/// A replacement inside a function body is not top level and replaces nothing.
#[test]
fn test_javascript_commonjs_nested_module_exports_reassignment_keeps_top_level_export() {
    let g = parse(b"function a() {}\nfunction b() {}\nmodule.exports = a;\nfunction init() { module.exports = b; }\n");
    assert!(exported(&g, "a"));
    assert!(!exported(&g, "b"));
}

/// After `exports = {}` the name `exports` no longer aliases `module.exports`,
/// so `exports.x = f` exports nothing.
#[test]
fn test_javascript_commonjs_exports_rebound_then_property_marks_nothing() {
    let g =
        parse(b"function privateHandler() {}\nexports = {};\nexports.handler = privateHandler;\n");
    assert!(!exported(&g, "privateHandler"));
}

/// `module.exports.x = f` still exports after `exports` is rebound.
#[test]
fn test_javascript_commonjs_exports_rebound_then_module_exports_property_still_exports() {
    let g = parse(b"function h() {}\nexports = {};\nmodule.exports.handler = h;\n");
    assert!(exported(&g, "h"));
}

/// `exports = module.exports = {}` keeps the alias, so `exports.x = f` exports.
#[test]
fn test_javascript_commonjs_exports_rebound_through_chain_keeps_alias() {
    let g = parse(b"function h() {}\nexports = module.exports = {};\nexports.handler = h;\n");
    assert!(exported(&g, "h"));
}

/// `exports = module.exports` restores the alias after a rebinding.
#[test]
fn test_javascript_commonjs_exports_realiased_to_module_exports_exports_again() {
    let g =
        parse(b"function h() {}\nexports = {};\nexports = module.exports;\nexports.handler = h;\n");
    assert!(exported(&g, "h"));
}

/// CRLF line ends do not change the source-order fold.
#[test]
fn test_javascript_commonjs_module_exports_reassigned_with_crlf_exports_last_only() {
    let g = parse(
        b"function a() {}\r\nfunction b() {}\r\nmodule.exports = a;\r\nmodule.exports = b;\r\n",
    );
    assert!(!exported(&g, "a"));
    assert!(exported(&g, "b"));
}

/// After a lone `module.exports = a`, `exports` still names the old object,
/// so `exports.b = b` exports nothing.
#[test]
fn test_javascript_commonjs_exports_property_after_whole_module_exports_marks_nothing() {
    let g = parse(b"function a() {}\nfunction b() {}\nmodule.exports = a;\nexports.b = b;\n");
    assert!(exported(&g, "a"));
    assert!(!exported(&g, "b"));
}

/// `module.exports.b = b` after `module.exports = a` still exports both.
#[test]
fn test_javascript_commonjs_module_exports_property_after_whole_module_exports_keeps_both() {
    let g =
        parse(b"function a() {}\nfunction b() {}\nmodule.exports = a;\nmodule.exports.b = b;\n");
    assert!(exported(&g, "a"));
    assert!(exported(&g, "b"));
}
