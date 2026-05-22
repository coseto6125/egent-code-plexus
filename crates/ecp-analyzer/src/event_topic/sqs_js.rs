//! `EventTopicConfig` for AWS SQS JavaScript clients.
//!
//! Covers `@aws-sdk/client-sqs` (AWS SDK v3 for JavaScript):
//! - `await client.send(new SendMessageCommand({ QueueUrl: "...", MessageBody: "..." }))`
//! - `await client.send(new ReceiveMessageCommand({ QueueUrl: "...", ... }))`
//! - `await client.send(new SendMessageBatchCommand({ QueueUrl: "...", Entries: [...] }))`
//! - `await client.send(new DeleteMessageCommand({ QueueUrl: "...", ReceiptHandle: "..." }))`
//!
//! Direction: `SendMessageCommand` / `SendMessageBatchCommand` → Publish.
//! `ReceiveMessageCommand` / `DeleteMessageCommand` → Subscribe.
//!
//! Topic literal: the `QueueUrl` property string in the Command constructor object.
//! Non-literal `QueueUrl` (variable) produces no capture → no RawEventTopic.
//!
//! LLM-utility: SQS durable queue semantics differ from Redis pub/sub (ephemeral)
//! and Kafka (log-based replay). LLMs must know all JS producers/consumers of a
//! queue when renaming a QueueUrl — this config surfaces them for `ecp impact`.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Maps the SQS Command constructor name to `PubSub` direction.
///
/// Consumer-side commands (`ReceiveMessageCommand`, `DeleteMessageCommand`)
/// → `Subscribe`. Send variants → `Publish`.
fn classify_sqs_direction(raw: &str) -> PubSub {
    match raw {
        "ReceiveMessageCommand" | "DeleteMessageCommand" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// SQS JavaScript detector — fires for `@aws-sdk/client-sqs` imports.
///
/// `direction_capture: "sqs.direction"` binds the Command constructor name so
/// `classify_sqs_direction` can resolve `PubSub` direction without fabrication.
///
/// `topic_capture: "sqs.topic"` captures the `QueueUrl` property string literal
/// from the Command constructor's object argument. Non-literal `QueueUrl`
/// produces no capture.
pub const SQS_JS: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Sqs,
    topic_capture: "sqs.topic",
    producer_capture: "sqs.fn",
    direction_capture: "sqs.direction",
    import_gate: &["@aws-sdk/client-sqs"],
    direction_classifier: classify_sqs_direction,
    canonicalize: false,
};
