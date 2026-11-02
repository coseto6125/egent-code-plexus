//! FU-2026-10-30-c0bc9a616111 part A, TypeScript: a method returning `this`
//! returns its own class, so `ecp impact` on the class lists it. `this` is
//! no type name: it binds through the owner, never through a project type
//! spelled `this`.
mod name_ref_support;

use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::graph::RelType;
use name_ref_support::{graph, refs};

const TYPE_ANNOTATION: &str = "type_annotation";

/// `(target file, target name)` of every `type_annotation` edge out of `source`.
fn annotated(g: &ecp_core::graph::ZeroCopyGraph, source: &str) -> Vec<(String, String)> {
    refs(g, source, RelType::Accesses, TYPE_ANNOTATION)
        .into_iter()
        .map(|(file, name, _, _)| (file, name))
        .collect()
}

/// Contract: `bump(): this` in `class Resolver` reaches `Resolver`, like
/// `bump(): Resolver`. Before the fix the query captured only a
/// `type_identifier`, so `this` left no annotation: no edge.
#[test]
fn test_this_return_annotation_binds_declaring_class() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "src/table.ts",
            "export class Resolver {\n  n = 0;\n  bump(): this { return this; }\n  plain(): Resolver { return this; }\n}\n",
        )],
    );
    let resolver = vec![("src/table.ts".to_string(), "Resolver".to_string())];
    assert_eq!(annotated(&g, "bump"), resolver, "bump: this");
    assert_eq!(annotated(&g, "plain"), resolver, "plain: Resolver");
}

/// Guard: a free function has no declaring class, so `: this` binds nothing.
#[test]
fn test_this_return_annotation_outside_class_no_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "src/free.ts",
            "export class Resolver { n = 0; }\nexport function free(this: Resolver): this { return this; }\n",
        )],
    );
    assert_eq!(annotated(&g, "free"), Vec::<(String, String)>::new());
}
