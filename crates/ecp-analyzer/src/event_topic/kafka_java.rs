//! `EventTopicConfig` for Kafka Java clients (T5-5).
//!
//! Covers two Java Kafka libraries under a single config:
//! - `org.apache.kafka`: `producer.send(new ProducerRecord<>("topic", ...))` and
//!   `consumer.subscribe(Arrays.asList("topic", ...))`
//! - `org.springframework.kafka`: `kafkaTemplate.send("topic", ...)` (Spring Kafka)
//!
//! Direction dispatch: `classify_kafka_java_direction` maps the captured method
//! name to `PubSub::Subscribe` for `subscribe`, and `PubSub::Publish` for all
//! other call sites (default: `send`).
//!
//! This classifier is intentionally module-private rather than added to
//! `event_topic/extract.rs`. Parallel-PR isolation prevents 3-way merge
//! conflicts; a followup can consolidate once all lang PRs land.
//!
//! # Topic literal semantics
//! - Apache Kafka producer: the first positional `String` literal in
//!   `new ProducerRecord<>("topic", ...)`.
//! - Spring Kafka producer: the first positional `String` literal in
//!   `kafkaTemplate.send("topic", ...)`.
//! - Apache Kafka consumer subscribe: the first `String` literal in
//!   `consumer.subscribe(Arrays.asList("topic", ...))`.
//! - Variable topic arguments → no capture → no `RawEventTopic` emitted
//!   (no fabrication).
//!
//! # Schema gap (deferred)
//! `RawEventTopic` has no `kind` field — see `redis_python.rs` for the
//! schema gap note that applies equally here.
//!
//! # LLM-utility justification (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to Java Kafka message paths.
//! A rename of `producer.send(new ProducerRecord<>("orders", ...))` would
//! show zero subscribers, causing the LLM to declare the change safe when
//! it silently breaks every consumer listening on `"orders"`.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for Java Kafka call sites.
///
/// `subscribe` is subscriber-side; everything else (i.e. `send`) is treated
/// as Publish. Default-Publish keeps topics indexed rather than dropped on
/// unknown capture text.
fn classify_kafka_java_direction(raw: &str) -> PubSub {
    match raw {
        "subscribe" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// Kafka Java detector — fires for `org.apache.kafka` and
/// `org.springframework.kafka` imports.
///
/// `direction_capture: "kafka.java.direction"` binds the method identifier
/// (`send` or `subscribe`) so `classify_kafka_java_direction` can resolve
/// `PubSub` direction without fabrication.
///
/// `topic_capture: "kafka.topic"` captures the topic name as a raw string
/// literal node. Non-literal args produce no capture (no fabrication).
pub const KAFKA_JAVA: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Kafka,
    topic_capture: "kafka.topic",
    producer_capture: "kafka.java.fn",
    direction_capture: "kafka.java.direction",
    import_gate: &["org.apache.kafka", "org.springframework.kafka"],
    direction_classifier: classify_kafka_java_direction,
    canonicalize: true,
};
