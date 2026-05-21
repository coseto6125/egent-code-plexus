//! `EventTopicConfig` for RabbitMQ / AMQP Rust clients.
//!
//! Covers two Rust RabbitMQ libraries:
//! - `lapin` (async AMQP 0-9-1 client):
//!   `channel.basic_publish(exchange, routing_key, options, payload, properties)` → Publish;
//!   `channel.basic_consume(queue, consumer_tag, options, fields)` → Subscribe.
//! - `amiquip` (sync-friendly AMQP client):
//!   Similar method names; same import gate covers both via prefix match.
//!
//! # Topic literal semantics
//! - `basic_publish(exchange, routing_key, ...)` → routing_key (2nd positional
//!   string literal arg). If routing_key is a variable, no capture.
//! - `basic_consume(queue, ...)` → queue (1st positional string literal arg).
//!
//! # LLM-utility (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to RabbitMQ message paths in
//! Rust async services using `lapin`. A refactor of a `basic_publish` call
//! would show zero consumers, causing the LLM to declare the change safe
//! when downstream consumers would silently stop receiving.
//!
//! # Schema gap (deferred)
//! Same as rabbitmq_python.rs — no `kind` field to distinguish routing_key
//! from queue-name strings. Deferred to a schema-migration PR.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for lapin and amiquip call sites.
///
/// `basic_consume`, `basic_get` → Subscribe; `basic_publish` → Publish.
/// Unrecognised text defaults to `Publish`.
fn classify_amqp_direction(raw: &str) -> PubSub {
    match raw {
        "basic_consume" | "basic_get" | "consume" | "subscribe" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// RabbitMQ Rust detector — fires for `lapin` and `amiquip` imports.
pub const RABBITMQ_RUST: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::RabbitMq,
    topic_capture: "amqp.topic",
    producer_capture: "amqp.fn",
    direction_capture: "amqp.direction",
    import_gate: &["lapin", "amiquip"],
    direction_classifier: classify_amqp_direction,
    canonicalize: true,
};
