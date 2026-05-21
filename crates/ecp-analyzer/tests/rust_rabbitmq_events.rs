//! T5-13 integration tests: RabbitMQ Rust event-topic detector.
//!
//! Exercises the production `RABBITMQ_RUST` const and the real `frameworks.scm`
//! query string against lapin and amiquip patterns.

use ecp_analyzer::event_topic::{extract_event_topics, RABBITMQ_RUST};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/rust/frameworks.scm");
const QUERIES_SCM: &str = include_str!("../src/rust/queries.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
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
    let mut pool = StringPool::new();
    let result = extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[RABBITMQ_RUST],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// lapin: channel.basic_publish(exchange, routing_key, ...) → Publish.
#[test]
fn test_lapin_basic_publish_literal_routing_key() {
    let src = r#"
use lapin::Channel;

async fn publish_order(channel: &Channel, body: Vec<u8>) {
    channel.basic_publish(
        "exchange",
        "orders",
        BasicPublishOptions::default(),
        &body,
        BasicProperties::default(),
    ).await.unwrap();
}
"#;
    let (result, pool) = run(src, &["lapin"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// lapin: channel.basic_consume(queue, ...) → Subscribe.
#[test]
fn test_lapin_basic_consume_literal_queue() {
    let src = r#"
use lapin::Channel;

async fn consume_orders(channel: &Channel) {
    channel.basic_consume(
        "orders",
        "consumer_tag",
        BasicConsumeOptions::default(),
        FieldTable::default(),
    ).await.unwrap();
}
"#;
    let (result, pool) = run(src, &["lapin"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// lapin: channel.basic_get(queue, ...) → Subscribe.
#[test]
fn test_lapin_basic_get_literal_queue() {
    let src = r#"
use lapin::Channel;

async fn poll_queue(channel: &Channel) {
    channel.basic_get("task_queue", BasicGetOptions::default()).await.unwrap();
}
"#;
    let (result, pool) = run(src, &["lapin"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "task/queue");
}

/// amiquip: same API, different import gate.
#[test]
fn test_amiquip_basic_publish_literal_routing_key() {
    let src = r#"
use amiquip::Channel;

fn send_notification(channel: &Channel, body: &[u8]) {
    channel.basic_publish(
        "notifications",
        "email",
        BasicPublishOptions::default(),
        body,
        BasicProperties::default(),
    ).unwrap();
}
"#;
    let (result, pool) = run(src, &["amiquip"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "email");
}

/// Variable routing key → no capture (no fabrication).
#[test]
fn test_variable_routing_key_emits_nothing() {
    let src = r#"
use lapin::Channel;

async fn publish_dynamic(channel: &Channel, routing_key: &str, body: Vec<u8>) {
    channel.basic_publish(
        "exchange",
        routing_key,
        BasicPublishOptions::default(),
        &body,
        BasicProperties::default(),
    ).await.unwrap();
}
"#;
    let (result, _pool) = run(src, &["lapin"]);
    assert!(
        result.is_empty(),
        "variable routing_key must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No RabbitMQ import → empty output (import gate enforces isolation).
#[test]
fn test_no_rabbitmq_import_emits_nothing() {
    let src = r#"
use std::io;

fn send_data(channel: &SomeChannel, body: &[u8]) {
    channel.basic_publish("exchange", "orders", Default::default(), body, Default::default());
}
"#;
    let (result, _pool) = run(src, &["std"]);
    assert!(
        result.is_empty(),
        "non-rabbitmq import must produce nothing; got {:?}",
        result
    );
}

/// Both basic_publish and basic_consume in the same file → 2 topics.
#[test]
fn test_publish_and_consume_same_file() {
    let src = r#"
use lapin::Channel;

async fn send_event(channel: &Channel, body: Vec<u8>) {
    channel.basic_publish("x", "payments", BasicPublishOptions::default(), &body, BasicProperties::default())
        .await.unwrap();
}

async fn receive_event(channel: &Channel) {
    channel.basic_consume("payments", "", BasicConsumeOptions::default(), FieldTable::default())
        .await.unwrap();
}
"#;
    let (result, pool) = run(src, &["lapin"]);
    assert_eq!(result.len(), 2, "expected 2 topics; got {:?}", result);
    let publish = result.iter().find(|r| r.direction == PubSub::Publish);
    let subscribe = result.iter().find(|r| r.direction == PubSub::Subscribe);
    assert!(publish.is_some(), "must have a Publish event");
    assert!(subscribe.is_some(), "must have a Subscribe event");
    let pub_lit = publish
        .unwrap()
        .topic_literal
        .expect("publish topic must be Some");
    let sub_lit = subscribe
        .unwrap()
        .topic_literal
        .expect("subscribe topic must be Some");
    assert_eq!(pool.resolve(&pub_lit), "payments");
    assert_eq!(pool.resolve(&sub_lit), "payments");
}
