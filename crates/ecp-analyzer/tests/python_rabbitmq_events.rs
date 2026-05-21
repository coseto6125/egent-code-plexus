//! T5-8 integration tests: RabbitMQ Python event-topic detector.
//!
//! Exercises the production `RABBITMQ_PYTHON` const and the real `frameworks.scm`
//! query string against pika, aio_pika, and kombu patterns.

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_PYTHON, RABBITMQ_PYTHON};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/python/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<ecp_core::analyzer::types::RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(&lang, FRAMEWORKS_SCM).expect("query compile");
    let imports: Vec<RawImport> = import_sources
        .iter()
        .map(|s| RawImport {
            source: (*s).to_string(),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    let result = extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[KAFKA_PYTHON, RABBITMQ_PYTHON],
        &imports,
    );
    result
}

/// pika basic_publish with literal routing_key → Publish direction, topic="orders".
#[test]
fn test_pika_basic_publish_literal_routing_key() {
    let src = r#"
import pika

def publish_order(data):
    channel.basic_publish(exchange='', routing_key='orders', body=data.encode())
"#;
    let result = run(src, &["pika"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "orders");
}

/// pika basic_consume with literal queue → Subscribe direction, topic="orders".
#[test]
fn test_pika_basic_consume_literal_queue() {
    let src = r#"
import pika

def consume_orders():
    channel.basic_consume(queue='orders', on_message_callback=callback, auto_ack=True)
"#;
    let result = run(src, &["pika"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "orders");
}

/// aio_pika async exchange.publish with literal routing_key → Publish direction.
#[test]
fn test_aio_pika_exchange_publish_literal_routing_key() {
    let src = r#"
import aio_pika

async def send_payment(payload):
    await exchange.publish(
        aio_pika.Message(body=payload),
        routing_key='payments',
    )
"#;
    let result = run(src, &["aio_pika"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from aio_pika publish; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "payments");
}

/// kombu producer.publish with literal routing_key → Publish direction.
#[test]
fn test_kombu_producer_publish_literal_routing_key() {
    let src = r#"
from kombu import Producer

def emit_event(producer, body):
    producer.publish(body, routing_key='events', exchange='my_exchange')
"#;
    let result = run(src, &["kombu"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from kombu publish; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "events");
}

/// Variable routing_key → no capture (no fabrication).
#[test]
fn test_pika_variable_routing_key_emits_nothing() {
    let src = r#"
import pika

def publish_dynamic(data, routing_key):
    channel.basic_publish(exchange='', routing_key=routing_key, body=data)
"#;
    let result = run(src, &["pika"]);
    assert!(
        result.is_empty(),
        "variable routing_key must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No RabbitMQ import → empty output (import gate enforces isolation).
#[test]
fn test_no_rabbitmq_import_no_captures() {
    let src = r#"
import json

def publish():
    result = json.dumps({"routing_key": "orders"})
"#;
    let result = run(src, &["json"]);
    assert!(
        result.is_empty(),
        "non-rabbitmq import must produce nothing; got {:?}",
        result
    );
}

/// Both publish and consume in the same file → 2 RawEventTopics with correct directions.
#[test]
fn test_pika_publish_and_consume_same_file() {
    let src = r#"
import pika

def publish_order(data):
    channel.basic_publish(exchange='', routing_key='orders', body=data.encode())

def consume_orders():
    channel.basic_consume(queue='orders', on_message_callback=callback)
"#;
    let result = run(src, &["pika"]);
    assert_eq!(
        result.len(),
        2,
        "expected 2 RawEventTopics (publish + subscribe); got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
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

/// Kafka import with KAFKA_PYTHON in the same config slice must not fire RabbitMQ patterns,
/// and vice versa — import gates provide full isolation.
#[test]
fn test_kafka_import_does_not_fire_rabbitmq_config() {
    let src = r#"
from kafka import KafkaProducer

def publish(data):
    channel.basic_publish(exchange='', routing_key='orders', body=data)
"#;
    // Only kafka import → RABBITMQ_PYTHON gate must stay closed.
    let result = run(src, &["kafka"]);
    // KAFKA_PYTHON will not match basic_publish (no "send" call), so result is empty.
    assert!(
        result.is_empty() || result.iter().all(|r| r.lib == FrameworkId::Kafka),
        "rabbitmq config must not fire under kafka-only import; got {:?}",
        result
            .iter()
            .map(|r| (
                r.lib,
                r.direction,
                r.topic_literal.as_ref().map(|t| t.to_string())
            ))
            .collect::<Vec<_>>()
    );
}
