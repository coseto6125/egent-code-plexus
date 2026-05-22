//! Regression: Java fields (NodeKind::Property) with the same name in sibling
//! classes within the same file must each get a distinct `owner_class`.

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    provider
        .parse_file(Path::new("Test.java"), source.as_bytes())
        .expect("parse_file")
}

/// Two sibling classes both declaring a field `count` must each emit with
/// a distinct owner_class — otherwise uid = (Property, path, "", "count") collides.
#[test]
fn sibling_classes_same_field_name_distinct_owners() {
    let src = r#"
class Counter {
    int count;
}
class Accumulator {
    int count;
}
"#;
    let g = parse(src);
    let counts: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property && n.name == "count")
        .collect();
    assert_eq!(counts.len(), 2, "both `count` fields must be emitted");
    let owners: Vec<_> = counts
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"Counter"),
        "one count must own Counter; owners={owners:?}"
    );
    assert!(
        owners.contains(&"Accumulator"),
        "one count must own Accumulator; owners={owners:?}"
    );
}

/// ImmutableMultimap-style: multiple inner classes each with a field `size`.
#[test]
fn inner_classes_same_field_distinct_owners() {
    let src = r#"
public class ImmutableMultimap {
    static class EntrySet {
        int size;
    }
    static class Values {
        int size;
    }
}
"#;
    let g = parse(src);
    let size_fields: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property && n.name == "size")
        .collect();
    assert_eq!(
        size_fields.len(),
        2,
        "both `size` fields must be emitted: {size_fields:?}"
    );
    let owners: Vec<_> = size_fields
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"EntrySet"),
        "one size must own EntrySet; owners={owners:?}"
    );
    assert!(
        owners.contains(&"Values"),
        "one size must own Values; owners={owners:?}"
    );
}

/// Field in a single class keeps its owner_class.
#[test]
fn single_class_field_has_owner() {
    let src = "class Foo { private String name; }\n";
    let g = parse(src);
    let name_field = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Property && n.name == "name")
        .expect("name field");
    assert_eq!(
        name_field.owner_class.as_deref(),
        Some("Foo"),
        "name.owner_class must be Foo; got {:?}",
        name_field.owner_class
    );
}
