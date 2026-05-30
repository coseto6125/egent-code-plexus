//! Language-neutral SQL-string analysis: decide whether a string literal is a
//! SQL statement and, if so, extract the tables it references with a read/write
//! verb. Takes only `&str`, so per-language drift is impossible (mirrors
//! `path_literal`). Reuses the in-tree `tree-sitter-sequel` grammar.

use ecp_core::analyzer::types::SqlVerb;
use std::cell::RefCell;
use tree_sitter::{Node, Parser};

thread_local! {
    static SQL_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_sequel::LANGUAGE.into())
            .expect("set tree-sitter-sequel");
        p
    });
}

/// Cheap pre-filter: a leading SQL verb keyword. Avoids tree-sitter work on the
/// overwhelming majority of string literals (log lines, prose, config keys).
pub fn is_sql_shaped(s: &str) -> bool {
    let trimmed = s.trim_start();
    let head: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(
        head.as_str(),
        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "WITH"
    )
}

/// Result of parsing one SQL string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTables {
    pub tables: Vec<(String, SqlVerb)>,
    pub unresolved: bool,
}

/// Parse a SQL string and return its referenced tables + statement verb.
/// Unparseable / table-less input → `unresolved`.
pub fn parse_tables(sql: &str) -> SqlTables {
    SQL_PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let Some(tree) = parser.parse(sql, None) else {
            return SqlTables {
                tables: vec![],
                unresolved: true,
            };
        };
        let root = tree.root_node();
        let Some(verb) = statement_verb(root) else {
            return SqlTables {
                tables: vec![],
                unresolved: true,
            };
        };
        let mut tables = Vec::new();
        collect_tables(root, sql.as_bytes(), verb, &mut tables);
        if tables.is_empty() {
            return SqlTables {
                tables: vec![],
                unresolved: true,
            };
        }
        SqlTables {
            tables,
            unresolved: false,
        }
    })
}

/// Find the first statement node and map it to a verb.
fn statement_verb(node: Node) -> Option<SqlVerb> {
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "select" => return Some(SqlVerb::Read),
            "insert" | "update" | "delete" => return Some(SqlVerb::Write),
            _ => {}
        }
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// Return true when a table name is a valid SQL identifier (not a brace
/// placeholder like `{tbl}` or a parameter like `$1`).
fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    // Identifiers start with letter or underscore (or unicode letter in some
    // dialects), never with `{`, `$`, `%`, or digits.
    if !first.is_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Return true when the node's ancestor chain (or its parent's siblings) contains
/// an ERROR node at the same level — meaning the identifier was recovered by the
/// parser from a broken context (e.g. `{tbl}` produces ERROR nodes for `{`/`}`
/// flanking the recovered `tbl` object_reference under the same `from` parent).
fn has_error_sibling(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let mut cursor = parent.walk();
    for sibling in parent.children(&mut cursor) {
        if sibling.is_error() {
            return true;
        }
    }
    false
}

/// Collect table identifiers: `object_reference` nodes whose PARENT is a table
/// position (`from` / `relation` / `insert` / `update` / `delete`), never a
/// `field` (column qualifier like `c.x`). Rejects recovered identifiers whose
/// parent has ERROR siblings (placeholder syntax like `{tbl}`).
fn collect_tables(node: Node, src: &[u8], verb: SqlVerb, out: &mut Vec<(String, SqlVerb)>) {
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "object_reference" {
            let parent_kind = n.parent().map(|p| p.kind()).unwrap_or("");
            let table_position = matches!(
                parent_kind,
                "from" | "relation" | "insert" | "update" | "delete"
            );
            if table_position {
                // Check for ERROR siblings at the parent level: indicates a
                // placeholder like {tbl} where braces parse as ERROR nodes.
                let contaminated =
                    has_error_sibling(n) || n.parent().map(has_error_sibling).unwrap_or(false);
                if !contaminated {
                    if let Some(name_node) = n.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(src) {
                            if is_valid_identifier(name) {
                                let owned = name.to_string();
                                if !out.iter().any(|(t, _)| *t == owned) {
                                    out.push((owned, verb));
                                }
                            }
                        }
                    }
                }
            }
        }
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
}
