//! FU-2026-10-30-c0bc9a616111 part A, TypeScript: a method returning `this`
//! returns its own class, so `ecp impact` on the class lists it. `this` is
//! no type name: it binds through the owner, never through a project type
//! spelled `this`.
use crate::name_ref_support;

use ecp_analyzer::typescript::parser::TypeScriptProvider;
use name_ref_support::{annotated, graph};

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

/// A free function has no declaring class, so `: this` binds nothing; the
/// `this: Resolver` parameter binds `Resolver` once.
#[test]
fn test_this_return_annotation_outside_class_binds_only_the_this_parameter() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "src/free.ts",
            "export class Resolver { n = 0; }\nexport function free(this: Resolver): this { return this; }\n",
        )],
    );
    assert_eq!(annotated(&g, "free"), [pair("src/free.ts", "Resolver")]);
}

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// The TypeScript query captures only a bare `type_identifier` or
/// `this_type` under the return `type_annotation`. `Tok | null` and
/// `Array<Tok>` are not captured; their text comes from the function meta,
/// and both bind `Tok`.
#[test]
fn test_union_and_generic_returns_bind_through_the_function_meta() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "src/app.ts",
            "export class Tok {}\nexport function plain(): Tok { return new Tok(); }\nexport function find(): Tok | null { return null; }\nexport function all(): Array<Tok> { return []; }\n",
        )],
    );
    assert_eq!(annotated(&g, "plain"), [pair("src/app.ts", "Tok")]);
    assert_eq!(annotated(&g, "find"), [pair("src/app.ts", "Tok")]);
    assert_eq!(annotated(&g, "all"), [pair("src/app.ts", "Tok")]);
}

/// Contract: a class field typed with a project class reaches it, like a
/// parameter does. An interface property makes no node (ref-gitnexus
/// parity, `interface_property_signature_not_emitted`), so it gets no edge. A builtin
/// type, a type parameter and an untyped field bind nothing.
#[test]
fn test_class_field_annotation_binds_project_type_and_interface_property_makes_no_node() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "src/app.ts",
            "export class St { x = 1; }\n\
             export interface I { st_prop: St; n: number }\n\
             export class C<T> {\n  st_field: St;\n  st_init: St = new St();\n  tparam: T;\n  plain: number = 1;\n  untyped = 1;\n}\n",
        )],
    );
    let st = vec![pair("src/app.ts", "St")];
    assert_eq!(annotated(&g, "st_field"), st, "class field");
    assert_eq!(annotated(&g, "st_init"), st, "class field with initializer");
    let none = Vec::<(String, String)>::new();
    assert_eq!(
        annotated(&g, "st_prop"),
        none,
        "interface property: no node"
    );
    assert_eq!(annotated(&g, "tparam"), none, "type parameter");
    assert_eq!(annotated(&g, "plain"), none, "builtin class field");
    assert_eq!(annotated(&g, "untyped"), none, "no annotation");
}

/// Contract: a nullable field binds its project type; a constructor
/// parameter property binds its type; a type-literal member is no declared
/// type's member and makes no node.
#[test]
fn test_nullable_field_and_parameter_property_bind_project_type() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "src/app.ts",
            "export class St {}\n\
             export class C {\n  opt: St | null = null;\n  constructor(private promoted: St, public n: number) {}\n}\n\
             export type Lit = { lit_member: St };\n",
        )],
    );
    let st = vec![pair("src/app.ts", "St")];
    assert_eq!(annotated(&g, "opt"), st, "St | null");
    assert_eq!(annotated(&g, "promoted"), st, "parameter property");
    assert_eq!(annotated(&g, "n"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "lit_member"), Vec::<(String, String)>::new());
}
