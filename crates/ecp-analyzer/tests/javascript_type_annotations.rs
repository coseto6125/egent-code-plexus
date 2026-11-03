//! JavaScript: plain JS has no return-type syntax, so `this` is a value there and a method returning it leaves no annotation edge.
use crate::name_ref_support;

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use name_ref_support::{annotated, graph};

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

/// Guard: JavaScript has no type syntax, so a nullable-looking construct (a
/// JSDoc `{?Tok}` / `{Tok|null}` return and a ternary returning `null`) emits
/// no `type_annotation` edge to the class `Tok`, before and after the change.
#[test]
fn test_jsdoc_nullable_return_emits_no_annotation_edge() {
    let g = graph(
        &JavaScriptProvider::new().unwrap(),
        &[(
            "src/app.js",
            "export class Tok {}\n/** @returns {?Tok} */\nexport function maybe(x) { return x ? new Tok() : null; }\n/** @returns {Tok|null} */\nexport function union(x) { return x ? new Tok() : null; }\n",
        )],
    );
    assert_eq!(annotated(&g, "maybe"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "union"), Vec::<(String, String)>::new());
}
