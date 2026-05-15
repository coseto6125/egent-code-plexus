//! Generic identifier-occurrence walker shared by every per-language
//! finder in this module. Per-language files plug in their tree-sitter
//! `Language` and the set of node kinds that count as "an identifier"
//! whose text should be checked against the target name.
//!
//! Each per-language file is a thin wrapper that calls this helper — the
//! walk logic itself is identical across grammars.

use graph_nexus_core::analyzer::types::IdentifierRange;
use std::collections::HashSet;
use tree_sitter::{Node, Parser};

/// Parse `source` with `language`, walk every node, and emit a byte-range
/// for every node whose `kind()` is in `id_kinds` AND whose UTF-8 text
/// equals `target`. Returns an empty vec on parse failure (defensive —
/// the caller treats "no hits" as "skip file").
pub fn find_by_kinds(
    source: &[u8],
    target: &str,
    language: &tree_sitter::Language,
    id_kinds: &[&str],
) -> Vec<IdentifierRange> {
    find_in_tree(source, target, language, id_kinds, &[])
}

/// As [`find_by_kinds`], but never recurses into nodes whose `kind()` is
/// in `skip_subtree_kinds`. Used by languages where a wrapper node
/// (e.g. PHP `variable_name` around `$foo`'s inner `name`) would
/// otherwise produce false matches.
pub fn find_in_tree(
    source: &[u8],
    target: &str,
    language: &tree_sitter::Language,
    id_kinds: &[&str],
    skip_subtree_kinds: &[&str],
) -> Vec<IdentifierRange> {
    let mut parser = Parser::new();
    if parser.set_language(language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(
        tree.root_node(),
        source,
        target,
        id_kinds,
        skip_subtree_kinds,
        &mut out,
    );
    out
}

fn walk(
    node: Node<'_>,
    source: &[u8],
    target: &str,
    id_kinds: &[&str],
    skip_subtree_kinds: &[&str],
    out: &mut Vec<IdentifierRange>,
) {
    let kind = node.kind();
    if skip_subtree_kinds.contains(&kind) {
        return;
    }
    if id_kinds.contains(&kind) {
        if let Ok(text) = node.utf8_text(source) {
            if text == target {
                let start = node.start_position();
                out.push(IdentifierRange {
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    row: start.row,
                    col: start.column,
                });
            }
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk(child, source, target, id_kinds, skip_subtree_kinds, out);
    }
}

/// Walk every AST node and collect the unique UTF-8 texts of nodes whose
/// `kind()` is in `id_kinds`. Used by `gnx scan` to enumerate all identifier
/// references in a file without needing a target name.
pub fn find_all_by_kinds(
    source: &[u8],
    language: &tree_sitter::Language,
    id_kinds: &[&str],
) -> Vec<(String, usize)> {
    let mut parser = Parser::new();
    if parser.set_language(language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, usize)> = Vec::new();
    walk_all(tree.root_node(), source, id_kinds, &mut seen, &mut out);
    out
}

fn walk_all(
    node: Node<'_>,
    source: &[u8],
    id_kinds: &[&str],
    seen: &mut HashSet<String>,
    out: &mut Vec<(String, usize)>,
) {
    if id_kinds.contains(&node.kind()) {
        if let Ok(text) = node.utf8_text(source) {
            let name = text.to_string();
            if seen.insert(name.clone()) {
                out.push((name, node.start_position().row + 1));
            }
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_all(child, source, id_kinds, seen, out);
    }
}
