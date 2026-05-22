//! Regression: PHP properties (NodeKind::Property) with the same name across
//! sibling classes must each receive a distinct `owner_class`.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("PhpProvider::new");
    p.parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse_file")
}

/// Two sibling PHP classes both declaring `$name` must emit with distinct owners.
#[test]
fn sibling_classes_same_property_distinct_owners() {
    let src = r#"<?php
class User {
    public string $name;
}
class Product {
    public string $name;
}
"#;
    let g = parse(src);
    let props: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property && n.name == "name")
        .collect();
    assert_eq!(props.len(), 2, "both `name` properties must be emitted");
    let owners: Vec<_> = props
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"User"),
        "one name must own User; owners={owners:?}"
    );
    assert!(
        owners.contains(&"Product"),
        "one name must own Product; owners={owners:?}"
    );
}

/// Laravel-style: sibling classes sharing a common property pattern.
#[test]
fn laravel_style_response_classes_same_property() {
    let src = r#"<?php
namespace Illuminate\Testing;
class TestResponse {
    protected $status;
}
class AssertableJson {
    protected $status;
}
"#;
    let g = parse(src);
    let props: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property && n.name == "status")
        .collect();
    assert_eq!(props.len(), 2, "both `status` properties must be emitted");
    let owners: Vec<_> = props
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"TestResponse"),
        "one status must own TestResponse; owners={owners:?}"
    );
    assert!(
        owners.contains(&"AssertableJson"),
        "one status must own AssertableJson; owners={owners:?}"
    );
}

/// Property in a single PHP class keeps its owner_class.
#[test]
fn single_class_property_has_owner() {
    let src = "<?php class Order { private int $total; }\n";
    let g = parse(src);
    let total = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Property && n.name == "total")
        .expect("total property");
    assert_eq!(
        total.owner_class.as_deref(),
        Some("Order"),
        "total.owner_class must be Order; got {:?}",
        total.owner_class
    );
}
