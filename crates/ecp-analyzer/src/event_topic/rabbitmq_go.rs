//! `EventTopicConfig` for RabbitMQ / AMQP Go clients.
//!
//! Covers two Go RabbitMQ libraries with identical method signatures:
//! - `github.com/streadway/amqp` (the original Go AMQP 0-9-1 client).
//! - `github.com/rabbitmq/amqp091-go` (the maintained fork).
//!
//! Key call sites:
//! - `channel.Publish(exchange, routingKey, mandatory, immediate, msg)` → Publish.
//!   The routingKey (2nd positional arg) is the topic.
//! - `channel.Consume(queue, consumer, autoAck, exclusive, noLocal, noWait, args)` → Subscribe.
//!   The queue (1st positional arg) is the topic.
//!
//! # Topic literal semantics
//! Only string literal arguments are captured; variable args produce no output.
//!
//! # LLM-utility (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to RabbitMQ message paths in
//! Go services. A refactor of a Publish call would show zero Consume consumers,
//! causing the LLM to declare the change safe when downstream Go workers
//! would silently stop receiving.
//!
//! # Schema gap (deferred)
//! Same as rabbitmq_python.rs — no `kind` field to distinguish routing_key
//! from queue-name strings. Deferred to a schema-migration PR.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for streadway/amqp and amqp091-go call sites.
///
/// `Consume` → Subscribe; `Publish` → Publish.
/// Unrecognised text defaults to `Publish`.
fn classify_amqp_direction(raw: &str) -> PubSub {
    match raw {
        "Consume" | "Get" | "consume" | "subscribe" | "basicConsume" | "basic_consume" => {
            PubSub::Subscribe
        }
        _ => PubSub::Publish,
    }
}

/// RabbitMQ Go detector — fires for `streadway/amqp` and
/// `rabbitmq/amqp091-go` imports.
pub const RABBITMQ_GO: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::RabbitMq,
    topic_capture: "amqp.topic",
    producer_capture: "amqp.fn",
    direction_capture: "amqp.direction",
    import_gate: &["streadway/amqp", "rabbitmq/amqp091-go"],
    direction_classifier: classify_amqp_direction,
    canonicalize: true,
};
