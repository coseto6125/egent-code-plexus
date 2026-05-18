//! SQL Named dimension: `CREATE VIEW` emits `NodeKind::Typedef`.
//!
//! A view is a named alias for an underlying SELECT query.
//! Column aliases inside SELECT (`x AS y`) are explicitly excluded.
//! `CREATE TABLE` continues to emit `NodeKind::Class`.

use graph_nexus_analyzer::sql::parser::SqlProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = SqlProvider::new().expect("SqlProvider init");
    let graph = provider
        .parse_file(Path::new("schema.sql"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find_kind<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn test_sql_create_view_emits_typedef() {
    let src = "CREATE VIEW active_users AS SELECT * FROM users WHERE active = true;";
    let nodes = parse(src);
    find_kind(&nodes, "active_users", NodeKind::Typedef);
}

#[test]
fn test_sql_create_table_still_emits_class() {
    // CREATE TABLE must remain NodeKind::Class — unaffected by the view change.
    let src = "CREATE TABLE users (id INT, name TEXT);";
    let nodes = parse(src);
    find_kind(&nodes, "users", NodeKind::Class);
}

#[test]
fn test_sql_column_alias_not_typedef() {
    // `SELECT x AS y` must NOT produce a Typedef node — column aliases are noise.
    let src = "CREATE VIEW v AS SELECT id AS user_id FROM users;";
    let nodes = parse(src);
    find_kind(&nodes, "v", NodeKind::Typedef);
    // `user_id` must not appear as Typedef
    assert!(
        nodes
            .iter()
            .find(|n| n.name == "user_id" && n.kind == NodeKind::Typedef)
            .is_none(),
        "column alias `user_id` must not emit as Typedef; nodes: {nodes:#?}"
    );
}

#[test]
fn test_sql_view_and_table_coexist() {
    let src = "CREATE TABLE products (id INT);\nCREATE VIEW top_products AS SELECT * FROM products;";
    let nodes = parse(src);
    find_kind(&nodes, "products", NodeKind::Class);
    find_kind(&nodes, "top_products", NodeKind::Typedef);
}

#[test]
fn test_sql_multiple_views() {
    let src = "CREATE VIEW v1 AS SELECT 1;\nCREATE VIEW v2 AS SELECT 2;";
    let nodes = parse(src);
    find_kind(&nodes, "v1", NodeKind::Typedef);
    find_kind(&nodes, "v2", NodeKind::Typedef);
}
