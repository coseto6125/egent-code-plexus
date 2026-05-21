//! `EventTopicConfig` for Kafka Python clients.
//!
//! Covers four Python Kafka libraries under a single config; all are
//! producer-only in our current model (`classify_kafka_direction` returns
//! `PubSub::Publish` unconditionally).
//!
//! Import gate: `kafka`, `aiokafka`, `confluent_kafka`, `faust`.
//! Tree-sitter capture names: `kafka.topic`, `kafka.producer_fn`.

use super::config::EventTopicConfig;
use super::extract::classify_kafka_direction;
use ecp_core::analyzer::types::FrameworkId;

/// Kafka Python detector — fires for `kafka`, `aiokafka`, `confluent_kafka`,
/// and `faust` imports.
///
/// LLM-utility: surfaces producer call sites so `ecp impact` can trace which
/// functions publish to a given topic — enabling cross-service blast-radius
/// queries without manual grep across repo boundaries.
pub const KAFKA_PYTHON: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Kafka,
    topic_capture: "kafka.topic",
    producer_capture: "kafka.producer_fn",
    direction_capture: "",
    import_gate: &["kafka", "aiokafka", "confluent_kafka", "faust"],
    direction_classifier: classify_kafka_direction,
    canonicalize: true,
};
