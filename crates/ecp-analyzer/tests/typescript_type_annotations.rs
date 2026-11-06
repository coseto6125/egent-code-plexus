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

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::typescript::parser::TypeScriptProvider;
    use ecp_core::graph::ZeroCopyGraph;

    fn pair(file: &str, name: &str) -> (String, String) {
        (file.to_string(), name.to_string())
    }

    /// Target names of every type-annotation edge out of `source`.
    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    fn token_file(token: &str) -> String {
        format!("src/{token}.ts")
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let _ = (&arg, &params);
        let import = if generic.is_none() {
            format!("import {{ {token} }} from './{token}';\n")
        } else {
            String::new()
        };
        let files = vec![
            (token_file(token), format!("export class {token} {{}}\n")),
            (
                "src/box.ts".to_string(),
                format!("{import}export class Box{params} {{\n  items: Array<{arg}> = [];\n}}\n"),
            ),
            (
                "src/holder.ts".to_string(),
                format!("import {{ {token} }} from './{token}';\nexport class Holder {{\n  tokens: Array<{token}> = [];\n}}\n"),
            ),
        ];
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        graph(&TypeScriptProvider::new().unwrap(), &refs)
    }

    /// Stands for: a field whose type argument names a class type parameter
    /// binds no project type, although a project class has the same name; the
    /// sibling field in an unparameterised holder (POSITIVE CONTROL) binds it.
    #[test]
    fn test_field_annotation_class_type_parameter_named_like_project_class_binds_nothing() {
        let g = build(Some("Token"), "Token");
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "items binds a type parameter as a project class: {:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for: a generic argument in a holder with no type parameters
    /// binds the project class it names.
    #[test]
    fn test_field_annotation_project_class_in_unparameterised_holder_binds_class() {
        let g = build(Some("Token"), "Token");
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "{:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, rename the parameter): `Box<E>` / `List<E>` binds no
    /// project edge, and `tokens` still binds `Token`.
    #[test]
    fn test_field_annotation_renamed_class_type_parameter_binds_nothing() {
        let g = build(Some("E"), "Token");
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, rename the project class): the project class is `Tok`
    /// and the parameter stays `Token`; `items` binds neither, `tokens` binds `Tok`.
    #[test]
    fn test_field_annotation_renamed_project_class_keeps_parameter_binding_nothing() {
        let g = build(Some("Token"), "Tok");
        let items = names(&g, "items");
        assert!(
            !items.iter().any(|n| n == "Tok" || n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Tok"), "Tok")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, remove the parameter): with no class type parameter
    /// the same field binds the project class `Token`.
    #[test]
    fn test_field_annotation_without_class_type_parameter_binds_project_class() {
        let g = build(None, "Token");
        assert!(
            annotated(&g, "items").contains(&pair(&token_file("Token"), "Token")),
            "{:?}",
            annotated(&g, "items")
        );
    }

    fn method_files() -> Vec<(String, String)> {
        vec![
            (token_file("Token"), "export class Token {}\n".to_string()),
            (
                "src/box.ts".to_string(),
                "import { Token } from './Token';\nexport class Box {\n  put<Token>(xs: Array<Token>): void {}\n}\n".to_string(),
            ),
            (
                "src/holder.ts".to_string(),
                "import { Token } from './Token';\nexport class Holder {\n  take(xs: Array<Token>): void {}\n}\n".to_string(),
            ),
        ]
    }

    /// Stands for: a method type parameter named like a project class binds
    /// nothing for `put`; `take`, declared where no such parameter exists
    /// (POSITIVE CONTROL), binds the project class.
    #[test]
    fn test_field_annotation_method_type_parameter_named_like_project_class_binds_nothing() {
        let files = method_files();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        let g = graph(&TypeScriptProvider::new().unwrap(), &refs);
        assert!(
            !names(&g, "put").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "put")
        );
        assert!(
            annotated(&g, "take").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "take")
        );
    }
}

/// A15a: interface property signatures are Property nodes, so they carry
/// annotation edges and never take a class receiver's field read.
mod a15a {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::typescript::parser::TypeScriptProvider;
    use ecp_core::graph::{RelType, ZeroCopyGraph};

    const SRC: &str = "export class Owner {}\nexport interface Cat {\n  readonly id: number;\n  name: string;\n  owner: Owner;\n  meow(): void;\n}\nexport class Dog {\n  name: string = '';\n}\nexport function readName(d: Dog, c: Cat) {\n  return d.name;\n}\n";

    fn build() -> ZeroCopyGraph {
        graph(&TypeScriptProvider::new().unwrap(), &[("cat.ts", SRC)])
    }

    /// Owner classes of every `name` Property that `readName` reads.
    fn read_owners(g: &ZeroCopyGraph) -> Vec<String> {
        let pool = g.string_pool.as_slice();
        g.edges
            .iter()
            .filter(|e| e.rel_type == RelType::ReadsField)
            .filter(|e| g.nodes[e.source as usize].name.resolve(pool) == "readName")
            .map(|e| &g.nodes[e.target as usize])
            .filter(|t| t.name.resolve(pool) == "name")
            .map(|t| t.owner_class.resolve(pool).to_string())
            .collect()
    }

    /// Stands for: an interface property's annotation binds its project type,
    /// like a class field's.
    #[test]
    fn test_field_annotation_typescript_interface_property_binds_project_type() {
        let g = build();
        assert_eq!(
            annotated(&g, "owner"),
            vec![("cat.ts".to_string(), "Owner".to_string())]
        );
    }

    /// Stands for: `d.name` with `d: Dog` reads `Dog.name` and never the
    /// namesake `Cat.name` of the interface. POSITIVE CONTROL: the `Dog.name`
    /// edge exists.
    #[test]
    fn test_reads_field_typescript_class_receiver_ignores_interface_namesake_property() {
        let g = build();
        let owners = read_owners(&g);
        assert!(owners.iter().any(|o| o == "Dog"), "control: {owners:?}");
        assert!(!owners.iter().any(|o| o == "Cat"), "{owners:?}");
    }
}
