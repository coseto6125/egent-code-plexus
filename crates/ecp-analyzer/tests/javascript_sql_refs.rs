//! JavaScript SQL-ref extractor tests.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{RawSqlRef, SqlVerb};
use std::path::Path;

fn parse_sql_refs(src: &str) -> Vec<RawSqlRef> {
    let provider = JavaScriptProvider::new().expect("provider");
    let graph = provider
        .parse_file(Path::new("api.js"), src.as_bytes())
        .expect("parse_file");
    graph.sql_refs.map(|b| b.into_vec()).unwrap_or_default()
}

#[test]
fn js_select_yields_read_ref() {
    let src = "async function listChannels(client) {\n  return client.query(\"SELECT id FROM channels WHERE org_id = $1\");\n}\n";
    let refs = parse_sql_refs(src);
    let r = refs
        .iter()
        .find(|r| r.tables.iter().any(|(t, _)| t == "channels"))
        .expect("ref");
    assert!(!r.unresolved);
    assert_eq!(r.tables, vec![("channels".to_string(), SqlVerb::Read)]);
}

#[test]
fn js_enclosing_symbol_captured() {
    let src = r#"
class UserRepo {
    async findById(db, id) {
        return db.query("SELECT * FROM users WHERE id = $1", [id]);
    }
}
"#;
    let refs = parse_sql_refs(src);
    let r = refs
        .iter()
        .find(|r| r.tables.iter().any(|(t, _)| t == "users"))
        .expect("ref");
    assert_eq!(r.enclosing_symbol.as_deref(), Some("findById"));
    assert_eq!(r.enclosing_owner.as_deref(), Some("UserRepo"));
}

#[test]
fn js_insert_yields_write_ref() {
    let src = r#"
function createOrder(db, data) {
    db.execute("INSERT INTO orders (user_id, amount) VALUES (?, ?)");
}
"#;
    let refs = parse_sql_refs(src);
    let r = refs
        .iter()
        .find(|r| r.tables.iter().any(|(t, _)| t == "orders"))
        .expect("ref");
    assert!(r.tables.iter().any(|(_, v)| *v == SqlVerb::Write));
}

#[test]
fn js_non_sql_string_not_emitted() {
    let src = r#"
function load() {
    return fs.readFileSync("session_meta.json", "utf-8");
}
"#;
    let refs = parse_sql_refs(src);
    assert!(
        refs.is_empty(),
        "non-SQL string should not produce sql_refs: {refs:?}"
    );
}

#[test]
fn test_parse_sql_refs_long_chain_preserves_enclosing_symbol_and_owner() {
    let source = format!(
        "class Repository {{ run(db) {{ db{}.query(\"SELECT id FROM users\"); }} }}",
        ".option('x')".repeat(200)
    );
    let refs = parse_sql_refs(&source);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].tables, vec![("users".into(), SqlVerb::Read)]);
    assert_eq!(refs[0].enclosing_symbol.as_deref(), Some("run"));
    assert_eq!(refs[0].enclosing_owner.as_deref(), Some("Repository"));
}
