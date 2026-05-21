//! T5-28 integration tests: Redis JavaScript pub/sub event-topic detector.
//!
//! Exercises the production `REDIS_JS` const and the real `frameworks.scm`
//! query string — a typo in either path breaks these tests immediately.
//!
//! Libraries under test: node-redis v4 (`redis` import), ioredis (`ioredis`).

use ecp_analyzer::event_topic::{extract_event_topics, REDIS_JS};
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
    extract_event_topics(&tree, src.as_bytes(), &query, &[REDIS_JS], &imports)
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
    let result = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    // canonicalize: '.' → '/' → "orders/created"
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders/created"
    );
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
    let result = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    // canonicalize: '.' → '/' → "orders/created"
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders/created"
    );
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
    let result = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic for pSubscribe");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    // canonicalize: '.' → '/' → "orders/*"
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders/*"
    );
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
    let result = run(src, &["ioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from ioredis publish"
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "notifications"
    );
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
    let result = run(src, &["ioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from ioredis subscribe"
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
    let result = run(src, &["ioredis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from ioredis psubscribe"
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    // canonicalize: '.' → '/' → "payments/*"
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "payments/*"
    );
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
    let result = run(src, &["redis"]);
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
    let result = run(src, &["express"]);
    assert!(result.is_empty(), "non-redis import must produce nothing");
}

// Kafka JS regression dropped: KAFKA_JS does not yet exist on `main`; add back
// when the Kafka JS detector PR lands.
