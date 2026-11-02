//! JavaScript: plain JS has no return-type syntax, so `this` is a value there and a method returning it leaves no annotation edge.
mod name_ref_support;

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use name_ref_support::{graph, refs};

/// `(target file, target name)` of every `type_annotation` edge out of `source`.
fn annotated(g: &ZeroCopyGraph, source: &str) -> Vec<(String, String)> {
    refs(g, source, RelType::Accesses, "type_annotation")
        .into_iter()
        .map(|(file, name, _, _)| (file, name))
        .collect()
}

/// Guard: the JavaScript query captures no `@type`, so a method that returns
/// `this` carries no `type_annotation` edge, to the class or to anything else.
/// This pins the absence: the `this` arm of the I3 gate serves TypeScript only.
#[test]
fn test_this_return_value_emits_no_annotation_edge() {
    let g = graph(
        &JavaScriptProvider::new().unwrap(),
        &[(
            "src/table.js",
            "export class Resolver {\n  bump() { return this; }\n}\nexport class This {}\n",
        )],
    );
    assert_eq!(annotated(&g, "bump"), Vec::<(String, String)>::new());
}

/// Guard: a JSDoc `@returns {this}` comment is no annotation either.
#[test]
fn test_jsdoc_returns_this_emits_no_annotation_edge() {
    let g = graph(
        &JavaScriptProvider::new().unwrap(),
        &[(
            "src/table.js",
            "export class Resolver {\n  /** @returns {this} */\n  bump() { return this; }\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "bump"), Vec::<(String, String)>::new());
}
