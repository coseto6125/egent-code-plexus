//! `EventTopicConfig` for Kafka Go clients (T5-6).
//!
//! Covers three Go Kafka libraries under a single config:
//! - `github.com/segmentio/kafka-go`: `writer.WriteMessages(ctx, kafka.Message{Topic: "topic",...})`
//! - `github.com/Shopify/sarama`: `producer.SendMessage(&sarama.ProducerMessage{Topic: "topic",...})`
//! - `github.com/confluentinc/confluent-kafka-go/kafka`:
//!   `producer.Produce(&kafka.Message{TopicPartition: kafka.TopicPartition{Topic: &topic},...},...)`
//!
//! Direction dispatch: `classify_kafka_go_direction` returns `PubSub::Subscribe`
//! for consumer method names and `PubSub::Publish` for everything else.
//!
//! This classifier is intentionally module-private. Parallel-PR isolation
//! prevents 3-way merge conflicts; followup can consolidate once all lang PRs land.
//!
//! # Topic literal semantics
//! - segmentio: the `Topic` string literal in `kafka.Message{Topic: "topic"}`.
//! - sarama: the `Topic` string literal in `sarama.ProducerMessage{Topic: "topic"}`.
//! - confluent-kafka-go: the `Topic` string literal in
//!   `kafka.TopicPartition{Topic: &"topic"}` — only when Topic is a string literal;
//!   variable Topic → no capture → no `RawEventTopic` emitted (no fabrication).
//! - Variable topic arguments → no capture → no fabrication.
//!
//! # Schema gap (deferred)
//! `RawEventTopic` has no `kind` field — see `redis_python.rs` for the
//! schema gap note that applies equally here.
//!
//! # LLM-utility justification (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to Go Kafka message paths.
//! A rename of a `WriteMessages` call site would show zero consumers, causing
//! the LLM to declare the change safe when it silently breaks every consumer
//! listening on the same topic.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for Go Kafka call sites.
///
/// Go Kafka consumer-side identifiers (`ReadMessage`, `FetchMessage`,
/// `ReadMessages`, `ConsumeMessage`, `SubscribeTopics`) map to Subscribe;
/// all other method names (i.e. `WriteMessages`, `SendMessage`, `Produce`)
/// default to Publish.
fn classify_kafka_go_direction(raw: &str) -> PubSub {
    match raw {
        "ReadMessage" | "FetchMessage" | "ReadMessages" | "ConsumeMessage" | "SubscribeTopics" => {
            PubSub::Subscribe
        }
        _ => PubSub::Publish,
    }
}

/// Kafka Go detector — fires for segmentio/kafka-go, Shopify/sarama, and
/// confluentinc/confluent-kafka-go imports.
///
/// `direction_capture: "kafka.go.direction"` binds the method identifier so
/// `classify_kafka_go_direction` can resolve `PubSub` direction without fabrication.
///
/// `topic_capture: "kafka.topic"` captures the topic name as a raw string
/// literal node. Non-literal topic values produce no capture (no fabrication).
pub const KAFKA_GO: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Kafka,
    topic_capture: "kafka.topic",
    producer_capture: "kafka.go.fn",
    direction_capture: "kafka.go.direction",
    import_gate: &[
        "github.com/segmentio/kafka-go",
        "github.com/Shopify/sarama",
        "github.com/confluentinc/confluent-kafka-go/kafka",
    ],
    direction_classifier: classify_kafka_go_direction,
    canonicalize: true,
};
