//! `EventTopicConfig` for AWS SQS TypeScript clients.
//!
//! Covers `@aws-sdk/client-sqs` (AWS SDK v3 for JavaScript/TypeScript):
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
//! LLM-utility: SQS is a durable queue (at-least-once delivery); LLMs renaming
//! a QueueUrl must trace all producers and consumers to avoid losing in-flight
//! messages. This config surfaces both send and receive call sites so `ecp impact`
//! can produce a complete blast-radius view across TypeScript AWS SDK code.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Maps the SQS Command constructor name to `PubSub` direction.
///
/// Consumer-side commands (`ReceiveMessageCommand`, `DeleteMessageCommand`)
/// → `Subscribe`. Everything else (send variants) → `Publish`.
fn classify_sqs_direction(raw: &str) -> PubSub {
    match raw {
        "ReceiveMessageCommand" | "DeleteMessageCommand" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// SQS TypeScript detector — fires for `@aws-sdk/client-sqs` imports.
///
/// `direction_capture: "sqs.direction"` binds the Command constructor name so
/// `classify_sqs_direction` can resolve `PubSub` direction without fabrication.
///
/// `topic_capture: "sqs.topic"` captures the `QueueUrl` property string literal
/// from the Command constructor's object argument. Non-literal `QueueUrl`
/// (variable or expression) produces no capture.
pub const SQS_TS: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Sqs,
    topic_capture: "sqs.topic",
    producer_capture: "sqs.fn",
    direction_capture: "sqs.direction",
    import_gate: &["@aws-sdk/client-sqs"],
    direction_classifier: classify_sqs_direction,
    canonicalize: false,
};
