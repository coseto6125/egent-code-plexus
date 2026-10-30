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
