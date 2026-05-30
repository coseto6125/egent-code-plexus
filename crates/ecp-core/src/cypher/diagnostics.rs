//! Pre-execution diagnostics for Cypher queries.
//!
//! The executor follows the OpenCypher convention that an unknown property
//! resolves to `Null` (executor.rs `node_prop_value` / edge arms end in
//! `_ => Value::Null`). That makes a typo'd property name — `n.file` instead
//! of `n.filePath` — silently match nothing and return 0 rows, which is
//! indistinguishable from a genuine empty result. This module walks the parsed
//! AST once (no graph access, no I/O) and reports property names that match
//! neither the node nor the edge set, so the CLI can warn before that silent
//! empty result is mistaken for "no data".

use crate::cypher::ast::{Expr, Query, ReturnExpr};

/// Known node-property names, mirroring every arm of
/// `executor.rs::node_prop_value`. Keep in sync with that match — a new arm
/// there needs its name added here or it will false-positive as unknown.
pub const KNOWN_NODE_PROPS: &[&str] = &[
    "name",
    "uid",
    "kind",
    "ownerClass",
    "line",
    "startLine",
    "endLine",
    "filePath",
    "content",
    "is_test",
    "isTest",
    "is_async",
    "isAsync",
    "is_static",
    "isStatic",
    "is_abstract",
    "isAbstract",
    "is_generator",
    "isGenerator",
    "is_extern",
    "isExtern",
    "visibility",
    "decorators",
];

/// Known edge-property names, mirroring the EdgeRef / edge_vars arms in
/// `executor.rs`.
pub const KNOWN_EDGE_PROPS: &[&str] = &["confidence", "reason", "rel_type"];

/// An unrecognised property reference found in a query, plus the closest known
/// name (node ∪ edge) when one is near enough to be a likely typo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownProp {
    pub prop: String,
    pub suggestion: Option<String>,
}

/// Walk a parsed query and return every property name that is in neither the
/// node nor the edge known-set, deduplicated (one entry per distinct name).
///
/// Membership is checked against the union of node and edge properties rather
/// than classifying each variable as node-vs-edge: a `WITH` clause can rebind a
/// variable, making that classification unreliable, while a real typo is almost
/// never coincidentally a valid name in the other category — so the union
/// catches essentially all typos with none of the rebinding fragility.
///
/// Returns an empty `Vec` (no allocation beyond the empty vec) for the common
/// case where every property is known — the hot path for a well-formed query.
pub fn unknown_properties(q: &Query) -> Vec<UnknownProp> {
    let mut names: Vec<String> = Vec::new();
    collect_query(q, &mut names);
    if names.is_empty() {
        return Vec::new();
    }
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .filter(|n| !is_known(n))
        .map(|n| {
            let suggestion = nearest(&n);
            UnknownProp {
                prop: n,
                suggestion,
            }
        })
        .collect()
}

fn is_known(name: &str) -> bool {
    KNOWN_NODE_PROPS.contains(&name) || KNOWN_EDGE_PROPS.contains(&name)
}

fn collect_query(q: &Query, out: &mut Vec<String>) {
    for m in &q.matches {
        for p in &m.patterns {
            for node in &p.nodes {
                for (key, _) in &node.props {
                    out.push(key.clone());
                }
            }
        }
    }
    if let Some(w) = &q.where_ {
        collect_expr(w, out);
    }
    if let Some(with) = &q.with {
        for item in &with.items {
            collect_return_expr(&item.expr, out);
        }
        if let Some(w) = &with.where_ {
            collect_expr(w, out);
        }
    }
    for item in &q.return_.items {
        collect_return_expr(&item.expr, out);
    }
    for o in &q.order_by {
        collect_return_expr(&o.expr, out);
    }
    if let Some(u) = &q.union {
        collect_query(u, out);
    }
}

fn collect_expr(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Prop(_, prop) => out.push(prop.clone()),
        Expr::BinOp(_, l, r) | Expr::InCollection(l, r) => {
            collect_expr(l, out);
            collect_expr(r, out);
        }
        Expr::UnaryOp(_, inner)
        | Expr::In(inner, _)
        | Expr::Regex(inner, _)
        | Expr::StartsWith(inner, _)
        | Expr::EndsWith(inner, _)
        | Expr::Contains(inner, _) => collect_expr(inner, out),
        Expr::FunCall { args, .. } => {
            for a in args {
                collect_expr(a, out);
            }
        }
        Expr::Var(_) | Expr::Lit(_) | Expr::HasLabel(_, _) => {}
    }
}

fn collect_return_expr(e: &ReturnExpr, out: &mut Vec<String>) {
    match e {
        ReturnExpr::Prop(_, prop) => out.push(prop.clone()),
        ReturnExpr::FunCall { args, .. } => {
            for a in args {
                collect_expr(a, out);
            }
        }
        ReturnExpr::Star | ReturnExpr::Var(_) => {}
    }
}

/// Closest known name (node ∪ edge), or `None` when nothing is near enough to
/// be a plausible typo. A known name that has the unknown name as a prefix
/// (`file` → `filePath`) is a stronger typo signal than raw edit distance —
/// otherwise `file` matches `line` (distance 2) over `filePath` (distance 4).
/// Prefix matches are preferred; among the rest, smallest edit distance within
/// a cap (half the name length, min 2) wins. Only ever called for already-
/// unknown names, so the cost is bounded by the typo count, not the query size.
fn nearest(name: &str) -> Option<String> {
    let cap = (name.len() / 2).max(2);
    KNOWN_NODE_PROPS
        .iter()
        .chain(KNOWN_EDGE_PROPS)
        .filter_map(|cand| {
            let prefix = cand.starts_with(name) || name.starts_with(*cand);
            let dist = levenshtein(name, cand);
            (prefix || dist <= cap).then_some((!prefix, dist, *cand))
        })
        .min()
        .map(|(_, _, cand)| cand.to_string())
}

/// Standard two-row Levenshtein edit distance.
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0usize; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cypher::parse;

    #[test]
    fn flags_typo_with_suggestion() {
        let q = parse("MATCH (n) WHERE n.file CONTAINS 'x' RETURN n.name").unwrap();
        let unknown = unknown_properties(&q);
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].prop, "file");
        assert_eq!(unknown[0].suggestion.as_deref(), Some("filePath"));
    }

    #[test]
    fn camelcase_alias_and_startline_are_known() {
        let q = parse("MATCH (n) WHERE n.isAsync = true RETURN n.startLine, n.endLine").unwrap();
        assert!(unknown_properties(&q).is_empty());
    }

    #[test]
    fn edge_props_are_known() {
        let q = parse("MATCH (a)-[r:Calls]->(b) WHERE r.confidence > 0.5 RETURN r.reason").unwrap();
        assert!(unknown_properties(&q).is_empty());
    }

    #[test]
    fn walks_with_and_union_branches() {
        // `bogus1` hides in a WITH-WHERE, `bogus2` in a UNION branch's RETURN.
        let q = parse(
            "MATCH (n) WITH n WHERE n.bogus1 = 1 RETURN n.name \
             UNION MATCH (m) RETURN m.bogus2",
        )
        .unwrap();
        let names: Vec<String> = unknown_properties(&q).into_iter().map(|u| u.prop).collect();
        assert!(names.contains(&"bogus1".to_string()), "got {names:?}");
        assert!(names.contains(&"bogus2".to_string()), "got {names:?}");
    }

    #[test]
    fn dedups_repeated_unknown() {
        let q = parse("MATCH (n) WHERE n.zzz = 1 OR n.zzz = 2 RETURN n.name").unwrap();
        assert_eq!(unknown_properties(&q).len(), 1);
    }
}
