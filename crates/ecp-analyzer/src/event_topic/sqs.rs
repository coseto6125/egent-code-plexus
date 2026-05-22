//! `EventTopicConfig` constants for AWS SQS across Java, Go, and Rust.
//!
//! All three share the same topic identifier: the **QueueUrl** string passed
//! to `SendMessage` / `SendMessageBatch` (publish) or `ReceiveMessage` (subscribe).
//!
//! Import gates (per T5-17/18/19 roadmap matrix):
//! - Java: `software.amazon.awssdk.services.sqs`
//! - Go:   `github.com/aws/aws-sdk-go-v2/service/sqs`
//! - Rust: `aws-sdk-sqs`  (Cargo crate name; as a `use` path: `aws_sdk_sqs`)
//!
//! Direction: `classify_sqs_direction` maps send/publish verbs to `Publish`
//! and receive/poll verbs to `Subscribe`; unknown text defaults to `Publish`.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Maps SQS call-site verb text to `PubSub` direction.
///
/// Producer verbs (`send_message`, `sendMessage`, `SendMessage`, etc.) →
/// `Publish`; consumer verbs (`receive_message`, `receiveMessage`,
/// `ReceiveMessage`) → `Subscribe`. Unrecognised text defaults to `Publish`
/// so the topic is still indexed rather than silently dropped.
pub fn classify_sqs_direction(raw: &str) -> PubSub {
    match raw {
        "receive_message" | "receiveMessage" | "ReceiveMessage" => PubSub::Subscribe,
        _ => PubSub::Publish,
    }
}

/// SQS detector for the **AWS SDK for Java v2** (`software.amazon.awssdk`).
///
/// Tree-sitter capture names: `sqs.topic`, `sqs.producer_fn`, `sqs.direction`.
///
/// Fires on `SqsClient.sendMessage(SendMessageRequest.builder().queueUrl("...").build())`
/// and the equivalent `sendMessageBatch` / `receiveMessage` shapes. QueueUrl
/// is captured as a string literal from the `.queueUrl("…")` builder call.
pub const SQS_JAVA: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Sqs,
    topic_capture: "sqs.topic",
    producer_capture: "sqs.producer_fn",
    direction_capture: "sqs.direction",
    import_gate: &["software.amazon.awssdk.services.sqs"],
    direction_classifier: classify_sqs_direction,
    canonicalize: false,
};

/// SQS detector for the **AWS SDK for Go v2** (`aws-sdk-go-v2`).
///
/// Tree-sitter capture names: `sqs.topic`, `sqs.producer_fn`, `sqs.direction`.
///
/// Fires on `client.SendMessage(ctx, &sqs.SendMessageInput{QueueUrl: aws.String("…")})`.
/// The QueueUrl field value inside the struct literal is captured when it is
/// a string literal passed to `aws.String("…")`.
pub const SQS_GO: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Sqs,
    topic_capture: "sqs.topic",
    producer_capture: "sqs.producer_fn",
    direction_capture: "sqs.direction",
    import_gate: &["github.com/aws/aws-sdk-go-v2/service/sqs"],
    direction_classifier: classify_sqs_direction,
    canonicalize: false,
};

/// SQS detector for the **AWS SDK for Rust** (`aws-sdk-sqs` crate).
///
/// Tree-sitter capture names: `sqs.topic`, `sqs.producer_fn`, `sqs.direction`.
///
/// Fires on `client.send_message().queue_url("…").send().await` fluent-builder
/// chains. The string literal passed to `.queue_url("…")` is captured.
/// SQS detector for the **AWS SDK for Rust** (`aws-sdk-sqs` crate).
///
/// Tree-sitter capture names: `sqs.topic`, `sqs.direction`.
///
/// Note: `producer_capture` is empty because tree-sitter Rust's named-field
/// `body: (block (...))` does not perform descendant matching, making it
/// impractical to anchor the pattern to a `function_item` while also
/// matching the `queue_url` call deep inside a fluent chain. The `enclosing_fn`
/// field in `RawEventTopic` is left empty; `topic_literal` + `direction` are
/// the primary outputs.
pub const SQS_RUST: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Sqs,
    topic_capture: "sqs.topic",
    producer_capture: "",
    direction_capture: "sqs.direction",
    import_gate: &["aws_sdk_sqs"],
    direction_classifier: classify_sqs_direction,
    canonicalize: false,
};
