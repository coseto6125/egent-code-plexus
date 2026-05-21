//! T5-27 integration tests: Redis TypeScript event-topic detector.
//!
//! Exercises the production `REDIS_TS` const and the real `frameworks.scm`
//! query string — a typo in either path breaks these tests immediately.
//!
//! Topic values in assertions use the canonical form because REDIS_TS sets
//! `canonicalize: true`.

use ecp_analyzer::event_topic::{extract_event_topics, REDIS_TS};
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
    extract_event_topics(&tree, src.as_bytes(), &query, &[REDIS_TS], &imports)
}

/// node-redis v4: `await client.publish('channel', msg)` — Publish.
#[test]
fn test_node_redis_await_publish_literal() {
    let src = r#"
import { createClient } from 'redis';

async function notifyShipped(orderId) {
    await client.publish('order.shipped', orderId);
}
"#;
    let result = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "order/shipped"
    );
}

/// node-redis v4: `await client.subscribe('channel', handler)` — Subscribe.
#[test]
fn test_node_redis_await_subscribe_literal() {
    let src = r#"
import { createClient } from 'redis';

async function listenForOrders(handler) {
    await client.subscribe('order.created', handler);
}
"#;
    let result = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "order/created"
    );
}

/// node-redis v4: `await client.pSubscribe('pattern.*', handler)` — Subscribe (camelCase).
#[test]
fn test_node_redis_await_p_subscribe_pattern_literal() {
    let src = r#"
import { createClient } from 'redis';

async function listenPatterns(handler) {
    await client.pSubscribe('order.*', handler);
}
"#;
    let result = run(src, &["redis"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    // canonicalize strips the `.*` suffix and lowercases
    assert!(
        lit.starts_with("order"),
        "channel should start with 'order'; got {}",
        lit
    );
}

/// ioredis: `redis.publish('channel', msg)` — Publish.
#[test]
fn test_ioredis_publish_literal() {
    let src = r#"
import Redis from 'ioredis';

function emitPayment(amount) {
    redis.publish('payment.processed', JSON.stringify({ amount }));
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
        "payment/processed"
    );
}

/// ioredis: `redis.subscribe('channel')` — Subscribe.
#[test]
fn test_ioredis_subscribe_literal() {
    let src = r#"
import Redis from 'ioredis';

function startConsumer() {
    redis.subscribe('payment.processed');
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
        "payment/processed"
    );
}

/// ioredis: `redis.psubscribe('pattern.*')` — Subscribe (lowercase, ioredis convention).
#[test]
fn test_ioredis_psubscribe_lowercase_literal() {
    let src = r#"
import Redis from 'ioredis';

function watchAll() {
    redis.psubscribe('payment.*');
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
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert!(
        lit.starts_with("payment"),
        "channel should start with 'payment'; got {}",
        lit
    );
}

/// Variable channel — extractor refuses to fabricate.
#[test]
fn test_variable_channel_emits_nothing() {
    let src = r#"
import { createClient } from 'redis';

async function publish(channelName, msg) {
    await client.publish(channelName, msg);
}
"#;
    let result = run(src, &["redis"]);
    assert!(
        result.is_empty(),
        "variable channel must not produce a RawEventTopic"
    );
}

/// No redis or ioredis import — import gate must reject.
#[test]
fn test_no_redis_import_no_captures() {
    let src = r#"
import express from 'express';

async function handler(req, res) {
    await client.publish('events', 'data');
}
"#;
    let result = run(src, &["express"]);
    assert!(result.is_empty(), "non-redis import must produce nothing");
}

/// node-redis v4 async method inside a class — captures method name.
#[test]
fn test_node_redis_async_method_publish_captures_method_name() {
    let src = r#"
import { createClient } from 'redis';

class NotificationService {
    async sendAlert(message) {
        await this.client.publish('alerts', message);
    }
}
"#;
    let result = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from async class method"
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "alerts"
    );
}
