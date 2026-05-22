//! T5-16 integration tests: AWS SQS JavaScript (@aws-sdk/client-sqs) event-topic detector.
//!
//! Exercises the production `SQS_JS` const and the real `queries.scm` +
//! `frameworks.scm` query strings — a typo in either breaks these tests.

use ecp_analyzer::event_topic::{extract_event_topics, SQS_JS};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/javascript/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/javascript/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<ecp_core::analyzer::types::RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query_source = format!(
        "{}\n;; ---- framework queries ----\n{}",
        QUERIES_SCM, FRAMEWORKS_SCM,
    );
    let query = Query::new(&lang, &query_source).expect("query compile");
    let imports: Vec<RawImport> = import_sources
        .iter()
        .map(|s| RawImport {
            source: (*s).to_string(),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    extract_event_topics(&tree, src.as_bytes(), &query, &[SQS_JS], &imports)
}

/// await SendMessageCommand with literal QueueUrl → Publish.
#[test]
fn test_sqs_js_send_message_command_literal_queue_url() {
    let src = r#"
const { SQSClient, SendMessageCommand } = require("@aws-sdk/client-sqs");

async function publishOrder(client, payload) {
    await client.send(new SendMessageCommand({
        QueueUrl: "https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        MessageBody: payload,
    }));
}
"#;
    let result = run(src, &["@aws-sdk/client-sqs"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some for literal QueueUrl");
    assert_eq!(
        lit,
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// await ReceiveMessageCommand with literal QueueUrl → Subscribe.
#[test]
fn test_sqs_js_receive_message_command_direction_subscribe() {
    let src = r#"
const { SQSClient, ReceiveMessageCommand } = require("@aws-sdk/client-sqs");

async function consumeOrders(client) {
    await client.send(new ReceiveMessageCommand({
        QueueUrl: "https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        MaxNumberOfMessages: 10,
    }));
}
"#;
    let result = run(src, &["@aws-sdk/client-sqs"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic for ReceiveMessageCommand"
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(
        lit,
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// await SendMessageBatchCommand with literal QueueUrl → Publish.
#[test]
fn test_sqs_js_send_message_batch_publish() {
    let src = r#"
const { SQSClient, SendMessageBatchCommand } = require("@aws-sdk/client-sqs");

async function publishBatch(client, entries) {
    await client.send(new SendMessageBatchCommand({
        QueueUrl: "https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        Entries: entries,
    }));
}
"#;
    let result = run(src, &["@aws-sdk/client-sqs"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic for SendMessageBatchCommand"
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(
        lit,
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// Variable QueueUrl → extractor refuses to fabricate; emits nothing.
#[test]
fn test_sqs_js_variable_queue_url_emits_nothing() {
    let src = r#"
const { SQSClient, SendMessageCommand } = require("@aws-sdk/client-sqs");

async function publishOrder(client, queueUrl, payload) {
    await client.send(new SendMessageCommand({
        QueueUrl: queueUrl,
        MessageBody: payload,
    }));
}
"#;
    let result = run(src, &["@aws-sdk/client-sqs"]);
    assert!(
        result.is_empty(),
        "variable QueueUrl must not produce a RawEventTopic; got {:?}",
        result
    );
}

/// No @aws-sdk/client-sqs import → import gate blocks all captures.
#[test]
fn test_sqs_js_no_import_no_captures() {
    let src = r#"
const { MyQueueClient } = require("my-queue-lib");

async function publishOrder(client, payload) {
    await client.send(new SendMessageCommand({
        QueueUrl: "https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        MessageBody: payload,
    }));
}
"#;
    let result = run(src, &["my-queue-lib"]);
    assert!(
        result.is_empty(),
        "non-SQS import must produce nothing; got {:?}",
        result
    );
}

/// Class method with await SendMessageCommand → Publish.
#[test]
fn test_sqs_js_class_method_send_message() {
    let src = r#"
const { SQSClient, SendMessageCommand } = require("@aws-sdk/client-sqs");

class OrderService {
    async publishOrder(payload) {
        await this.client.send(new SendMessageCommand({
            QueueUrl: "https://sqs.us-east-1.amazonaws.com/123456789012/orders",
            MessageBody: payload,
        }));
    }
}
"#;
    let result = run(src, &["@aws-sdk/client-sqs"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from class method; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(
        lit,
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}
