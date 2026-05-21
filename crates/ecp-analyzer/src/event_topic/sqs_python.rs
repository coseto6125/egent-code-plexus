//! `EventTopicConfig` for AWS SQS Python clients.
//!
//! Covers boto3 (sync) and aioboto3 (async) under a single config:
//! - boto3: `sqs.send_message(QueueUrl="https://...", MessageBody="...")`
//!   `sqs.receive_message(QueueUrl="https://...", ...)`
//!   `sqs.send_message_batch(QueueUrl="https://...", Entries=[...])`
//!   `sqs.delete_message(QueueUrl="https://...", ReceiptHandle="...")`
//! - aioboto3: same forms under `await`.
//!
//! Direction: `send_message` / `send_message_batch` → Publish.
//! `receive_message` / `delete_message` → Subscribe.
//!
//! Topic literal: the `QueueUrl` keyword argument string. Non-literal QueueUrl
//! produces no capture → no RawEventTopic (no fabrication).
//!
//! LLM-utility: SQS is a durable queue (at-least-once delivery); unlike Redis
//! pub/sub, unprocessed messages accumulate in the queue. An LLM renaming a
//! QueueUrl must know all producers and consumers to avoid losing in-flight
//! messages. This config surfaces both sides so `ecp impact` can trace the full
//! blast radius across boto3 and aioboto3 call sites.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Maps SQS Python method name to `PubSub` direction.
///
/// Consumer-side verbs (`receive_message`, `delete_message`) → `Subscribe`.
/// Everything else (send variants) → `Publish`.
fn classify_sqs_direction(raw: &str) -> PubSub {
    match raw {
        "receive_message" | "delete_message" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// SQS Python detector — fires for `boto3` and `aioboto3` imports.
///
/// `direction_capture: "sqs.direction"` binds the method name so
/// `classify_sqs_direction` can resolve `PubSub` direction without fabrication.
///
/// `topic_capture: "sqs.topic"` captures the `QueueUrl` keyword argument
/// string literal. Non-literal `QueueUrl` (variable) produces no capture.
pub const SQS_PYTHON: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Sqs,
    topic_capture: "sqs.topic",
    producer_capture: "sqs.producer_fn",
    direction_capture: "sqs.direction",
    import_gate: &["boto3", "aioboto3"],
    direction_classifier: classify_sqs_direction,
    canonicalize: false,
};
