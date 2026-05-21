//! `EventTopicConfig` for RabbitMQ / AMQP Java clients.
//!
//! Covers two Java RabbitMQ libraries:
//! - `org.springframework.amqp` (Spring AMQP):
//!   `rabbitTemplate.convertAndSend(exchange, routingKey, payload)` — Publish;
//!   `@RabbitListener(queues = "orders")` — Subscribe (annotation on method).
//! - `com.rabbitmq.client` (Java AMQP client):
//!   `channel.basicPublish(exchange, routingKey, props, body)` — Publish;
//!   `channel.basicConsume(queue, autoAck, consumer)` — Subscribe.
//!
//! # Topic literal semantics
//! - `convertAndSend(exchange, routingKey, ...)` → routingKey (2nd positional
//!   string literal) is the topic.
//! - `@RabbitListener(queues = "orders")` → the queue string literal from
//!   the annotation attribute.
//! - `basicPublish(exchange, routingKey, ...)` → routingKey (2nd positional).
//! - `basicConsume(queue, ...)` → queue (1st positional).
//!
//! # LLM-utility (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to RabbitMQ message paths in
//! Java services. A refactor of a Spring `convertAndSend` publisher would
//! show zero consumers, causing the LLM to declare the change safe when
//! downstream `@RabbitListener` services would silently stop receiving.
//!
//! # Schema gap (deferred)
//! Same as rabbitmq_python.rs — no `kind` field to distinguish routing_key
//! from queue-name strings. Deferred to a schema-migration PR.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for Spring AMQP and Java AMQP client call sites.
///
/// `basicConsume`, `basicGet`, `receive`, `receiveAndConvert`, and the
/// `@RabbitListener` sentinel value → Subscribe. Everything else → Publish.
fn classify_amqp_direction(raw: &str) -> PubSub {
    match raw {
        "basicConsume" | "basicGet" | "receive" | "receiveAndConvert" | "RabbitListener"
        | "subscribe" | "consume" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// RabbitMQ Java detector — fires for `org.springframework.amqp` and
/// `com.rabbitmq.client` imports.
pub const RABBITMQ_JAVA: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::RabbitMq,
    topic_capture: "amqp.topic",
    producer_capture: "amqp.fn",
    direction_capture: "amqp.direction",
    import_gate: &["org.springframework.amqp", "com.rabbitmq.client"],
    direction_classifier: classify_amqp_direction,
    canonicalize: true,
};
