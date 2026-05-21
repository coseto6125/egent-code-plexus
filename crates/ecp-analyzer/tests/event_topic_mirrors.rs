//! T5-33: `EventTopic` Node + `Publishes`/`Subscribes` + `EventTopicMirror`
//! end-to-end emission tests.
//!
//! Exercises the full pipeline:
//!   per-language parsers emit `RawEventTopic`
//!   → `GraphBuilder::build()`
//!   → `post_process::event_topic_mirrors`
//!   → final `ZeroCopyGraph` with EventTopic nodes + Publishes/Subscribes +
//!     EventTopicMirror edges.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};

fn parse_python(path: &str, src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = PythonProvider::new().expect("python provider");
    provider
        .parse_file(path.as_ref(), src.as_bytes())
        .expect("parse_file")
}

fn build(local_graphs: Vec<ecp_core::analyzer::types::LocalGraph>) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for lg in local_graphs {
        builder.add_graph(lg);
    }
    builder.build()
}

fn count_edges(graph: &ZeroCopyGraph, rel: RelType) -> usize {
    graph.edges.iter().filter(|e| e.rel_type == rel).count()
}

fn count_event_topic_nodes(graph: &ZeroCopyGraph, name: &str) -> usize {
    let pool = graph.string_pool.as_slice();
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EventTopic && n.name.resolve(pool) == name)
        .count()
}

// ---------------------------------------------------------------------------
// Test 1: Single-lib pair → 1 EventTopicMirror edge with confidence < 0.9
// ---------------------------------------------------------------------------

/// Redis publisher + Redis subscriber in Python on the same topic "orders"
/// → exactly one `EventTopicMirror` edge with confidence < 0.9.
#[test]
fn test_single_lib_pair_emits_one_mirror_edge() {
    let publisher = parse_python(
        "services/publisher.py",
        r#"
import redis

def publish_order(r, data):
    r.publish("orders", data)
"#,
    );
    let subscriber = parse_python(
        "services/subscriber.py",
        r#"
import redis

def listen_orders(pubsub):
    pubsub.subscribe("orders")
"#,
    );

    let graph = build(vec![publisher, subscriber]);

    // Two EventTopic nodes emitted — one per call site.
    assert_eq!(
        count_event_topic_nodes(&graph, "orders"),
        2,
        "expected 2 EventTopic nodes for 'orders' (one publish, one subscribe)"
    );

    // Exactly 1 EventTopicMirror edge.
    let mirror_count = count_edges(&graph, RelType::EventTopicMirror);
    assert_eq!(
        mirror_count, 1,
        "expected 1 EventTopicMirror edge; got {mirror_count}"
    );

    // Confidence must be < 0.9 so is_heuristic() returns true.
    let mirror_edge = graph
        .edges
        .iter()
        .find(|e| e.rel_type == RelType::EventTopicMirror)
        .expect("EventTopicMirror edge must exist");
    assert!(
        mirror_edge.confidence < 0.9,
        "EventTopicMirror confidence must be < 0.9 (heuristic); got {}",
        mirror_edge.confidence
    );
}

// ---------------------------------------------------------------------------
// Test 2: Same topic, different lib → NO mirror edge (same-lib gate)
// ---------------------------------------------------------------------------

/// Kafka publisher on "orders" + Redis subscriber on "orders" → NO mirror.
///
/// Cross-lib pairing is deferred (T5-33-followup). The same-lib gate prevents
/// false positives between independent transport semantics.
#[test]
fn test_different_lib_same_topic_no_mirror() {
    // Kafka publish: "orders" via kafka-python
    let kafka_publisher = parse_python(
        "services/kafka_pub.py",
        r#"
from kafka import KafkaProducer

def publish_order(data):
    p = KafkaProducer(bootstrap_servers="localhost:9092")
    p.send("orders", b"x")
"#,
    );
    // Redis subscribe: "orders" via redis-py
    let redis_subscriber = parse_python(
        "services/redis_sub.py",
        r#"
import redis

def listen_orders(pubsub):
    pubsub.subscribe("orders")
"#,
    );

    let graph = build(vec![kafka_publisher, redis_subscriber]);

    // No EventTopicMirror — they're on different frameworks.
    let mirror_count = count_edges(&graph, RelType::EventTopicMirror);
    assert_eq!(
        mirror_count, 0,
        "different-lib pair must not emit EventTopicMirror; got {mirror_count}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Ambiguity (2 publishers, 1 subscriber) → top-1 only
// ---------------------------------------------------------------------------

/// Two Redis publishers on "payments" + one Redis subscriber on "payments".
/// Bidirectional top-1 picks the first publisher by insertion/line order.
/// Exactly one EventTopicMirror edge is emitted (not two).
#[test]
fn test_two_publishers_one_subscriber_emits_one_mirror() {
    let pub_a = parse_python(
        "services/pub_a.py",
        r#"
import redis

def publish_payment_a(r, data):
    r.publish("payments", data)
"#,
    );
    let pub_b = parse_python(
        "services/pub_b.py",
        r#"
import redis

def publish_payment_b(r, data):
    r.publish("payments", data)
"#,
    );
    let subscriber = parse_python(
        "services/payments_sub.py",
        r#"
import redis

def listen_payments(pubsub):
    pubsub.subscribe("payments")
"#,
    );

    let graph = build(vec![pub_a, pub_b, subscriber]);

    // Conservative: only one mirror edge despite 2 publishers (top-1 picks first).
    let mirror_count = count_edges(&graph, RelType::EventTopicMirror);
    assert_eq!(
        mirror_count, 1,
        "2 publishers + 1 subscriber must emit exactly 1 mirror (top-1); got {mirror_count}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: No matching topic → 0 edges
// ---------------------------------------------------------------------------

/// Publisher on "orders", subscriber on "payments" → NO mirror.
#[test]
fn test_mismatched_topics_no_mirror() {
    let publisher = parse_python(
        "services/order_pub.py",
        r#"
import redis

def publish_order(r, data):
    r.publish("orders", data)
"#,
    );
    let subscriber = parse_python(
        "services/payments_sub.py",
        r#"
import redis

def listen_payments(pubsub):
    pubsub.subscribe("payments")
"#,
    );

    let graph = build(vec![publisher, subscriber]);

    let mirror_count = count_edges(&graph, RelType::EventTopicMirror);
    assert_eq!(
        mirror_count, 0,
        "mismatched topics must not emit EventTopicMirror; got {mirror_count}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Confidence assertion — emitted edge confidence < 0.9 → is_heuristic
// ---------------------------------------------------------------------------

/// Structural assertion: `RelType::EventTopicMirror.is_heuristic()` is true,
/// and a freshly-emitted edge carries confidence < 0.9.
#[test]
fn test_event_topic_mirror_is_heuristic() {
    // Schema-level check: the variant is registered as heuristic.
    assert!(
        RelType::EventTopicMirror.is_heuristic(),
        "EventTopicMirror MUST be marked heuristic so ecp impact hides it by default"
    );

    // Pipeline check: emitted edge has confidence < 0.9.
    let publisher = parse_python(
        "svc/pub.py",
        r#"
import redis

def emit_event(r, data):
    r.publish("events", data)
"#,
    );
    let subscriber = parse_python(
        "svc/sub.py",
        r#"
import redis

def handle_event(pubsub):
    pubsub.subscribe("events")
"#,
    );
    let graph = build(vec![publisher, subscriber]);

    let mirror_edge = graph
        .edges
        .iter()
        .find(|e| e.rel_type == RelType::EventTopicMirror)
        .expect("EventTopicMirror edge must be emitted for matched pair");

    assert!(
        mirror_edge.confidence < 0.9,
        "EventTopicMirror edge confidence must be < 0.9; got {}",
        mirror_edge.confidence
    );
}

// ---------------------------------------------------------------------------
// Additional sanity checks
// ---------------------------------------------------------------------------

/// Publishes + Subscribes structural edges are emitted alongside the mirror.
#[test]
fn test_publishes_subscribes_edges_emitted() {
    let publisher = parse_python(
        "svc/producer.py",
        r#"
import redis

def broadcast(r, msg):
    r.publish("notifications", msg)
"#,
    );
    let subscriber = parse_python(
        "svc/consumer.py",
        r#"
import redis

def listen(pubsub):
    pubsub.subscribe("notifications")
"#,
    );
    let graph = build(vec![publisher, subscriber]);

    // Publishes edge: fn → EventTopic
    assert!(
        count_edges(&graph, RelType::Publishes) >= 1,
        "at least one Publishes edge must be emitted"
    );
    // Subscribes edge: fn → EventTopic
    assert!(
        count_edges(&graph, RelType::Subscribes) >= 1,
        "at least one Subscribes edge must be emitted"
    );
    // Mirror edge
    assert_eq!(
        count_edges(&graph, RelType::EventTopicMirror),
        1,
        "exactly one EventTopicMirror for matched 'notifications' pair"
    );
}

/// File with no event_topics → no EventTopic nodes, no mirror edges.
#[test]
fn test_no_event_topics_no_emission() {
    let plain = parse_python("plain.py", "def add(x, y):\n    return x + y\n");
    let graph = build(vec![plain]);

    assert_eq!(count_event_topic_nodes(&graph, "orders"), 0);
    assert_eq!(count_edges(&graph, RelType::EventTopicMirror), 0);
    assert_eq!(count_edges(&graph, RelType::Publishes), 0);
    assert_eq!(count_edges(&graph, RelType::Subscribes), 0);
}
