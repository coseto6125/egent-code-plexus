//! `EventTopicConfig` for Kafka TypeScript clients.
//!
//! Covers two TypeScript Kafka libraries under a single config:
//! - `kafkajs`: producer-side `producer.send({ topic: '...', messages: [...] })`
//! - `node-rdkafka`: producer-side `producer.produce('topic-name', ...)`
//!
//! Producer-only in this PR (`classify_kafka_direction` returns `PubSub::Publish`
//! unconditionally). Subscribe-side capture is T5-4-followup.
//!
//! Import gate: `kafkajs`, `node-rdkafka`.
//! Tree-sitter capture names: `kafka.topic`, `kafka.producer_fn`.

use super::config::EventTopicConfig;
use super::extract::classify_kafka_direction;
use ecp_core::analyzer::types::FrameworkId;

/// Kafka TypeScript detector — fires for `kafkajs` and `node-rdkafka` imports.
///
/// LLM-utility: surfaces producer call sites so `ecp impact` can trace which
/// functions publish to a given topic — enabling cross-service blast-radius
/// queries without manual grep across repo boundaries.
pub const KAFKA_TS: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Kafka,
    topic_capture: "kafka.topic",
    producer_capture: "kafka.producer_fn",
    direction_capture: "",
    import_gate: &["kafkajs", "node-rdkafka"],
    direction_classifier: classify_kafka_direction,
    canonicalize: true,
};
