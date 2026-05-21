//! `EventTopicConfig` for Redis pub/sub JavaScript clients.
//!
//! Covers node-redis v4 (`client.publish/subscribe/pSubscribe`) and ioredis
//! (`redis.publish/subscribe/psubscribe`) under a single config.
//!
//! LLM-utility: surfaces both publisher and subscriber call sites so
//! `ecp impact` can trace which JavaScript functions write to or read from a
//! given Redis channel — enabling cross-service blast-radius queries across
//! repos without manual grep.
//!
//! Fire-and-forget semantics: Redis pub/sub is ephemeral — messages are lost
//! if no subscriber is listening at publish time. The `direction` field lets
//! LLMs distinguish ephemeral publish from durable subscribe patterns so they
//! don't conflate Redis channels with queue-backed brokers (Kafka, RabbitMQ).
//!
//! Import gate: `redis`, `ioredis`.
//! Tree-sitter capture names: `redis.topic`, `redis.fn`, `redis.direction`.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Maps raw direction-capture text to `PubSub`.
///
/// Both camelCase (node-redis v4: `pSubscribe`) and lowercase (ioredis:
/// `psubscribe`) variants are handled. Unrecognised text defaults to
/// `Publish` so the topic is indexed rather than silently dropped.
fn classify_redis_direction(raw: &str) -> PubSub {
    match raw {
        "subscribe" | "psubscribe" | "pSubscribe" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// Redis pub/sub JavaScript detector — fires for `redis` and `ioredis` imports.
///
/// LLM-utility: surfaces publish and subscribe call sites so `ecp impact` can
/// trace JavaScript functions that communicate over Redis channels — enabling
/// cross-service blast-radius queries without manual grep across repo
/// boundaries.
///
/// Fire-and-forget note: Redis pub/sub is ephemeral. The `direction` field
/// distinguishes publish from subscribe so LLMs don't confuse Redis channels
/// with durable queue-backed brokers.
pub const REDIS_JS: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Redis,
    topic_capture: "redis.topic",
    producer_capture: "redis.fn",
    direction_capture: "redis.direction",
    import_gate: &["redis", "ioredis"],
    direction_classifier: classify_redis_direction,
    canonicalize: true,
};
