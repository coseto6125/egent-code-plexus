use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{RawSqlRef, SqlVerb};
use std::path::Path;

fn parse_sql_refs(src: &str) -> Vec<RawSqlRef> {
    let provider = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    let graph = provider
        .parse_file(Path::new("api.ts"), src.as_bytes())
        .expect("parse_file");
    graph.sql_refs.map(|b| b.into_vec()).unwrap_or_default()
}

#[test]
fn ts_select_yields_read_ref() {
    let src = "async function listChannels(db) {\n  return db.query(\"SELECT id, slug FROM channels WHERE org_id = $1\");\n}\n";
    let refs = parse_sql_refs(src);
    let r = refs
        .iter()
        .find(|r| r.tables.iter().any(|(t, _)| t == "channels"))
        .expect("expected sql_ref for table 'channels'");
    assert!(!r.unresolved);
    assert_eq!(r.tables, vec![("channels".to_string(), SqlVerb::Read)]);
}

#[test]
fn ts_insert_yields_write_ref() {
    let src = "function createUser(db, name: string) {\n  db.query(`INSERT INTO users (name) VALUES ($1)`, [name]);\n}\n";
    let refs = parse_sql_refs(src);
    let r = refs
        .iter()
        .find(|r| r.tables.iter().any(|(t, _)| t == "users"))
        .expect("expected sql_ref for table 'users'");
    assert!(!r.unresolved);
    assert_eq!(r.tables, vec![("users".to_string(), SqlVerb::Write)]);
}

#[test]
fn ts_non_sql_string_not_emitted() {
    let src = "function greet() {\n  return \"hello world\";\n}\n";
    let refs = parse_sql_refs(src);
    assert!(refs.is_empty(), "expected no sql_refs, got: {refs:?}");
}

#[test]
fn ts_select_enclosing_symbol_captured() {
    let src = "async function fetchOrders(db) {\n  return db.query(\"SELECT * FROM orders\");\n}\n";
    let refs = parse_sql_refs(src);
    let r = refs
        .iter()
        .find(|r| r.tables.iter().any(|(t, _)| t == "orders"))
        .expect("expected sql_ref for table 'orders'");
    assert_eq!(
        r.enclosing_symbol.as_deref(),
        Some("fetchOrders"),
        "enclosing_symbol mismatch"
    );
}

#[test]
fn test_parse_sql_refs_long_non_sql_chain_and_sql_preserves_ref() {
    let chain = "x.".repeat(200);
    let src = format!(
        "function fetchOrders(db) {{\n  {chain}log(\"not SQL\");\n  return db.query(\"SELECT * FROM orders\");\n}}\n"
    );
    let refs = parse_sql_refs(&src);
    assert_eq!(refs.len(), 1, "unexpected sql_refs: {refs:?}");
    assert_eq!(refs[0].enclosing_symbol.as_deref(), Some("fetchOrders"));
    assert_eq!(refs[0].tables, vec![("orders".to_string(), SqlVerb::Read)]);
}
