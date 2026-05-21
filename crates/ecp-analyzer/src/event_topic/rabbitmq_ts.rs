//! `EventTopicConfig` for RabbitMQ / AMQP TypeScript clients.
//!
//! Covers two TypeScript / Node.js RabbitMQ libraries:
//! - `amqplib`: `await channel.publish(exchange, routingKey, content)`,
//!   `await channel.assertQueue('orders')`,
//!   `await channel.consume('orders', handler)`.
//! - `amqp-connection-manager`: same surface API as amqplib; wrapped in
//!   a `ChannelWrapper` but the method names are identical.
//!
//! # Topic literal semantics
//! - `publish(exchange, routingKey, ...)` → `routingKey` (second positional
//!   string literal) is the topic. If routingKey is a variable, no capture.
//! - `consume(queue, ...)` → `queue` (first positional string literal) is the
//!   topic.
//! - `assertQueue(queue, ...)` → treated as Subscribe (declaring a consumer
//!   queue).
//!
//! Only the first positional string literal is captured per call site.
//! Multi-arg patterns with non-literal args produce no output — no fabrication.
//!
//! # LLM-utility (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to RabbitMQ publish paths in
//! TypeScript services. Renaming a routing key in a publisher would show zero
//! consumers, causing an LLM to declare the change safe when downstream
//! Node.js consumers bound to that queue would silently stop receiving.
//!
//! # Schema gap (deferred)
//! Same as rabbitmq_python.rs — `RawEventTopic` lacks a `kind` field to
//! distinguish routing_key from queue-name strings. Deferred to a
//! schema-migration PR.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for amqplib / amqp-connection-manager call sites.
///
/// `consume` and `assertQueue` are subscriber-side calls;
/// `publish` and `sendToQueue` are publisher-side.
/// Unrecognised text defaults to `Publish`.
fn classify_amqp_direction(raw: &str) -> PubSub {
    match raw {
        "consume" | "assertQueue" | "subscribe" | "basic_consume" | "basic_get" => {
            PubSub::Subscribe
        }
        _ => PubSub::Publish,
    }
}

/// RabbitMQ TypeScript detector — fires for `amqplib` and
/// `amqp-connection-manager` imports.
///
/// `direction_capture: "amqp.direction"` binds the method identifier so
/// `classify_amqp_direction` can resolve `PubSub` direction.
pub const RABBITMQ_TS: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::RabbitMq,
    topic_capture: "amqp.topic",
    producer_capture: "amqp.fn",
    direction_capture: "amqp.direction",
    import_gate: &["amqplib", "amqp-connection-manager"],
    direction_classifier: classify_amqp_direction,
    canonicalize: true,
};
