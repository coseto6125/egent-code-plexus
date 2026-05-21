//! `EventTopicConfig` for Kafka Kotlin clients (T5-5, JVM symmetry with Java).
//!
//! Covers the same two Kafka libraries as Java but with Kotlin syntax:
//! - `org.apache.kafka`: `producer.send(ProducerRecord("topic", ...))`
//!   and `consumer.subscribe(listOf("topic", ...))`
//! - `org.springframework.kafka`: `template.send("topic", ...)` (Spring Kafka)
//!
//! Direction dispatch: `classify_kafka_kotlin_direction` maps the captured
//! method name to `PubSub::Subscribe` for `subscribe`, and `PubSub::Publish`
//! for all other call sites.
//!
//! This classifier is intentionally module-private. Parallel-PR isolation
//! prevents 3-way merge conflicts; followup can consolidate once all lang PRs land.
//!
//! # Topic literal semantics
//! - Apache Kafka producer: the first `String` literal in
//!   `ProducerRecord("topic", ...)`.
//! - Spring Kafka producer: the first `String` literal in `template.send("topic", ...)`.
//! - Apache Kafka consumer subscribe: the first `String` literal in
//!   `consumer.subscribe(listOf("topic", ...))`.
//! - Variable topic arguments → no capture → no `RawEventTopic` emitted
//!   (no fabrication).
//!
//! # Schema gap (deferred)
//! `RawEventTopic` has no `kind` field — see `redis_python.rs` for the
//! schema gap note that applies equally here.
//!
//! # LLM-utility justification (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to Kotlin Kafka message paths.
//! Symmetric with `kafka_java.rs` — mixed JVM codebases (Java + Kotlin in the
//! same repo) are load-bearing use cases; both parsers must surface the same
//! signal so cross-language `ecp impact` queries work correctly.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for Kotlin Kafka call sites.
///
/// `subscribe` is subscriber-side; everything else (i.e. `send`) is treated
/// as Publish. Default-Publish keeps topics indexed rather than dropped.
fn classify_kafka_kotlin_direction(raw: &str) -> PubSub {
    match raw {
        "subscribe" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// Kafka Kotlin detector — fires for `org.apache.kafka` and
/// `org.springframework.kafka` imports.
///
/// `direction_capture: "kafka.kotlin.direction"` binds the method identifier
/// (`send` or `subscribe`) so `classify_kafka_kotlin_direction` can resolve
/// `PubSub` direction without fabrication.
///
/// `topic_capture: "kafka.topic"` captures the topic name as a raw string
/// literal node. Non-literal args produce no capture (no fabrication).
pub const KAFKA_KOTLIN: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Kafka,
    topic_capture: "kafka.topic",
    producer_capture: "kafka.kotlin.fn",
    direction_capture: "kafka.kotlin.direction",
    import_gate: &["org.apache.kafka", "org.springframework.kafka"],
    direction_classifier: classify_kafka_kotlin_direction,
    canonicalize: true,
};
