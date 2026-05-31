//! In-class trait composition edges for the PHP parser.
//!
//! `use TraitName;` inside a class body must produce a heritage entry on the
//! class node — the same mechanism as `extends` / `implements`. Without this,
//! `ecp impact --target <trait>` misses every class that composes the trait.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("provider");
    p.parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse")
}

fn class_node(g: &LocalGraph, name: &str) -> ecp_core::analyzer::types::RawNode {
    g.nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == name)
        .unwrap_or_else(|| panic!("no Class node named {name} in {:#?}", g.nodes))
        .clone()
}

// ── Single trait use ──────────────────────────────────────────────────────────

#[test]
fn single_trait_use_adds_heritage() {
    let src = "<?php trait Bar {} class Foo { use Bar; }";
    let g = parse(src);
    let foo = class_node(&g, "Foo");
    assert!(
        foo.heritage.iter().any(|h| h == "Bar"),
        "expected 'Bar' in Foo's heritage; got: {:?}",
        foo.heritage
    );
}

// ── Multi-trait use (`use A, B;`) ─────────────────────────────────────────────

#[test]
fn multi_trait_use_adds_all_traits_to_heritage() {
    let src = "<?php trait A {} trait B {} class Foo { use A, B; }";
    let g = parse(src);
    let foo = class_node(&g, "Foo");
    assert!(
        foo.heritage.iter().any(|h| h == "A"),
        "expected 'A' in Foo's heritage; got: {:?}",
        foo.heritage
    );
    assert!(
        foo.heritage.iter().any(|h| h == "B"),
        "expected 'B' in Foo's heritage; got: {:?}",
        foo.heritage
    );
}

// ── Multiple separate use statements ─────────────────────────────────────────

#[test]
fn separate_use_statements_accumulate_heritage() {
    let src = "<?php trait A {} trait B {} class Foo { use A; use B; }";
    let g = parse(src);
    let foo = class_node(&g, "Foo");
    assert!(
        foo.heritage.iter().any(|h| h == "A"),
        "expected 'A' in Foo's heritage; got: {:?}",
        foo.heritage
    );
    assert!(
        foo.heritage.iter().any(|h| h == "B"),
        "expected 'B' in Foo's heritage; got: {:?}",
        foo.heritage
    );
}

// ── Trait use combined with extends ──────────────────────────────────────────

#[test]
fn trait_use_alongside_extends_adds_both_to_heritage() {
    let src =
        "<?php trait Serializable {} class Base {} class Child extends Base { use Serializable; }";
    let g = parse(src);
    let child = class_node(&g, "Child");
    assert!(
        child.heritage.iter().any(|h| h == "Base"),
        "expected 'Base' in Child's heritage; got: {:?}",
        child.heritage
    );
    assert!(
        child.heritage.iter().any(|h| h == "Serializable"),
        "expected 'Serializable' in Child's heritage; got: {:?}",
        child.heritage
    );
}

// ── Guard: file-level namespace import must NOT become a heritage edge ────────

#[test]
fn file_level_use_import_is_not_heritage() {
    // `use Some\Namespace\Thing;` is a namespace_use_declaration — structurally
    // different from `use_declaration` inside a class body. It must not leak
    // into any class's heritage list.
    let src = r#"<?php
use App\Http\Controllers\Controller;
use Illuminate\Support\Facades\Auth;
class Foo {}
"#;
    let g = parse(src);
    let foo = class_node(&g, "Foo");
    assert!(
        foo.heritage.is_empty(),
        "file-level imports must not appear in class heritage; got: {:?}",
        foo.heritage
    );
}

// ── Adaptation block form does not crash ─────────────────────────────────────

#[test]
fn trait_use_adaptation_block_does_not_crash() {
    // `use A { A::hello insteadof B; }` — the use_list child contains
    // conflict-resolution clauses, not trait names. The query must not
    // pick up the clause names as trait heritage and must not panic.
    let src = r#"<?php
trait A { public function hello() {} }
trait B { public function hello() {} }
class Foo {
    use A, B {
        A::hello insteadof B;
    }
}
"#;
    let g = parse(src);
    // Must not panic. A and B are present in the `use A, B {` line which
    // the grammar still emits as name children before the use_list block.
    let foo = class_node(&g, "Foo");
    // A and B should appear from the `use A, B` prefix of the declaration.
    assert!(
        foo.heritage.iter().any(|h| h == "A"),
        "expected 'A' in Foo's heritage from adaptation-block form; got: {:?}",
        foo.heritage
    );
}
