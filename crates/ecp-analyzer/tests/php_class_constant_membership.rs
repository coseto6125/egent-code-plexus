//! A PHP class constant is its class's member: the class links it with
//! `HasProperty`, and no `File -Defines->` edge claims it as top-level. A
//! module-level `const` keeps its `Defines` edge. Enum constants are members
//! too, so their enum links them with `HasProperty`.
mod name_ref_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use name_ref_support::graph;

/// `(source name, rel type)` of every edge into the node named `target`.
fn incoming(graph: &ZeroCopyGraph, target: &str) -> Vec<(String, RelType)> {
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<(String, RelType)> = graph
        .edges
        .iter()
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == target)
        .map(|e| {
            (
                graph.nodes[e.source as usize]
                    .name
                    .resolve(pool)
                    .to_string(),
                e.rel_type,
            )
        })
        .collect();
    out.sort_by_key(|(name, rel)| (name.clone(), format!("{rel:?}")));
    out
}

const SRC: &str = "<?php\nconst LIMIT = 3;\n\nclass Order {\n    const STATUS = 'x';\n}\n\ninterface Shape {\n    const SIDES = 4;\n}\n\nenum Suit: string {\n    case Hearts = 'H';\n    const WILD = 'W';\n}\n";
const ENUM_ONLY_SRC: &str =
    "<?php\nenum Suit: string {\n    case Hearts = 'H';\n    const WILD = 'W';\n}\n";
const DUPLICATE_SHORT_TYPE_NAMES: &str = "<?php\n\
namespace First { class Foo { const CLASS_VALUE = 1; } }\n\
namespace Second { interface Foo { const INTERFACE_VALUE = 2; } }\n";
const ANONYMOUS_CLASS_CONSTANT: &str = "<?php\n\
class Factory {\n\
    function make(): object { return new class { const INNER = 1; }; }\n\
}\n";

#[test]
fn test_class_membership_php_class_constant_returns_has_property_from_class() {
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", SRC)]);
    assert_eq!(
        incoming(&g, "STATUS"),
        vec![("Order".to_string(), RelType::HasProperty)]
    );
}

#[test]
fn test_class_membership_php_interface_constant_returns_has_property_from_interface() {
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", SRC)]);
    assert_eq!(
        incoming(&g, "SIDES"),
        vec![("Shape".to_string(), RelType::HasProperty)]
    );
}

#[test]
fn test_class_membership_php_module_constant_keeps_file_defines() {
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", SRC)]);
    assert_eq!(
        incoming(&g, "LIMIT"),
        vec![("p.php".to_string(), RelType::Defines)]
    );
}

#[test]
fn test_class_membership_php_enum_constant_returns_has_property_from_enum() {
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", ENUM_ONLY_SRC)]);
    assert_eq!(
        incoming(&g, "WILD"),
        vec![("Suit".to_string(), RelType::HasProperty)]
    );
}

#[test]
fn test_class_membership_php_ambiguous_enum_constant_returns_no_property_edge() {
    let source = "<?php\nnamespace A { enum Suit { const WILD = 1; } }\nnamespace B { enum Suit { const WILD = 2; } }\n";
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", source)]);
    assert!(incoming(&g, "WILD")
        .iter()
        .all(|(_, rel)| *rel != RelType::HasProperty));
}

#[test]
fn test_class_membership_php_same_short_type_names_suppress_ambiguous_constant_membership() {
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[("p.php", DUPLICATE_SHORT_TYPE_NAMES)],
    );
    for constant in ["CLASS_VALUE", "INTERFACE_VALUE"] {
        assert!(
            incoming(&g, constant)
                .iter()
                .all(|(_, rel)| *rel != RelType::HasProperty),
            "{constant}: {g:#?}"
        );
    }
}

#[test]
fn test_class_membership_php_anonymous_constant_returns_no_outer_property_edge() {
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[("p.php", ANONYMOUS_CLASS_CONSTANT)],
    );
    assert!(
        incoming(&g, "INNER")
            .iter()
            .all(|(source, rel)| source != "Factory" || *rel != RelType::HasProperty),
        "anonymous class constant must not become a Factory member: {g:#?}"
    );
}

#[test]
fn test_class_membership_php_anonymous_constant_returns_no_global_read_edge() {
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[
            ("factory.php", ANONYMOUS_CLASS_CONSTANT),
            (
                "reader.php",
                "<?php function readInner(): int { return INNER; }",
            ),
        ],
    );
    assert!(name_ref_support::value_reads(&g, "readInner").is_empty());
}

#[test]
fn test_class_membership_php_distinct_anonymous_constants_keep_distinct_ids() {
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[("factory.php", "<?php function first() { return new class { const INNER = 1; }; } function second() { return new class { const INNER = 2; }; }")],
    );
    let constants: Vec<_> = g
        .nodes
        .iter()
        .filter(|node| node.name.resolve(&g.string_pool) == "INNER")
        .collect();
    assert_eq!(constants.len(), 2);
    assert_ne!(constants[0].uid, constants[1].uid);
    assert_ne!(constants[0].owner_class, constants[1].owner_class);
}
