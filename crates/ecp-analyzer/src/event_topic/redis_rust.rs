//! `EventTopicConfig` for Redis pub/sub Rust clients.
//!
//! Covers the `redis` crate (sync and tokio async variants):
//! - Sync: `let _: () = con.publish("channel", "msg")?;`
//! - Sync pubsub: `pubsub.subscribe("channel")?;` / `pubsub.psubscribe("pattern.*")?;`
//! - Async (`redis::aio`): `con.publish("channel", "msg").await?;`
//! - Async pubsub: `pubsub.subscribe("channel").await?;` / `pubsub.psubscribe("pattern.*").await?;`
//!
//! Direction dispatch: `classify_redis_direction` maps `subscribe` and `psubscribe`
//! → `PubSub::Subscribe`; everything else defaults to `PubSub::Publish`.
//!
//! # Import gate
//! - `redis` — the `redis` crate (sync and async via `redis::aio`).
//!
//! # Fire-and-forget semantics (LLM-critical)
//! Redis pub/sub has no message durability: a publish that fires when no subscriber is
//! connected is silently dropped. Unlike Kafka (durable log) and RabbitMQ
//! (broker-queued with ACK), there is no replay or retry. LLMs must not assume
//! at-least-once delivery when reasoning about Rust Redis channel renames.
//!
//! # Topic literal semantics
//! - Publish: the channel string literal (first positional arg to `publish`).
//! - Subscribe: the channel string literal (first positional arg to `subscribe`).
//! - Psubscribe: the glob pattern string literal (first positional arg to `psubscribe`).
//!
//! Variable channel arguments produce no capture (no fabrication). The tree-sitter
//! patterns anchor on `string_literal` nodes only inside `function_item` bodies.
//!
//! # LLM-utility justification (graph-completeness criterion A)
//! Without this config `ecp impact` is blind to Redis message paths in Rust services.
//! A rename of `con.publish("orders", ...)` would show zero subscribers in the graph,
//! causing the LLM to declare the change safe when it silently breaks every active
//! subscriber on `"orders"`.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for Redis pub/sub Rust call sites.
///
/// The `redis` crate uses lowercase for all methods: `publish`, `subscribe`,
/// `psubscribe`. `subscribe` and `psubscribe` → `PubSub::Subscribe`; everything
/// else (including `publish`) → `PubSub::Publish`.
fn classify_redis_direction(raw: &str) -> PubSub {
    match raw {
        "subscribe" | "psubscribe" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// Redis pub/sub Rust detector — fires for `redis` crate imports.
///
/// `direction_capture: "redis.direction"` binds the method identifier so
/// `classify_redis_direction` can resolve `PubSub` direction without fabrication.
///
/// `topic_capture: "redis.topic"` captures the channel name or glob pattern as a
/// raw string literal node. Non-literal args produce no capture → no `RawEventTopic`.
pub const REDIS_RUST: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Redis,
    topic_capture: "redis.topic",
    producer_capture: "redis.fn",
    direction_capture: "redis.direction",
    import_gate: &["redis"],
    direction_classifier: classify_redis_direction,
    canonicalize: true,
};
