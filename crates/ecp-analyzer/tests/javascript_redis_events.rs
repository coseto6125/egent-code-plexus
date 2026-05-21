//! T5-28 integration tests: Redis JavaScript pub/sub event-topic detector.
//!
//! Exercises the production `REDIS_JS` const and the real `frameworks.scm`
//! query string — a typo in either path breaks these tests immediately.
//!
//! Libraries under test: node-redis v4 (`redis` import), ioredis (`ioredis`).

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_JS, REDIS_JS};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/javascript/frameworks.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
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
    let mut pool = StringPool::new();
    let result = extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[KAFKA_JS, REDIS_JS],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// node-redis v4: `await client.publish('channel', msg)` → Publish
///
/// Channel `orders.created` → canonicalized to `orders/created` (dot → slash per normalize.rs).
#[test]
fn test_node_redis_await_publish_literal() {
    let src = r#"
import { createClient } from 'redis';

async function notifyOrder(msg) {
    await client.publish('orders.created', msg);
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    // canonicalize: '.' → '/' → "orders/created"
    assert_eq!(pool.resolve(&lit), "orders/created");
}

/// node-redis v4: `await client.subscribe('channel', handler)` → Subscribe
///
/// Channel `orders.created` → canonicalized to `orders/created`.
#[test]
fn test_node_redis_await_subscribe_literal() {
    let src = r#"
import { createClient } from 'redis';

async function listenForOrders() {
    await subscriber.subscribe('orders.created', (msg) => console.log(msg));
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    // canonicalize: '.' → '/' → "orders/created"
    assert_eq!(pool.resolve(&lit), "orders/created");
}

/// node-redis v4: `await client.pSubscribe('pattern.*', handler)` → Subscribe (camelCase)
///
/// Pattern `orders.*` → canonicalized to `orders/*` (dot → slash).
#[test]
fn test_node_redis_await_psubscribe_camel_literal() {
    let src = r#"
import { createClient } from 'redis';

async function watchAll() {
    await subscriber.pSubscribe('orders.*', (msg) => process(msg));
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic for pSubscribe");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    // canonicalize: '.' → '/' → "orders/*"
    assert_eq!(pool.resolve(&lit), "orders/*");
}

/// ioredis: `redis.publish('channel', msg)` → Publish (sync, no await)
#[test]
fn test_ioredis_sync_publish_literal() {
    let src = r#"
import Redis from 'ioredis';

function sendNotification(msg) {
    redis.publish('notifications', msg);
}
"#;
    let (result, pool) = run(src, &["ioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from ioredis publish"
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "notifications");
}

/// ioredis: `redis.subscribe('channel')` → Subscribe (sync form)
#[test]
fn test_ioredis_sync_subscribe_literal() {
    let src = r#"
import Redis from 'ioredis';

function startListener() {
    subscriber.subscribe('payments');
}
"#;
    let (result, pool) = run(src, &["ioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from ioredis subscribe"
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "payments");
}

/// ioredis: `redis.psubscribe('pattern.*')` → Subscribe (lowercase)
///
/// Pattern `payments.*` → canonicalized to `payments/*` (dot → slash).
#[test]
fn test_ioredis_sync_psubscribe_lowercase_literal() {
    let src = r#"
import Redis from 'ioredis';

function watchPayments() {
    subscriber.psubscribe('payments.*');
}
"#;
    let (result, pool) = run(src, &["ioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from ioredis psubscribe"
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    // canonicalize: '.' → '/' → "payments/*"
    assert_eq!(pool.resolve(&lit), "payments/*");
}

/// Variable channel → extractor refuses to fabricate.
#[test]
fn test_variable_channel_emits_nothing() {
    let src = r#"
import { createClient } from 'redis';

async function broadcast(channel, msg) {
    await client.publish(channel, msg);
}
"#;
    let (result, _pool) = run(src, &["redis"]);
    assert!(
        result.is_empty(),
        "variable channel must not produce a RawEventTopic"
    );
}

/// No redis/ioredis import → import gate blocks all captures.
#[test]
fn test_no_redis_import_emits_nothing() {
    let src = r#"
import express from 'express';

async function handler(req, res) {
    await client.publish('orders', req.body);
}
"#;
    let (result, _pool) = run(src, &["express"]);
    assert!(result.is_empty(), "non-redis import must produce nothing");
}

/// Kafka regression: REDIS_JS in config slice must not break KAFKA_JS captures.
#[test]
fn test_kafka_regression_with_redis_in_slice() {
    let src = r#"
import { Kafka } from 'kafkajs';

async function publishOrder(data) {
    const producer = kafka.producer();
    await producer.send({ topic: 'orders', messages: [{ value: JSON.stringify(data) }] });
}
"#;
    let (result, pool) = run(src, &["kafkajs"]);
    assert_eq!(
        result.len(),
        1,
        "Kafka detection must still work with REDIS_JS in slice"
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}
