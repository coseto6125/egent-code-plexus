//! Tx scopes and the event-topic and schema-field re-queries.

use super::{FileCx, TxRule};
use crate::event_topic::{extract_event_topics, EventTopicConfig};
use crate::framework_helpers::enclosing_fn_idx_by_span;
use crate::schema_field::{extract_schema_fields, SchemaFieldConfig};
use ecp_core::analyzer::types::{FrameworkId, RawNode, RawTxScope};
use tree_sitter::Query;

pub(super) fn tx_scopes(rule: TxRule, cx: &mut FileCx<'_>) {
    cx.tx_scopes = match rule {
        TxRule::None => None,
        TxRule::Nodes { collect, kinds } => collect(&cx.nodes, kinds),
        TxRule::Tree(collect) => collect(cx.tree.root_node(), cx.source, &cx.nodes),
        TxRule::Sites => resolve_tx_sites(&cx.nodes, &cx.tx_sites),
    };
}

/// Python's `resolve_tx_scopes`: each site binds the innermost enclosing
/// callable; a site with none drops, and sites are not deduped.
fn resolve_tx_sites(
    nodes: &[RawNode],
    sites: &[(u32, u32, FrameworkId)],
) -> Option<Box<[RawTxScope]>> {
    if sites.is_empty() {
        return None;
    }
    let scopes: Vec<RawTxScope> = sites
        .iter()
        .filter_map(|&(row, col, framework)| {
            enclosing_fn_idx_by_span(nodes, row, col).map(|idx| RawTxScope::new(idx, framework))
        })
        .collect();
    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}

pub(super) fn event_topics_requery(
    query: &Query,
    configs: &[EventTopicConfig],
    cx: &mut FileCx<'_>,
) {
    cx.event_topics = extract_event_topics(cx.tree, cx.source, query, configs, &cx.imports);
}

pub(super) fn schema_fields_requery(
    query: &Query,
    configs: &[SchemaFieldConfig],
    cx: &mut FileCx<'_>,
) {
    cx.schema_fields = extract_schema_fields(cx.tree, cx.source, query, configs, &cx.imports);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::node::{decl, rust_tree};
    use crate::driver::TestRule;
    use ecp_core::graph::NodeKind;
    use std::path::Path;

    const FW: FrameworkId = FrameworkId::SpringTransactional;

    fn bind_sites(sites: Vec<(u32, u32, FrameworkId)>) -> Option<Vec<u32>> {
        let tree = rust_tree(b"");
        let mut cx = FileCx::new(&tree, b"", Path::new("a.py"), TestRule::PythonPath);
        cx.nodes = vec![
            decl("outer".to_string(), NodeKind::Function, (0, 0, 9, 0), b""),
            decl("inner".to_string(), NodeKind::Function, (2, 4, 4, 0), b""),
        ];
        cx.tx_sites = sites;
        tx_scopes(TxRule::Sites, &mut cx);
        cx.tx_scopes
            .map(|s| s.iter().map(|t| t.node_idx()).collect())
    }

    #[test]
    fn test_tx_scopes_sites_bind_innermost_and_drop_unenclosed() {
        let sites = vec![(3, 8, FW), (20, 0, FW), (1, 0, FW), (3, 9, FW)];
        assert_eq!(bind_sites(sites), Some(vec![1, 0, 1]));
        assert_eq!(bind_sites(vec![(20, 0, FW)]), None);
        assert_eq!(bind_sites(Vec::new()), None);
    }
}
