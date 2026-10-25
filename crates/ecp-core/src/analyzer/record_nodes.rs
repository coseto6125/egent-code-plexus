//! The nodes and edges a build makes from one file's raw records: path
//! literals, SQL table references, transaction scopes, schema fields and
//! event topics.
//!
//! The index's post-process passes (`ecp-analyzer` `post_process::
//! {path_literal_nodes, sql_table_edges, tx_scope_edges,
//! schema_field_mirrors, event_topic_mirrors}`) and the overlay's per-file
//! replay (`session::incremental`) both call these, so a dirty file gets the
//! nodes a reindex gives. Each side maps a record's endpoint and a table name
//! to its own node ids. The name-paired heuristic edges (`MirrorsField`,
//! `EventTopicMirror`) stay index-time and are not here.

use crate::analyzer::types::{
    PubSub, RawEventTopic, RawPathLiteral, RawSchemaField, RawSqlRef, RawTxScope, Span,
};
use crate::contracts::raw_records::{RawEndpoint, RawIdx};
use crate::graph::{NodeKind, RelType};
use crate::uid;
use rustc_hash::FxHashMap;
use std::borrow::Cow;

/// `Edge.reason` of an `OpensTxScope` edge.
pub const TX_SCOPE_REASON: &str = "post_process:tx_scope";

/// `Edge.reason` of a `Class -> SchemaField` `HasProperty` edge.
pub const SCHEMA_FIELD_HAS_PROPERTY_REASON: &str = "post_process:schema_field:has_property";

/// `Edge.reason` of a `Publishes` edge.
pub const EVENT_TOPIC_PUBLISHES_REASON: &str = "post_process:event_topic:publishes";

/// `Edge.reason` of a `Subscribes` edge.
pub const EVENT_TOPIC_SUBSCRIBES_REASON: &str = "post_process:event_topic:subscribes";

/// Every `RawPathLiteral::sink_reason` the extractors render
/// (`ecp_analyzer::path_literal::sink_reason`: each sink kind with each
/// confidence). A `UsesPathLiteral` edge carries its literal's reason
/// verbatim; the overlay's cached view stores a reason as its position in a
/// closed list, so the list lives here. `tests/strict_overlay_records.rs`
/// holds it to the renderer.
pub const PATH_LITERAL_SINK_REASONS: [&str; 14] = [
    "sink:read|confidence:high",
    "sink:read|confidence:medium",
    "sink:write|confidence:high",
    "sink:write|confidence:medium",
    "sink:open-read|confidence:high",
    "sink:open-read|confidence:medium",
    "sink:open-write|confidence:high",
    "sink:open-write|confidence:medium",
    "sink:join|confidence:high",
    "sink:join|confidence:medium",
    "sink:ext-change|confidence:high",
    "sink:ext-change|confidence:medium",
    "sink:free|confidence:high",
    "sink:free|confidence:medium",
];

/// The fields of one record node that the graph stores. `owner` is empty
/// for no owner, as `Node.owner_class` stores it; `span` is 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordNode<'a> {
    pub kind: NodeKind,
    pub uid: u64,
    pub name: Cow<'a, str>,
    pub owner: &'a str,
    pub span: Span,
}

/// The `PathLiteral` node of `literal` in the file at `path`. `endpoint` is
/// the literal's endpoint, and `owner_of(raw)` the owner class of the file's
/// raw node `raw` (`None` for no owner or an index past the file).
///
/// The owner is the bound node's own owner, so two identical literals in
/// same-name methods of two types get distinct uids. A literal with no bound
/// node (a class-body field initializer binds the file) keeps the class its
/// walker found, or two classes of one file holding the same literal would
/// share a uid. Two identical literals under one owner share a uid: the
/// index keeps both nodes.
pub fn path_literal_node<'a>(
    path: &str,
    literal: &'a RawPathLiteral,
    endpoint: Option<&RawEndpoint>,
    owner_of: impl FnOnce(RawIdx) -> Option<&'a str>,
) -> RecordNode<'a> {
    let owner = match endpoint {
        Some(RawEndpoint::Node(idx)) => owner_of(*idx),
        _ => literal.enclosing_owner.as_deref(),
    }
    .unwrap_or("");
    RecordNode {
        kind: NodeKind::PathLiteral,
        uid: uid::compute(
            NodeKind::PathLiteral,
            path,
            (!owner.is_empty()).then_some(owner),
            &literal.value,
        ),
        name: Cow::Borrowed(&literal.value),
        owner,
        span: literal.span,
    }
}

/// The `QueriesTable` targets of `sql`, as (table name, `Edge.reason`) in
/// statement order: nothing for a statement the parser could not resolve.
/// The target is the first `Class` node of that name in node order, in any
/// language (the SQL DDL parser emits each table as a `Class`); a name with
/// no such node gets no edge.
pub fn sql_table_refs(sql: &RawSqlRef) -> impl Iterator<Item = (&str, &'static str)> {
    sql.tables
        .iter()
        .filter(|_| !sql.unresolved)
        .map(|(table, verb)| (table.as_str(), verb.as_reason()))
}

/// The `TransactionScope` node of `scope` in the file at `path`, whose
/// callable is the file's raw node named `callable` at `span`. Its name is
/// `tx_scope:{callable}#{framework}`, so the uid is stable across indexes.
pub fn tx_scope_node(
    path: &str,
    scope: RawTxScope,
    callable: &str,
    span: Span,
) -> RecordNode<'static> {
    let name = format!("tx_scope:{callable}#{}", scope.framework().as_str());
    RecordNode {
        kind: NodeKind::TransactionScope,
        uid: uid::compute(NodeKind::TransactionScope, path, None, &name),
        name: Cow::Owned(name),
        owner: "",
        span,
    }
}

/// The `SchemaField` node of `field` in the file at `path`. A field whose
/// endpoint binds no type of the file gets no node and no `HasProperty`:
/// the caller checks the endpoint first.
pub fn schema_field_node<'a>(path: &str, field: &'a RawSchemaField) -> RecordNode<'a> {
    RecordNode {
        kind: NodeKind::SchemaField,
        uid: uid::compute(
            NodeKind::SchemaField,
            path,
            Some(&field.owner_class),
            &field.name,
        ),
        name: Cow::Borrowed(&field.name),
        owner: &field.owner_class,
        span: field.span,
    }
}

/// The `EventTopic` node of each topic of one file with a literal topic, as
/// `emit(record index, node)` in record order; a dynamic topic gets none.
/// The uid keys on the topic's pairing key, and the name is the key's
/// earliest spelling in the file (smallest span), so two spellings of one
/// key name one node. A whitespace-only topic is a topic like any other.
/// Cost: O(topics) with one map of the file's keys.
pub fn event_topic_nodes<'a>(
    path: &str,
    topics: &'a [RawEventTopic],
    mut emit: impl FnMut(usize, RecordNode<'a>),
) {
    let key = |topic: &'a RawEventTopic| -> Option<(&'a str, &'a str)> {
        let literal = topic.topic_literal.as_deref()?;
        Some((topic.topic_key.as_deref().unwrap_or(literal), literal))
    };
    let mut first_spelling: FxHashMap<&str, (Span, &str)> = FxHashMap::default();
    for topic in topics {
        if let Some((key, literal)) = key(topic) {
            let slot = first_spelling.entry(key).or_insert((topic.span, literal));
            if topic.span < slot.0 {
                *slot = (topic.span, literal);
            }
        }
    }
    for (i, topic) in topics.iter().enumerate() {
        let Some((key, literal)) = key(topic) else {
            continue;
        };
        let name = first_spelling
            .get(key)
            .map_or(literal, |&(_, spelling)| spelling);
        emit(
            i,
            RecordNode {
                kind: NodeKind::EventTopic,
                uid: uid::compute(NodeKind::EventTopic, path, None, key),
                name: Cow::Borrowed(name),
                owner: "",
                span: topic.span,
            },
        );
    }
}

/// The edge kind and `Edge.reason` from a topic's endpoint to its node.
pub fn event_topic_edge(direction: PubSub) -> (RelType, &'static str) {
    match direction {
        PubSub::Publish => (RelType::Publishes, EVENT_TOPIC_PUBLISHES_REASON),
        PubSub::Subscribe => (RelType::Subscribes, EVENT_TOPIC_SUBSCRIBES_REASON),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::types::{FrameworkId, SchemaType, SqlVerb};

    fn literal(value: &str, enclosing_owner: Option<&str>) -> RawPathLiteral {
        RawPathLiteral {
            value: value.to_string(),
            span: (3, 4, 3, 12),
            enclosing_symbol: None,
            enclosing_owner: enclosing_owner.map(str::to_string),
            sink_reason: PATH_LITERAL_SINK_REASONS[0].to_string(),
            endpoint: None,
        }
    }

    fn topic(literal: Option<&str>, key: Option<&str>, row: u32) -> RawEventTopic {
        RawEventTopic {
            topic_literal: literal.map(Into::into),
            topic_key: key.map(Into::into),
            direction: PubSub::Publish,
            lib: FrameworkId::Kafka,
            enclosing_fn: "".into(),
            span: (row, 0, row, 9),
            endpoint: None,
        }
    }

    fn topics(path: &str, records: &[RawEventTopic]) -> Vec<(usize, String, u64)> {
        let mut out = Vec::new();
        event_topic_nodes(path, records, |i, node| {
            out.push((i, node.name.into_owned(), node.uid))
        });
        out
    }

    #[test]
    fn test_path_literal_node_bound_node_owner_wins_over_walker_owner() {
        let lit = literal("cfg.json", Some("Walker"));
        let node = path_literal_node(
            "src/a.rs",
            &lit,
            Some(&RawEndpoint::Node(RawIdx(1))),
            |raw| (raw == RawIdx(1)).then_some("Bound"),
        );
        assert_eq!(node.owner, "Bound");
        assert_eq!(
            node.uid,
            uid::compute(NodeKind::PathLiteral, "src/a.rs", Some("Bound"), "cfg.json")
        );
        assert_eq!((node.name.as_ref(), node.span), ("cfg.json", (3, 4, 3, 12)));
    }

    #[test]
    fn test_path_literal_node_file_endpoint_keeps_walker_owner() {
        let lit = literal("cfg.json", Some("Walker"));
        let node = path_literal_node("src/a.rs", &lit, Some(&RawEndpoint::File), |_| {
            unreachable!("a file endpoint reads no node")
        });
        assert_eq!(node.owner, "Walker");
    }

    /// A bound node without an owner and an empty walker owner both give the
    /// owner-less uid.
    #[test]
    fn test_path_literal_node_no_owner_uid_has_no_owner_segment() {
        let unowned = uid::compute(NodeKind::PathLiteral, "src/a.rs", None, "cfg.json");
        let bound = literal("cfg.json", Some("Walker"));
        let node = path_literal_node(
            "src/a.rs",
            &bound,
            Some(&RawEndpoint::Node(RawIdx(0))),
            |_| None,
        );
        assert_eq!((node.owner, node.uid), ("", unowned));
        let top = literal("cfg.json", None);
        assert_eq!(
            path_literal_node("src/a.rs", &top, None, |_| None).uid,
            unowned
        );
    }

    #[test]
    fn test_sql_table_refs_unresolved_statement_yields_nothing() {
        let mut sql = RawSqlRef {
            tables: vec![
                ("users".to_string(), SqlVerb::Read),
                ("audit".to_string(), SqlVerb::Write),
            ],
            unresolved: false,
            span: (0, 0, 0, 1),
            enclosing_symbol: None,
            enclosing_owner: None,
            endpoint: None,
        };
        assert_eq!(
            sql_table_refs(&sql).collect::<Vec<_>>(),
            [("users", "read"), ("audit", "write")]
        );
        sql.unresolved = true;
        assert_eq!(sql_table_refs(&sql).count(), 0);
    }

    #[test]
    fn test_tx_scope_node_name_joins_callable_and_framework() {
        let scope = RawTxScope::new(2, FrameworkId::SpringTransactional);
        let node = tx_scope_node("src/S.java", scope, "save", (4, 0, 9, 1));
        let name = format!(
            "tx_scope:save#{}",
            FrameworkId::SpringTransactional.as_str()
        );
        assert_eq!(node.name, name);
        assert_eq!(
            node.uid,
            uid::compute(NodeKind::TransactionScope, "src/S.java", None, &name)
        );
        assert_eq!((node.owner, node.span), ("", (4, 0, 9, 1)));
    }

    #[test]
    fn test_schema_field_node_uid_carries_owner() {
        let field = RawSchemaField {
            name: "email".into(),
            type_class: SchemaType::String,
            owner_class: "User".into(),
            framework: FrameworkId::Pydantic,
            span: (2, 4, 2, 15),
            endpoint: None,
        };
        let node = schema_field_node("m.py", &field);
        assert_eq!(
            node.uid,
            uid::compute(NodeKind::SchemaField, "m.py", Some("User"), "email")
        );
        assert_eq!((node.name.as_ref(), node.owner), ("email", "User"));
    }

    /// Two spellings of one key name one node by the earliest one; a dynamic
    /// topic gets no node; a whitespace-only topic is kept as written.
    #[test]
    fn test_event_topic_nodes_earliest_spelling_names_shared_key() {
        let records = [
            topic(Some("Orders"), Some("orders"), 5),
            topic(None, None, 6),
            topic(Some("ORDERS"), Some("orders"), 2),
            topic(Some(" "), None, 7),
        ];
        let key_uid = uid::compute(NodeKind::EventTopic, "a.ts", None, "orders");
        let blank_uid = uid::compute(NodeKind::EventTopic, "a.ts", None, " ");
        assert_eq!(
            topics("a.ts", &records),
            [
                (0, "ORDERS".to_string(), key_uid),
                (2, "ORDERS".to_string(), key_uid),
                (3, " ".to_string(), blank_uid),
            ]
        );
    }

    #[test]
    fn test_event_topic_nodes_empty_file_emits_nothing() {
        assert!(topics("a.ts", &[]).is_empty());
    }

    #[test]
    fn test_event_topic_edge_direction_picks_rel_and_reason() {
        assert_eq!(
            event_topic_edge(PubSub::Publish),
            (RelType::Publishes, EVENT_TOPIC_PUBLISHES_REASON)
        );
        assert_eq!(
            event_topic_edge(PubSub::Subscribe),
            (RelType::Subscribes, EVENT_TOPIC_SUBSCRIBES_REASON)
        );
    }
}
