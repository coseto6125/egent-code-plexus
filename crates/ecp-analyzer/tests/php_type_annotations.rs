//! FU-2026-10-30-c0bc9a616111 part A for PHP: a `self` / `static` return
//! type stands for the class that owns the method.
use crate::name_ref_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::graph::ZeroCopyGraph;
use name_ref_support::{annotated, graph};

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

fn build(src: &str) -> ZeroCopyGraph {
    graph(&PhpProvider::new().unwrap(), &[("src/Resolver.php", src)])
}

/// Contract: `: self` and `: static` reach the class that declares the method.
#[test]
fn test_self_and_static_return_annotations_bind_owner_class() {
    let g = build(
        "<?php\nclass Resolver\n{\n\
         \x20   public function copy(): self { return $this; }\n\
         \x20   public static function make(): static { return new static(); }\n}\n",
    );
    assert_eq!(
        annotated(&g, "copy"),
        [pair("src/Resolver.php", "Resolver")]
    );
    assert_eq!(
        annotated(&g, "make"),
        [pair("src/Resolver.php", "Resolver")]
    );
}

/// Contract: a nullable `?self` keeps the class's identity.
#[test]
fn test_nullable_self_return_annotation_binds_owner_class() {
    let g = build(
        "<?php\nclass Resolver\n{\n\
         \x20   public function next(): ?self { return null; }\n}\n",
    );
    assert_eq!(
        annotated(&g, "next"),
        [pair("src/Resolver.php", "Resolver")]
    );
}

/// Contract: a method with no return type has no annotation edge.
#[test]
fn test_method_without_return_type_has_no_annotation_edge() {
    let g = build("<?php\nclass Resolver\n{\n    public function run() {}\n}\n");
    assert_eq!(annotated(&g, "run"), Vec::<(String, String)>::new());
}

/// Contract: a nullable `?Tok` return binds the project class `Tok`, like
/// `: Tok`. The parser keeps the `?` (the `?self` test above relies on it).
/// Before the change the raw text `?Tok` names no type: no edge for `find`.
/// With no project `Tok` it binds nothing. PHP has no generic return syntax
/// (`array` is the only collection type), so there is no generic case.
#[test]
fn test_nullable_return_annotation_binds_project_type() {
    let g = build(
        "<?php\nclass Tok {}\nclass Owner\n{\n\
         \x20   public function find(): ?Tok { return null; }\n\
         \x20   public function make(): Tok { return new Tok(); }\n}\n",
    );
    assert_eq!(annotated(&g, "find"), [pair("src/Resolver.php", "Tok")]);
    assert_eq!(annotated(&g, "make"), [pair("src/Resolver.php", "Tok")]);

    let g = build("<?php\nclass Owner\n{\n    public function find(): ?Tok { return null; }\n}\n");
    assert_eq!(annotated(&g, "find"), Vec::<(String, String)>::new());
}

/// Contract: a typed property (plain, nullable) and a constructor-promoted
/// property reach the project class, like a parameter does. A builtin type,
/// an untyped property and a non-promoted constructor parameter bind
/// nothing to a property node.
#[test]
fn test_typed_property_annotation_binds_project_type() {
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[(
            "src/App.php",
            "<?php\nclass St {}\nclass C\n{\n\
             \x20   public St $st_field;\n\
             \x20   public ?St $opt;\n\
             \x20   public int $plain;\n\
             \x20   public $untyped;\n\
             \x20   public function __construct(private St $st_promo, public int $n, St $loose) {}\n}\n",
        )],
    );
    let st = vec![pair("src/App.php", "St")];
    assert_eq!(annotated(&g, "st_field"), st, "typed property");
    assert_eq!(annotated(&g, "opt"), st, "nullable property");
    assert_eq!(annotated(&g, "st_promo"), st, "promoted property");
    let none = Vec::<(String, String)>::new();
    assert_eq!(annotated(&g, "plain"), none, "builtin property");
    assert_eq!(annotated(&g, "untyped"), none, "no annotation");
    assert_eq!(annotated(&g, "n"), none, "builtin promoted property");
    assert_eq!(
        annotated(&g, "loose"),
        none,
        "plain parameter is no property"
    );
}
