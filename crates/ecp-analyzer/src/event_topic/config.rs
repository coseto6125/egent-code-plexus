use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Table-driven configuration for a single message-bus / event-streaming framework.
///
/// Mirrors `SchemaFieldConfig` from the T4-1 schema-field extractor: instead of
/// N separate hardcoded detectors, each framework ships an `EventTopicConfig`
/// constant and `extract_event_topics` dispatches all of them uniformly.
///
/// `&'static str` fields are intentional — configs are `const` items, so all
/// strings live in the binary's read-only segment (zero heap alloc). These
/// structs are **never** archived by rkyv (only `RawEventTopic` is).
///
/// T5-1 (this PR): config struct + dispatch loop.
/// T5-2..T5-N: concrete `EventTopicConfig` constants for Kafka, RabbitMQ, NATS,
/// SNS/SQS, EventBridge.
pub struct EventTopicConfig {
    /// Framework identity written into `RawEventTopic::lib`.
    pub framework: FrameworkId,
    /// Tree-sitter capture name binding the topic string literal node.
    /// Example: `"kafka.topic"`, `"topic_name"`.
    pub topic_capture: &'static str,
    /// Tree-sitter capture name binding the enclosing function / producer call
    /// site identifier. Written into `RawEventTopic::enclosing_fn`.
    /// Empty string means the capture is absent; `extract_event_topics` then
    /// interns an empty string for `enclosing_fn`.
    pub producer_capture: &'static str,
    /// Tree-sitter capture name binding the direction / kind classification
    /// node (e.g. the call that distinguishes `produce` from `consume`).
    /// Empty string means absent; the extractor falls back to `PubSub::Publish`.
    pub direction_capture: &'static str,
    /// Import-gate: at least one entry must match a `RawImport::source` for
    /// this config to fire.  Checked via `framework_helpers::has_import_from`.
    pub import_gate: &'static [&'static str],
    /// Maps raw direction-capture text to the canonical `PubSub` variant.
    pub direction_classifier: fn(&str) -> PubSub,
    /// When `true`, run `event_topic::normalize::canonicalize()` on the raw
    /// captured topic string to build `RawEventTopic::topic_key`, the pairing
    /// key. `topic_literal` keeps the source spelling either way.
    ///
    /// Default-true ("opt-out"): T5-0 normalization is the locked canonical
    /// form for all topics. Set to `false` only for frameworks that already
    /// emit canonical strings (e.g. a schema-registry-backed topic that is
    /// pre-normalized before it reaches source code).
    pub canonicalize: bool,
}
