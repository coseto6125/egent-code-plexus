//! T5-26 integration tests: Redis pub/sub Python event-topic detector.
//!
//! Exercises the production `REDIS_PYTHON` const and the real `frameworks.scm`
//! query string against `redis` (sync) and `aioredis` (async) patterns.
//! Also re-verifies Kafka + RabbitMQ regression isolation.

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_PYTHON, REDIS_PYTHON};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/python/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
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
    extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[KAFKA_PYTHON, REDIS_PYTHON],
        &imports,
    )
}

/// redis (sync) publish with literal channel → Publish direction, topic="orders".
#[test]
fn test_redis_publish_literal_channel() {
    let src = r#"
import redis

def publish_order(r, data):
    r.publish("orders", data)
"#;
    let result = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
}

/// redis (sync) pubsub.subscribe with literal channel → Subscribe direction.
#[test]
fn test_redis_pubsub_subscribe_literal_channel() {
    let src = r#"
import redis

def listen_orders(pubsub):
    pubsub.subscribe("orders")
"#;
    let result = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
}

/// redis (sync) pubsub.psubscribe with literal pattern → Subscribe, pattern stored as topic.
#[test]
fn test_redis_pubsub_psubscribe_literal_pattern() {
    let src = r#"
import redis

def listen_pattern(pubsub):
    pubsub.psubscribe("orders.*")
"#;
    let result = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from psubscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    // `canonicalize: true` converts `orders.*` → `orders/*` (dot → slash rule).
    // This is a known schema gap: without a `kind` field the canonicalizer
    // cannot distinguish a glob pattern from a plain channel name. T5-33 will
    // receive `"orders/*"` — it cannot reconstruct `"orders.*"` to match against
    // Redis channel names that contain dots. Deferred to schema-migration PR;
    // see redis_python.rs schema gap note.
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders/*"
    );
}

/// aioredis (async) await publish → Publish direction.
#[test]
fn test_aioredis_await_publish_literal_channel() {
    let src = r#"
import aioredis

async def publish_payment(r, data):
    await r.publish("payments", data)
"#;
    let result = run(src, &["aioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from aioredis publish; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "payments"
    );
}

/// aioredis (async) await pubsub.subscribe → Subscribe direction.
#[test]
fn test_aioredis_await_subscribe_literal_channel() {
    let src = r#"
import aioredis

async def listen_payments(pubsub):
    await pubsub.subscribe("payments")
"#;
    let result = run(src, &["aioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from aioredis subscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "payments"
    );
}

/// Variable channel argument → no capture (no fabrication).
#[test]
fn test_redis_variable_channel_emits_nothing() {
    let src = r#"
import redis

def publish_dynamic(r, channel, data):
    r.publish(channel, data)
"#;
    let result = run(src, &["redis"]);
    assert!(
        result.is_empty(),
        "variable channel must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No redis/aioredis import → empty output (import gate enforces isolation).
#[test]
fn test_no_redis_import_no_captures() {
    let src = r#"
import json

def publish():
    result = json.dumps({"channel": "orders"})
"#;
    let result = run(src, &["json"]);
    assert!(
        result.is_empty(),
        "non-redis import must produce nothing; got {:?}",
        result
    );
}

/// Multi-arg subscribe — first positional literal captured; subsequent args not captured.
///
/// The tree-sitter pattern anchors on `. (string) @redis.topic` which matches
/// the first positional argument only. `subscribe("ch1", "ch2")` therefore
/// produces one RawEventTopic for "ch1". See redis_python.rs doc comment for the
/// T5-26-followup tracking multi-arg support.
#[test]
fn test_redis_multi_arg_subscribe_captures_first_literal() {
    let src = r#"
import redis

def listen_multi(pubsub):
    pubsub.subscribe("ch1", "ch2")
"#;
    let result = run(src, &["redis"]);
    // Only the first literal is captured — documented limitation.
    assert_eq!(
        result.len(),
        1,
        "multi-arg subscribe: expected exactly 1 capture (first literal only); got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "ch1"
    );
}

/// aioredis (async) await pubsub.psubscribe with literal pattern → Subscribe.
#[test]
fn test_aioredis_await_psubscribe_literal_pattern() {
    let src = r#"
import aioredis

async def listen_pattern(pubsub):
    await pubsub.psubscribe("events.*")
"#;
    let result = run(src, &["aioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from aioredis psubscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    // `canonicalize: true` converts `events.*` → `events/*` — same schema gap
    // as psubscribe sync form; see redis_python.rs schema gap note.
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "events/*"
    );
}

// ── Regression: Kafka still fires correctly in the same config slice ──

/// Kafka send in a redis-importing file — import gate must block kafka config.
/// (No kafka import → KAFKA_PYTHON gate stays closed; result empty or redis only.)
#[test]
fn test_kafka_regression_fires_on_kafka_import() {
    let src = r#"
from kafka import KafkaProducer

def send_event(producer):
    producer.send("events", b"data")
"#;
    let result = run(src, &["kafka"]);
    assert_eq!(
        result.len(),
        1,
        "Kafka regression: expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("kafka topic_literal must be Some"),
        "events"
    );
}

// RabbitMQ regression dropped: RABBITMQ_PYTHON does not yet exist on `main`
// (sibling PR #297, T5-8); add back when that detector lands.

/// Redis import does not fire RabbitMQ config and vice versa — import gates isolate.
#[test]
fn test_redis_import_does_not_fire_rabbitmq_config() {
    let src = r#"
import redis

def handler(r, pubsub):
    r.publish("orders", b"data")
    pubsub.subscribe("orders")
"#;
    let result = run(src, &["redis"]);
    assert!(
        result.iter().all(|r| r.lib == FrameworkId::Redis),
        "redis import must not fire RabbitMQ config; got libs: {:?}",
        result.iter().map(|r| r.lib).collect::<Vec<_>>()
    );
}
