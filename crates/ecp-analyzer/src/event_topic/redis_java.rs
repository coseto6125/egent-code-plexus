//! `EventTopicConfig` for Redis pub/sub Java clients.
//!
//! Covers three Java Redis libraries under a single config:
//! - `spring-data-redis`: `redisTemplate.convertAndSend("channel", message)` (publish),
//!   `MessageListenerAdapter`-backed subscription wired through Spring container.
//!   The static `subscribe` side is typically handled via `@ServiceActivator` /
//!   `RedisMessageListenerContainer.addMessageListener` at runtime — the tree-sitter
//!   query captures the `MessageListenerAdapter` constructor call as a Subscribe site.
//! - `jedis`: `jedis.publish("channel", msg)` / `jedis.subscribe(listener, "channel")`.
//! - `lettuce`: `commands.publish("channel", msg)` / `commands.subscribe("channel")` /
//!   `commands.psubscribe("pattern.*")`.
//!   Lettuce also exposes `pSubscribe` (camelCase) on the reactive API; both spellings
//!   are mapped to `Subscribe`.
//!
//! Direction dispatch: `classify_redis_direction` maps `subscribe`, `psubscribe`, and
//! `pSubscribe` → `PubSub::Subscribe`; everything else defaults to `PubSub::Publish`.
//!
//! # Import gates
//! - `org.springframework.data.redis` — spring-data-redis (prefix match covers all sub-packages).
//! - `redis.clients.jedis` — Jedis.
//! - `io.lettuce.core` — Lettuce Core.
//!
//! # Fire-and-forget semantics (LLM-critical)
//! Redis pub/sub has no message durability: a publish that fires when no subscriber is
//! connected is silently dropped. This differs from Kafka (durable log) and RabbitMQ
//! (broker-queued). LLMs must not assume at-least-once delivery when reasoning about
//! Redis channel renames or subscriber availability.
//!
//! # Topic literal semantics
//! - Publish: the channel string literal (first positional string arg).
//! - Subscribe: the channel string literal (first positional string arg to `subscribe`).
//! - Psubscribe: the glob pattern string literal (first positional string arg to
//!   `psubscribe` / `pSubscribe`).
//!
//! Variable channel arguments produce no capture (no fabrication).
//!
//! # LLM-utility justification (graph-completeness criterion A)
//! Without this config `ecp impact` is blind to Redis message paths in Java services.
//! A rename of a `convertAndSend("payments", ...)` call site would show zero
//! subscribers, causing the LLM to declare the change safe when it silently breaks
//! every Jedis or Lettuce subscriber listening on `"payments"`.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for Redis pub/sub Java call sites.
///
/// Lettuce uses `pSubscribe` (camelCase) on its reactive/async API; Jedis uses
/// lowercase `psubscribe`. Both are mapped to `Subscribe`. `subscribe` is also
/// `Subscribe`. Everything else (including `convertAndSend`, `publish`) → `Publish`.
fn classify_redis_direction(raw: &str) -> PubSub {
    match raw {
        "subscribe" | "psubscribe" | "pSubscribe" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// Redis pub/sub Java detector — fires for `spring-data-redis`, `jedis`, and `lettuce` imports.
///
/// `direction_capture: "redis.direction"` binds the method identifier so
/// `classify_redis_direction` can resolve `PubSub` direction without fabrication.
///
/// `topic_capture: "redis.topic"` captures the channel name or glob pattern as a
/// raw string literal node. Non-literal args produce no capture → no `RawEventTopic`.
pub const REDIS_JAVA: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Redis,
    topic_capture: "redis.topic",
    producer_capture: "redis.fn",
    direction_capture: "redis.direction",
    import_gate: &[
        "org.springframework.data.redis",
        "redis.clients.jedis",
        "io.lettuce.core",
    ],
    direction_classifier: classify_redis_direction,
    canonicalize: true,
};
