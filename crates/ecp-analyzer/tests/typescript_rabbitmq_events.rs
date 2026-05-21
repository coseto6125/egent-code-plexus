//! T5-9 integration tests: RabbitMQ TypeScript event-topic detector.
//!
//! Exercises the production `RABBITMQ_TS` const and the real `frameworks.scm`
//! query string against amqplib and amqp-connection-manager patterns.

use ecp_analyzer::event_topic::{extract_event_topics, RABBITMQ_TS};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/typescript/frameworks.scm");
const QUERIES_SCM: &str = include_str!("../src/typescript/queries.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<ecp_core::analyzer::types::RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
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
    let result = extract_event_topics(&tree, src.as_bytes(), &query, &[RABBITMQ_TS], &imports);
    result
}

/// amqplib: channel.publish(exchange, routingKey, content) → Publish, topic="orders".
#[test]
fn test_amqplib_publish_literal_routing_key() {
    let src = r#"
import * as amqplib from 'amqplib';

async function publishOrder(data: Buffer) {
    await channel.publish('exchange', 'orders', data);
}
"#;
    let result = run(src, &["amqplib"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "orders");
}

/// amqplib: channel.consume('orders', handler) → Subscribe.
#[test]
fn test_amqplib_consume_literal_queue() {
    let src = r#"
import * as amqplib from 'amqplib';

async function consumeOrders() {
    await channel.consume('orders', (msg) => { process(msg); });
}
"#;
    let result = run(src, &["amqplib"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "orders");
}

/// amqplib: channel.assertQueue('orders') → Subscribe.
#[test]
fn test_amqplib_assert_queue_literal() {
    let src = r#"
import * as amqplib from 'amqplib';

async function setupQueue() {
    await channel.assertQueue('orders', { durable: true });
}
"#;
    let result = run(src, &["amqplib"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "orders");
}

/// amqp-connection-manager: same API, different import gate.
#[test]
fn test_amqp_connection_manager_publish() {
    let src = r#"
import { connect } from 'amqp-connection-manager';

function sendEvent() {
    channel.publish('events', 'payments', Buffer.from('data'));
}
"#;
    let result = run(src, &["amqp-connection-manager"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "payments");
}

/// Variable routing_key → no capture (no fabrication).
#[test]
fn test_variable_routing_key_emits_nothing() {
    let src = r#"
import * as amqplib from 'amqplib';

async function publishDynamic(key: string, data: Buffer) {
    await channel.publish('exchange', key, data);
}
"#;
    let result = run(src, &["amqplib"]);
    assert!(
        result.is_empty(),
        "variable routing_key must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No amqplib import → empty output (import gate enforces isolation).
#[test]
fn test_no_rabbitmq_import_emits_nothing() {
    let src = r#"
import * as redis from 'redis';

async function publish() {
    await channel.publish('exchange', 'orders', Buffer.from('x'));
}
"#;
    let result = run(src, &["redis"]);
    assert!(
        result.is_empty(),
        "non-rabbitmq import must produce nothing; got {:?}",
        result
    );
}

/// Both publish and consume in the same function → 2 topics.
#[test]
fn test_publish_and_consume_same_file() {
    let src = r#"
import * as amqplib from 'amqplib';

async function setupMessaging() {
    await channel.publish('x', 'orders', Buffer.from('data'));
    await channel.consume('orders', handler);
}
"#;
    let result = run(src, &["amqplib"]);
    assert_eq!(result.len(), 2, "expected 2 topics; got {:?}", result);
    let publish = result.iter().find(|r| r.direction == PubSub::Publish);
    let subscribe = result.iter().find(|r| r.direction == PubSub::Subscribe);
    assert!(publish.is_some(), "must have a Publish event");
    assert!(subscribe.is_some(), "must have a Subscribe event");
    let pub_lit = publish
        .unwrap()
        .topic_literal
        .as_deref()
        .expect("publish topic must be Some");
    let sub_lit = subscribe
        .unwrap()
        .topic_literal
        .as_deref()
        .expect("subscribe topic must be Some");
    assert_eq!(pub_lit, "orders");
    assert_eq!(sub_lit, "orders");
}
