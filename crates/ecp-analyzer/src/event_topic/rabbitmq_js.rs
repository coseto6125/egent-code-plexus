//! `EventTopicConfig` for RabbitMQ / AMQP JavaScript clients.
//!
//! Identical library surface as the TypeScript detector (`rabbitmq_ts.rs`):
//! - `amqplib`: `channel.publish(exchange, routingKey, content)`,
//!   `channel.consume('orders', handler)`, `channel.assertQueue('orders')`,
//!   `channel.sendToQueue('orders', content)`.
//! - `amqp-connection-manager`: same shape, same import gate.
//!
//! The capture names reuse the `amqp.*` namespace so the shared
//! `extract_event_topics` dispatcher works with a single `frameworks.scm`
//! query compiled into the JavaScript parser.
//!
//! # LLM-utility (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to RabbitMQ publish paths in
//! JavaScript services. Renaming a routing key in a publisher would show zero
//! consumers — silent data loss for downstream subscribers.
//!
//! # Schema gap (deferred)
//! Same as rabbitmq_python.rs — no `kind` field to distinguish routing_key
//! from queue-name strings. Deferred to a schema-migration PR.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for amqplib / amqp-connection-manager JS call sites.
///
/// `consume`, `assertQueue` → Subscribe; `publish`, `sendToQueue` → Publish.
/// Unrecognised text defaults to `Publish`.
fn classify_amqp_direction(raw: &str) -> PubSub {
    match raw {
        "consume" | "assertQueue" | "subscribe" | "basic_consume" | "basic_get" => {
            PubSub::Subscribe
        }
        _ => PubSub::Publish,
    }
}

/// RabbitMQ JavaScript detector — fires for `amqplib` and
/// `amqp-connection-manager` imports.
pub const RABBITMQ_JS: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::RabbitMq,
    topic_capture: "amqp.topic",
    producer_capture: "amqp.fn",
    direction_capture: "amqp.direction",
    import_gate: &["amqplib", "amqp-connection-manager"],
    direction_classifier: classify_amqp_direction,
    canonicalize: true,
};
