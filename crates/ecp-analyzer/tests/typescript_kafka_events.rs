//! T5-3 integration tests: Kafka TypeScript event-topic detector.
//!
//! Exercises the production `KAFKA_NODE` const and the real `frameworks.scm`
//! query string — a typo in either path breaks these tests immediately.
//!
//! Topic values in assertions use the canonical form (hyphens/underscores → `/`,
//! lowercase) because KAFKA_NODE sets `canonicalize: true`.

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_NODE};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/typescript/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
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
    extract_event_topics(&tree, src.as_bytes(), &query, &[KAFKA_NODE], &imports)
}

#[test]
fn test_kafkajs_producer_send_literal_topic() {
    let src = r#"
import { Kafka } from 'kafkajs';

function publishOrder(data) {
    const producer = kafka.producer();
    producer.send({ topic: 'orders', messages: [{ value: JSON.stringify(data) }] });
}
"#;
    let result = run(src, &["kafkajs"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
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

#[test]
fn test_node_rdkafka_produce_literal_topic() {
    let src = r#"
import Kafka from 'node-rdkafka';

function emitPayment(payload) {
    producer.produce('payments', -1, Buffer.from(JSON.stringify(payload)));
}
"#;
    let result = run(src, &["node-rdkafka"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from node-rdkafka"
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

/// Dynamic topic variable — extractor refuses to fabricate.
#[test]
fn test_kafkajs_variable_topic_emits_nothing() {
    let src = r#"
import { Kafka } from 'kafkajs';

function publish(topicName) {
    producer.send({ topic: topicName, messages: [{ value: 'x' }] });
}
"#;
    let result = run(src, &["kafkajs"]);
    assert!(
        result.is_empty(),
        "variable topic must not produce a RawEventTopic"
    );
}

/// No kafkajs or node-rdkafka import — import gate must reject.
#[test]
fn test_no_kafka_import_no_captures() {
    let src = r#"
import express from 'express';

function handleRequest(req, res) {
    producer.send({ topic: 'orders', messages: [] });
}
"#;
    let result = run(src, &["express"]);
    assert!(result.is_empty(), "non-kafka import must produce nothing");
}

/// Both libraries imported in the same file — each call site captured once.
/// Topic literals use single words so canonicalize() is a no-op.
#[test]
fn test_both_libraries_imported_both_captured() {
    let src = r#"
import { Kafka } from 'kafkajs';
import Rdkafka from 'node-rdkafka';

function publishKafkaJs(data) {
    kafkaProducer.send({ topic: 'events', messages: [{ value: 'a' }] });
}

function publishRdkafka(data) {
    rdProducer.produce('billing', -1, Buffer.from('b'));
}
"#;
    let result = run(src, &["kafkajs", "node-rdkafka"]);
    let topics: Vec<&str> = result
        .iter()
        .map(|r| r.topic_literal.as_deref().unwrap())
        .collect();
    assert_eq!(
        result.len(),
        2,
        "one capture per library call site; got {:?}",
        topics
    );
    assert!(topics.contains(&"events"), "kafkajs topic must appear");
    assert!(
        topics.contains(&"billing"),
        "node-rdkafka topic must appear"
    );
}

/// kafkajs `await producer.send(...)` inside an async class method — captures the method name.
#[test]
fn test_kafkajs_async_method_definition_captures_fn_name() {
    let src = r#"
import { Kafka } from 'kafkajs';

class OrderService {
    async publishOrder(data) {
        await this.producer.send({ topic: 'orders', messages: [{ value: 'x' }] });
    }
}
"#;
    let result = run(src, &["kafkajs"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from async method"
    );
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
}
