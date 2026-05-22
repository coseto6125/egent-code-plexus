//! T5-17 integration tests: AWS SQS Java SDK v2 event-topic detector.
//!
//! Exercises the production `SQS_JAVA` const and the real `queries.scm` +
//! `frameworks.scm` query strings — a typo in either breaks these tests.

use ecp_analyzer::event_topic::{extract_event_topics, SQS_JAVA};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/java/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/java/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<ecp_core::analyzer::types::RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
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
    extract_event_topics(&tree, src.as_bytes(), &query, &[SQS_JAVA], &imports)
}

/// Literal QueueUrl in sendMessage builder chain → RawEventTopic with direction Publish.
#[test]
fn test_sqs_java_send_message_literal_queue_url() {
    let src = r#"
import software.amazon.awssdk.services.sqs.SqsClient;
import software.amazon.awssdk.services.sqs.model.SendMessageRequest;

public class OrderService {
    public void publishOrder(SqsClient sqsClient, String payload) {
        sqsClient.sendMessage(
            SendMessageRequest.builder()
                .queueUrl("https://sqs.us-east-1.amazonaws.com/123456789012/orders")
                .messageBody(payload)
                .build()
        );
    }
}
"#;
    let result = run(src, &["software.amazon.awssdk.services.sqs.SqsClient"]);
    assert_eq!(result.len(), 1, "expected one RawEventTopic");
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

/// QueueUrl assigned from a variable → extractor refuses to fabricate; emits nothing.
#[test]
fn test_sqs_java_dynamic_queue_url_emits_nothing() {
    let src = r#"
import software.amazon.awssdk.services.sqs.SqsClient;
import software.amazon.awssdk.services.sqs.model.SendMessageRequest;

public class OrderService {
    public void publishOrder(SqsClient sqsClient, String queueUrl, String payload) {
        sqsClient.sendMessage(
            SendMessageRequest.builder()
                .queueUrl(queueUrl)
                .messageBody(payload)
                .build()
        );
    }
}
"#;
    let result = run(src, &["software.amazon.awssdk.services.sqs.SqsClient"]);
    assert!(
        result.is_empty(),
        "variable QueueUrl must not produce a RawEventTopic; got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
}

/// No SQS SDK import → import gate blocks all captures.
#[test]
fn test_sqs_java_no_import_no_captures() {
    let src = r#"
import com.example.internal.Queue;

public class OrderService {
    public void publishOrder(Queue q, String payload) {
        q.sendMessage(
            RequestBuilder.builder()
                .queueUrl("https://example.com/queue")
                .body(payload)
                .build()
        );
    }
}
"#;
    let result = run(src, &["com.example.internal.Queue"]);
    assert!(
        result.is_empty(),
        "non-SQS import must produce nothing; got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
}

/// receiveMessage with literal QueueUrl → direction Subscribe.
#[test]
fn test_sqs_java_receive_message_direction_subscribe() {
    let src = r#"
import software.amazon.awssdk.services.sqs.SqsClient;
import software.amazon.awssdk.services.sqs.model.ReceiveMessageRequest;

public class OrderConsumer {
    public void consume(SqsClient sqsClient) {
        sqsClient.receiveMessage(
            ReceiveMessageRequest.builder()
                .queueUrl("https://sqs.us-east-1.amazonaws.com/123456789012/orders")
                .maxNumberOfMessages(10)
                .build()
        );
    }
}
"#;
    let result = run(src, &["software.amazon.awssdk.services.sqs.SqsClient"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic for receiveMessage"
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
