//! FU-2026-10-30-c0bc9a616111 part A for PHP: a `self` / `static` return
//! type stands for the class that owns the method.
mod name_ref_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use name_ref_support::{graph, refs};

fn annotated(g: &ZeroCopyGraph, source: &str) -> Vec<(String, String)> {
    refs(g, source, RelType::Accesses, "type_annotation")
        .into_iter()
        .map(|(file, name, _, _)| (file, name))
        .collect()
}

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
