//! T5-2 integration tests: Kafka Python event-topic detector.
//!
//! Exercises the production `KAFKA_PYTHON` const and the real `frameworks.scm`
//! query string — a typo in either path breaks these tests immediately.

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_PYTHON};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/python/frameworks.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
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
    let mut pool = StringPool::new();
    let result = extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[KAFKA_PYTHON],
        &imports,
        &mut pool,
    );
    (result, pool)
}

#[test]
fn test_kafka_producer_send_literal_topic() {
    let src = r#"
from kafka import KafkaProducer

def publish_order(data):
    p = KafkaProducer(bootstrap_servers="localhost:9092")
    p.send("orders", b"x")
"#;
    let (result, pool) = run(src, &["kafka"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// Non-literal topic variable — extractor refuses to fabricate.
#[test]
fn test_kafka_variable_topic_emits_nothing() {
    let src = r#"
from kafka import KafkaProducer

def publish():
    topic = "orders"
    producer = KafkaProducer()
    producer.send(topic, b"payload")
"#;
    let (result, _pool) = run(src, &["kafka"]);
    assert!(
        result.is_empty(),
        "variable topic must not produce a RawEventTopic"
    );
}

#[test]
fn test_no_kafka_import_no_captures() {
    let src = r#"
import json

def publish():
    result = json.dumps({"key": "value"})
"#;
    let (result, _pool) = run(src, &["json"]);
    assert!(result.is_empty(), "non-kafka import must produce nothing");
}

#[test]
fn test_aiokafka_producer_send_literal() {
    let src = r#"
from aiokafka import AIOKafkaProducer

async def send_payment(payload):
    producer = AIOKafkaProducer(bootstrap_servers="localhost:9092")
    await producer.send("payments", payload)
"#;
    let (result, pool) = run(src, &["aiokafka"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic from aiokafka");
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "payments");
}

#[test]
fn test_confluent_kafka_produce_literal() {
    let src = r#"
from confluent_kafka import Producer

def emit_event(data):
    p = Producer({"bootstrap.servers": "localhost"})
    p.produce("events", data.encode())
"#;
    let (result, pool) = run(src, &["confluent_kafka"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from confluent_kafka"
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "events");
}

/// Both kafka and aiokafka in scope, async fn with `await`. Sync pattern's
/// `(_ (call ...))` wildcard must NOT also match the `await` form — otherwise
/// the same call site would emit twice.
#[test]
fn test_await_send_does_not_double_emit_under_both_imports() {
    let src = r#"
from kafka import KafkaProducer
from aiokafka import AIOKafkaProducer

async def send_event(payload):
    producer = AIOKafkaProducer()
    await producer.send("billing", payload)
"#;
    let (result, _pool) = run(src, &["kafka", "aiokafka"]);
    assert_eq!(
        result.len(),
        1,
        "await producer.send must emit exactly one RawEventTopic; got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
}
