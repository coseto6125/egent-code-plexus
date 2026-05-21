//! T5-30 integration tests: Redis pub/sub Go event-topic detector.
//!
//! Exercises the production `REDIS_GO` const and the real `frameworks.scm`
//! query string against go-redis (v8/v9) and gomodule/redigo patterns.

use ecp_analyzer::event_topic::{extract_event_topics, REDIS_GO};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/go/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/go/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
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
            imported_name: s.split('/').next_back().unwrap_or("*").to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    extract_event_topics(&tree, src.as_bytes(), &query, &[REDIS_GO], &imports)
}

/// go-redis: client.Publish(ctx, "orders", msg) → Publish, topic="orders".
#[test]
fn test_goredis_publish_literal_channel() {
    let src = r#"package main

import "github.com/redis/go-redis/v9"

func publishOrder(client *redis.Client, ctx context.Context, data string) {
    client.Publish(ctx, "orders", data)
}
"#;
    let result = run(src, &["github.com/redis/go-redis/v9"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic; got {:?}",
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

/// go-redis: client.Subscribe(ctx, "payments") → Subscribe, topic="payments".
#[test]
fn test_goredis_subscribe_literal_channel() {
    let src = r#"package main

import "github.com/redis/go-redis/v9"

func listenPayments(client *redis.Client, ctx context.Context) {
    pubsub := client.Subscribe(ctx, "payments")
    _ = pubsub
}
"#;
    let result = run(src, &["github.com/redis/go-redis/v9"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic; got {:?}",
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

/// go-redis: client.PSubscribe(ctx, "orders.*") → Subscribe, pattern stored.
#[test]
fn test_goredis_psubscribe_literal_pattern() {
    let src = r#"package main

import "github.com/redis/go-redis/v9"

func listenPattern(client *redis.Client, ctx context.Context) {
    pubsub := client.PSubscribe(ctx, "orders.*")
    _ = pubsub
}
"#;
    let result = run(src, &["github.com/redis/go-redis/v9"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic from PSubscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    // canonicalize: true converts "orders.*" → "orders/*"
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders/*"
    );
}

/// redigo: psc.Subscribe("events") → Subscribe, topic="events".
#[test]
fn test_redigo_subscribe_literal_channel() {
    let src = r#"package main

import "github.com/gomodule/redigo/redis"

func listenEvents(psc redis.PubSubConn) {
    psc.Subscribe("events")
}
"#;
    let result = run(src, &["github.com/gomodule/redigo/redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic from redigo Subscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "events"
    );
}

/// Variable channel → no capture (no fabrication).
#[test]
fn test_goredis_variable_channel_emits_nothing() {
    let src = r#"package main

import "github.com/redis/go-redis/v9"

func publishDynamic(client *redis.Client, ctx context.Context, channel string, data string) {
    client.Publish(ctx, channel, data).Err()
}
"#;
    let result = run(src, &["github.com/redis/go-redis/v9"]);
    assert!(
        result.is_empty(),
        "variable channel must produce nothing; got {:?}",
        result
    );
}

/// No redis import → empty output (import gate).
#[test]
fn test_no_redis_import_no_captures() {
    let src = r#"package main

import "fmt"

func process(channel string) {
    fmt.Println(channel)
}
"#;
    let result = run(src, &["fmt"]);
    assert!(
        result.is_empty(),
        "non-redis import must produce nothing; got {:?}",
        result
    );
}
