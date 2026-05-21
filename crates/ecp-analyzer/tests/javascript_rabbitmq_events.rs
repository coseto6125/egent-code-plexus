//! T5-10 integration tests: RabbitMQ JavaScript event-topic detector.
//!
//! Exercises the production `RABBITMQ_JS` const and the real `frameworks.scm`
//! query string against amqplib and amqp-connection-manager patterns.

use ecp_analyzer::event_topic::{extract_event_topics, RABBITMQ_JS};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/javascript/frameworks.scm");
const QUERIES_SCM: &str = include_str!("../src/javascript/queries.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
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
        &[RABBITMQ_JS],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// amqplib: channel.publish(exchange, routingKey, content) → Publish.
#[test]
fn test_amqplib_publish_literal_routing_key() {
    let src = r#"
const amqplib = require('amqplib');

async function publishOrder(data) {
    await channel.publish('exchange', 'orders', Buffer.from(data));
}
"#;
    let (result, pool) = run(src, &["amqplib"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// amqplib: channel.consume('orders', handler) → Subscribe.
#[test]
fn test_amqplib_consume_literal_queue() {
    let src = r#"
const amqplib = require('amqplib');

async function consumeOrders() {
    await channel.consume('orders', function(msg) { process(msg); });
}
"#;
    let (result, pool) = run(src, &["amqplib"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// amqplib: channel.assertQueue('payments') → Subscribe.
#[test]
fn test_amqplib_assert_queue_literal() {
    let src = r#"
const amqplib = require('amqplib');

async function setupQueue() {
    await channel.assertQueue('payments', { durable: false });
}
"#;
    let (result, pool) = run(src, &["amqplib"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "payments");
}

/// amqp-connection-manager publish.
#[test]
fn test_connection_manager_publish() {
    let src = r#"
const amqp = require('amqp-connection-manager');

function sendNotification() {
    channel.publish('notifications', 'email', Buffer.from('hello'));
}
"#;
    let (result, pool) = run(src, &["amqp-connection-manager"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "email");
}

/// Variable routing_key → no capture (no fabrication).
#[test]
fn test_variable_routing_key_emits_nothing() {
    let src = r#"
const amqplib = require('amqplib');

async function publishDynamic(key, data) {
    await channel.publish('exchange', key, Buffer.from(data));
}
"#;
    let (result, _pool) = run(src, &["amqplib"]);
    assert!(
        result.is_empty(),
        "variable routing_key must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No rabbitmq import → empty output.
#[test]
fn test_no_rabbitmq_import_emits_nothing() {
    let src = r#"
const ioredis = require('ioredis');

async function publishOrder(data) {
    await channel.publish('exchange', 'orders', Buffer.from(data));
}
"#;
    let (result, _pool) = run(src, &["ioredis"]);
    assert!(
        result.is_empty(),
        "non-rabbitmq import must produce nothing; got {:?}",
        result
    );
}

/// sendToQueue → Subscribe direction (queue is consumer-side).
#[test]
fn test_send_to_queue_literal() {
    let src = r#"
const amqplib = require('amqplib');

function directSend() {
    channel.sendToQueue('task_queue', Buffer.from('work'));
}
"#;
    let (result, pool) = run(src, &["amqplib"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "task/queue");
}
