//! T5-31 integration tests: Redis pub/sub Rust event-topic detector.
//!
//! Exercises the production `REDIS_RUST` const and the real `frameworks.scm`
//! query string against the `redis` crate (sync and async variants).

use ecp_analyzer::event_topic::{extract_event_topics, REDIS_RUST};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/rust/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/rust/frameworks.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
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
            imported_name: s.split("::").last().unwrap_or("*").to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    let mut pool = StringPool::new();
    let result = extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[REDIS_RUST],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// redis crate: con.publish("orders", "msg") → Publish, topic="orders".
#[test]
fn test_redis_publish_literal_channel() {
    let src = r#"
use redis::Commands;

fn publish_order(con: &mut redis::Connection) -> redis::RedisResult<()> {
    let _: () = con.publish("orders", "data")?;
    Ok(())
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// redis crate: pubsub.subscribe("payments") → Subscribe, topic="payments".
#[test]
fn test_redis_subscribe_literal_channel() {
    let src = r#"
use redis::Commands;

fn listen_payments(con: &mut redis::Connection) -> redis::RedisResult<()> {
    let mut pubsub = con.as_pubsub();
    pubsub.subscribe("payments")?;
    Ok(())
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "payments");
}

/// redis crate: pubsub.psubscribe("orders.*") → Subscribe, pattern stored.
#[test]
fn test_redis_psubscribe_literal_pattern() {
    let src = r#"
use redis::Commands;

fn listen_pattern(con: &mut redis::Connection) -> redis::RedisResult<()> {
    let mut pubsub = con.as_pubsub();
    pubsub.psubscribe("orders.*")?;
    Ok(())
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic from psubscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    // canonicalize: true converts "orders.*" → "orders/*"
    assert_eq!(pool.resolve(&lit), "orders/*");
}

/// Async redis: con.publish("events", "msg").await — Publish direction.
#[test]
fn test_redis_async_publish_literal_channel() {
    let src = r#"
use redis::AsyncCommands;

async fn publish_event(con: &mut redis::aio::Connection) -> redis::RedisResult<()> {
    let _: () = con.publish("events", "payload").await?;
    Ok(())
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic from async publish; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "events");
}

/// Variable channel → no capture (no fabrication).
#[test]
fn test_redis_variable_channel_emits_nothing() {
    let src = r#"
use redis::Commands;

fn publish_dynamic(con: &mut redis::Connection, channel: &str, msg: &str) -> redis::RedisResult<()> {
    let _: () = con.publish(channel, msg)?;
    Ok(())
}
"#;
    let (result, _pool) = run(src, &["redis"]);
    assert!(
        result.is_empty(),
        "variable channel must produce nothing; got {:?}",
        result
    );
}

/// No redis import → empty output (import gate).
#[test]
fn test_no_redis_import_no_captures() {
    let src = r#"
fn process(channel: &str) {
    println!("channel: {}", channel);
}
"#;
    let (result, _pool) = run(src, &["std"]);
    assert!(
        result.is_empty(),
        "non-redis import must produce nothing; got {:?}",
        result
    );
}

/// Async redis: pubsub.subscribe("notifications").await — Subscribe direction.
#[test]
fn test_redis_async_subscribe_literal_channel() {
    let src = r#"
use redis::aio::PubSub;

async fn listen_notifications(mut pubsub: redis::aio::PubSub) -> redis::RedisResult<()> {
    pubsub.subscribe("notifications").await?;
    Ok(())
}
"#;
    let (result, pool) = run(src, &["redis"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic from async subscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "notifications");
}
