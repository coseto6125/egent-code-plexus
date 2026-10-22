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
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", SRC)]);
    assert_eq!(
        incoming(&g, "WILD"),
        vec![("Suit".to_string(), RelType::HasProperty)]
    );
}
