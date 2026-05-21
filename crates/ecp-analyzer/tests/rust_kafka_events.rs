//! T5-7 integration tests: Kafka Rust event-topic detector.
//!
//! Exercises the production `KAFKA_RUST` const and the real `frameworks.scm`
//! query string against rdkafka patterns:
//! - `producer.send(FutureRecord::to("topic"), ...)` (async producer)
//! - `consumer.subscribe(&["topic", ...])` (consumer)

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_RUST};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/rust/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/rust/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let combined = format!(
        "{}\n;; ---- framework queries ----\n{}",
        QUERIES_SCM, FRAMEWORKS_SCM
    );
    let query = Query::new(&lang, &combined).expect("query compile");
    let imports: Vec<RawImport> = import_sources
        .iter()
        .map(|s| RawImport {
            source: (*s).to_string(),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    extract_event_topics(&tree, src.as_bytes(), &query, &[KAFKA_RUST], &imports)
}

/// rdkafka async producer: producer.send(FutureRecord::to("topic"), ...).await → Publish.
#[test]
fn test_rust_rdkafka_future_record_await_literal_topic() {
    let src = r#"
use rdkafka::producer::{FutureProducer, FutureRecord};

async fn publish_order(producer: &FutureProducer) {
    producer.send(
        FutureRecord::to("orders").payload("hello"),
        std::time::Duration::from_secs(0),
    ).await;
}
"#;
    let result = run(src, &["rdkafka"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from FutureRecord::to; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
}

/// rdkafka consumer: consumer.subscribe(&["topic"]) → Subscribe direction.
#[test]
fn test_rust_rdkafka_consumer_subscribe_literal_topic() {
    let src = r#"
use rdkafka::consumer::{StreamConsumer, Consumer};

fn subscribe_orders(consumer: &StreamConsumer) {
    consumer.subscribe(&["orders"]).expect("subscribe failed");
}
"#;
    let result = run(src, &["rdkafka"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from consumer.subscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
}

/// Variable topic → no capture (no fabrication).
#[test]
fn test_rust_variable_topic_emits_nothing() {
    let src = r#"
use rdkafka::producer::{FutureProducer, FutureRecord};

async fn publish_dynamic(producer: &FutureProducer, topic: &str) {
    producer.send(
        FutureRecord::to(topic).payload("hello"),
        std::time::Duration::from_secs(0),
    ).await;
}
"#;
    let result = run(src, &["rdkafka"]);
    assert!(
        result.is_empty(),
        "variable topic must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No rdkafka import → import gate must reject all captures.
#[test]
fn test_rust_no_rdkafka_import_emits_nothing() {
    let src = r#"
async fn publish_order(producer: &SomeProducer) {
    producer.send(
        FutureRecord::to("orders").payload("hello"),
        std::time::Duration::from_secs(0),
    ).await;
}
"#;
    let result = run(src, &["some_other_crate"]);
    assert!(
        result.is_empty(),
        "non-rdkafka import must produce nothing; got {:?}",
        result
    );
}

/// rdkafka sync producer send (non-await form) → Publish.
#[test]
fn test_rust_rdkafka_future_record_sync_literal_topic() {
    let src = r#"
use rdkafka::producer::{FutureProducer, FutureRecord};

fn publish_payment(producer: &FutureProducer) {
    producer.send(
        FutureRecord::to("payments").payload("data"),
        std::time::Duration::from_secs(5),
    );
}
"#;
    let result = run(src, &["rdkafka"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from sync send; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "payments"
    );
}

/// Enclosing function name is captured correctly.
#[test]
fn test_rust_enclosing_fn_name_captured() {
    let src = r#"
use rdkafka::producer::{FutureProducer, FutureRecord};

async fn publish_user_event(producer: &FutureProducer) {
    producer.send(
        FutureRecord::to("users").payload("x"),
        std::time::Duration::from_secs(0),
    ).await;
}
"#;
    let result = run(src, &["rdkafka"]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].enclosing_fn.as_ref(), "publish_user_event");
}
