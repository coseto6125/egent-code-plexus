//! T5-4 integration tests: Kafka JavaScript event-topic detector.
//!
//! Exercises the production `KAFKA_NODE` const and the real `frameworks.scm`
//! query string — a typo in either path breaks these tests immediately.

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_NODE};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/javascript/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
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

async function publishOrder(data) {
    const producer = kafka.producer();
    await producer.send({ topic: 'orders', messages: [{ value: JSON.stringify(data) }] });
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
import { Producer } from 'node-rdkafka';

function emitPayment(payload) {
    producer.produce('payments', null, Buffer.from(payload));
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

/// Non-literal topic variable — extractor refuses to fabricate.
#[test]
fn test_variable_topic_emits_nothing() {
    let src = r#"
import { Kafka } from 'kafkajs';

function publish() {
    const topic = 'orders';
    producer.send({ topic: topic, messages: [] });
}
"#;
    let result = run(src, &["kafkajs"]);
    assert!(
        result.is_empty(),
        "variable topic must not produce a RawEventTopic"
    );
}

#[test]
fn test_no_kafka_import_no_captures() {
    let src = r#"
import express from 'express';

function handleRequest(req, res) {
    res.send({ topic: 'orders', messages: [] });
}
"#;
    let result = run(src, &["express"]);
    assert!(result.is_empty(), "non-kafka import must produce nothing");
}

/// Both kafkajs and node-rdkafka present — both producer patterns captured.
#[test]
fn test_both_libraries_in_same_file() {
    let src = r#"
import { Kafka } from 'kafkajs';
import { Producer } from 'node-rdkafka';

function sendViaKafkaJs(data) {
    producer.send({ topic: 'billing', messages: [{ value: data }] });
}

function sendViaRdkafka(payload) {
    rdProducer.produce('events', null, Buffer.from(payload));
}
"#;
    let result = run(src, &["kafkajs", "node-rdkafka"]);
    assert_eq!(
        result.len(),
        2,
        "expected two RawEventTopics (one per library pattern)"
    );
    let topics: Vec<&str> = result
        .iter()
        .map(|r| r.topic_literal.as_deref().expect("topic_literal"))
        .collect();
    assert!(topics.contains(&"billing"), "missing 'billing' topic");
    assert!(topics.contains(&"events"), "missing 'events' topic");
    for r in &result {
        assert_eq!(r.lib, FrameworkId::Kafka);
        assert_eq!(r.direction, PubSub::Publish);
    }
}

/// Method definition inside a class — enclosing fn is the method name.
#[test]
fn test_kafkajs_send_inside_method() {
    let src = r#"
import { Kafka } from 'kafkajs';

class OrderService {
    async publishOrder(order) {
        await this.producer.send({ topic: 'orders', messages: [{ value: JSON.stringify(order) }] });
    }
}
"#;
    let result = run(src, &["kafkajs"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic from method");
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
}
