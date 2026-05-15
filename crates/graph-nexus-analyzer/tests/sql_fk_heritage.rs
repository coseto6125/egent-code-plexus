//! Integration tests for SQL foreign-key detection as `RawNode.heritage` entries.
//!
//! The tree-sitter-sequel grammar surfaces three canonical FK syntactic forms,
//! all of which must populate the referencing table node's `heritage` field
//! with the referenced table's name. Heritage on a SQL table = "the tables
//! this table depends on by foreign key", which lets impact/route analysis
//! follow data-flow edges between tables.

use graph_nexus_analyzer::sql::parser::SqlProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> Vec<RawNode> {
    let provider = SqlProvider::new().expect("sql provider construction");
    let graph = provider
        .parse_file(Path::new("schema.sql"), source.as_bytes())
        .expect("parse sql source");
    graph.nodes
}

fn table<'a>(nodes: &'a [RawNode], name: &str) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Class) && n.name == name)
        .unwrap_or_else(|| panic!("no Class-kind node named {name} found in {nodes:#?}"))
}

#[test]
fn inline_column_level_fk_emits_heritage() {
    // `col INT REFERENCES other(id)` — the most compact FK form.
    let nodes = parse("CREATE TABLE orders (user_id INT REFERENCES users(id));");
    let orders = table(&nodes, "orders");
    assert_eq!(
        orders.heritage,
        vec!["users".to_string()],
        "inline FK must add referenced table to heritage; got {:?}",
        orders.heritage
    );
}

#[test]
fn table_level_unnamed_fk_emits_heritage() {
    // `FOREIGN KEY (col) REFERENCES other(id)` as a sibling constraint.
    let nodes = parse(
        "CREATE TABLE orders (\n  user_id INT,\n  FOREIGN KEY (user_id) REFERENCES users(id)\n);",
    );
    let orders = table(&nodes, "orders");
    assert_eq!(
        orders.heritage,
        vec!["users".to_string()],
        "unnamed table-level FK must add referenced table to heritage; got {:?}",
        orders.heritage
    );
}

#[test]
fn table_level_named_fk_emits_heritage() {
    // `CONSTRAINT fk_name FOREIGN KEY (col) REFERENCES other(id)`.
    // tree-sitter-sequel currently produces an ERROR node for the
    // `CONSTRAINT <ident> FOREIGN` prefix, but the surviving `constraint`
    // node still carries the REFERENCES clause, so the query must catch it.
    let nodes = parse(
        "CREATE TABLE orders (\n  user_id INT,\n  CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users(id)\n);",
    );
    let orders = table(&nodes, "orders");
    assert_eq!(
        orders.heritage,
        vec!["users".to_string()],
        "named table-level FK must add referenced table to heritage; got {:?}",
        orders.heritage
    );
}

#[test]
fn multiple_fks_all_appear_in_heritage() {
    // Mix of inline and table-level FKs on the same table — both targets
    // must show up. Order should be deterministic from the parse order
    // (inline first, then constraint), but the test only asserts presence
    // so the parser is free to dedupe / reorder without breaking.
    let nodes = parse(
        "CREATE TABLE order_items (\n  user_id INT REFERENCES users(id),\n  product_id INT,\n  FOREIGN KEY (product_id) REFERENCES products(id)\n);",
    );
    let oi = table(&nodes, "order_items");
    assert!(
        oi.heritage.contains(&"users".to_string()),
        "expected `users` in heritage, got {:?}",
        oi.heritage
    );
    assert!(
        oi.heritage.contains(&"products".to_string()),
        "expected `products` in heritage, got {:?}",
        oi.heritage
    );
    assert_eq!(
        oi.heritage.len(),
        2,
        "expected exactly two heritage entries, got {:?}",
        oi.heritage
    );
}

#[test]
fn no_fk_leaves_heritage_empty() {
    let nodes = parse("CREATE TABLE t (id INT, name TEXT);");
    let t = table(&nodes, "t");
    assert!(
        t.heritage.is_empty(),
        "table without FK must have empty heritage; got {:?}",
        t.heritage
    );
}
