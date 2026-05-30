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
        // Must have at least one recognisable statement verb anywhere in the tree.
        if !has_any_statement(root) {
            return SqlTables {
                tables: vec![],
                unresolved: true,
            };
        }
        let cte_names = collect_cte_names(root, sql.as_bytes());
        let mut tables = Vec::new();
        collect_tables(root, sql.as_bytes(), &cte_names, &mut tables);
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

/// Return true when the tree contains at least one select/insert/update/delete node.
fn has_any_statement(node: Node) -> bool {
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "select" | "insert" | "update" | "delete") {
            return true;
        }
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

/// Collect all CTE alias names bound by `WITH <name> AS (...)`.
/// In tree-sitter-sequel the structure is:
///   `(cte (identifier) (keyword_as) (...))` — the first child `identifier`
///   is the alias, not an `object_reference`.
fn collect_cte_names(node: Node, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "cte" {
            // First child of kind "identifier" is the alias.
            if let Some(alias) = n.child(0) {
                if alias.kind() == "identifier" {
                    if let Ok(name) = alias.utf8_text(src) {
                        names.push(name.to_string());
                    }
                }
            }
        }
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    names
}

/// Resolve the read/write verb for a table reference node by walking its
/// ancestor chain.
///
/// Grammar shapes (from tree-sitter-sequel sexps):
/// - SELECT: `statement → select` is a sibling of `from`; the object_reference
///   is under `from → statement`. Neither `select` nor any write verb appears
///   as an ancestor — the `statement` has no write-verb child.
/// - INSERT target: `object_reference` is a direct child of `insert`.
/// - INSERT…SELECT source: `object_reference` is under `from → insert`; but
///   `select` appears as a sibling of `from` inside `insert`. We detect this
///   by seeing `select` as a sibling of any `from` ancestor before we confirm
///   a Write verb.
/// - UPDATE: `object_reference` is under `relation → update`.
/// - DELETE: `(statement (delete) (from (object_reference)))` — `from` is a
///   child of `statement`; `delete` is a sibling of `from`, never an ancestor.
///   We detect this by checking whether any sibling of `from` (or `statement`)
///   is a `delete` node.
fn verb_for(node: Node) -> SqlVerb {
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        match parent.kind() {
            // Direct ancestor is a write-verb node.
            "insert" | "update" | "delete" => {
                // Inside INSERT: check if `cur` is under a `from` that has a
                // `select` sibling — that means the table is a SELECT source,
                // not the INSERT target.
                if parent.kind() == "insert" && is_select_from_under(node) {
                    return SqlVerb::Read;
                }
                return SqlVerb::Write;
            }
            // Direct ancestor is select → always Read.
            "select" => return SqlVerb::Read,
            // Reached the top-level statement node: check whether any of its
            // direct children is a `delete` node (the DELETE grammar puts
            // `delete` and `from` as siblings under `statement`).
            "statement" | "program" => {
                if node_has_child_kind(parent, "delete") {
                    return SqlVerb::Write;
                }
                return SqlVerb::Read;
            }
            _ => {}
        }
        cur = parent;
    }
    SqlVerb::Read
}

/// Return true when `node` is part of a `from` clause that also has a `select`
/// sibling under the same `insert` parent — i.e. the node is the SELECT-source
/// of an INSERT…SELECT, not the insert target itself.
fn is_select_from_under(node: Node) -> bool {
    // Walk up looking for a `from` ancestor whose parent is `insert`.
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        if cur.kind() == "from" && parent.kind() == "insert" {
            // Check whether `insert` also has a `select` child.
            return node_has_child_kind(parent, "select");
        }
        cur = parent;
    }
    false
}

/// Return true if any direct child of `node` has kind `target_kind`.
fn node_has_child_kind(node: Node, target_kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == target_kind);
    found
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
/// parent has ERROR siblings (placeholder syntax like `{tbl}`). Skips any name
/// that is a CTE alias. Verb is resolved per-table from the nearest ancestor
/// statement node so that INSERT...SELECT correctly marks the SELECT source as Read.
fn collect_tables(node: Node, src: &[u8], cte_names: &[String], out: &mut Vec<(String, SqlVerb)>) {
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
                            if is_valid_identifier(name) && !cte_names.iter().any(|c| c == name) {
                                let owned = name.to_string();
                                if !out.iter().any(|(t, _)| *t == owned) {
                                    // Resolve verb per-table from ancestor context,
                                    // not from a single global verb for the whole tree.
                                    let verb = verb_for(n);
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
