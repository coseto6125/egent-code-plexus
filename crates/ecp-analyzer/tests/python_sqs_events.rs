//! T5-14 integration tests: AWS SQS Python (boto3/aioboto3) event-topic detector.
//!
//! Exercises the production `SQS_PYTHON` const and the real `frameworks.scm`
//! query string — a typo in either breaks these tests.

use ecp_analyzer::event_topic::{extract_event_topics, SQS_PYTHON};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/python/frameworks.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(&lang, FRAMEWORKS_SCM).expect("query compile");
    let imports: Vec<RawImport> = import_sources
        .iter()
        .map(|s| RawImport {
            source: (*s).to_string(),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    let mut pool = StringPool::new();
    let result = extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[SQS_PYTHON],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// boto3 (sync) send_message with literal QueueUrl → Publish.
#[test]
fn test_sqs_python_send_message_literal_queue_url() {
    let src = r#"
import boto3

def publish_order(sqs, payload):
    sqs.send_message(
        QueueUrl="https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        MessageBody=payload,
    )
"#;
    let (result, pool) = run(src, &["boto3"]);
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
        .expect("topic_literal must be Some for literal QueueUrl");
    assert_eq!(
        pool.resolve(&lit),
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// boto3 (sync) receive_message with literal QueueUrl → Subscribe.
#[test]
fn test_sqs_python_receive_message_direction_subscribe() {
    let src = r#"
import boto3

def consume_orders(sqs):
    response = sqs.receive_message(
        QueueUrl="https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        MaxNumberOfMessages=10,
    )
"#;
    let (result, pool) = run(src, &["boto3"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(
        pool.resolve(&lit),
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// aioboto3 (async) send_message with literal QueueUrl → Publish.
#[test]
fn test_sqs_python_aioboto3_async_send_message() {
    let src = r#"
import aioboto3

async def publish_order(sqs, payload):
    await sqs.send_message(
        QueueUrl="https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        MessageBody=payload,
    )
"#;
    let (result, pool) = run(src, &["aioboto3"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(
        pool.resolve(&lit),
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// send_message_batch with literal QueueUrl → Publish.
#[test]
fn test_sqs_python_send_message_batch_publish() {
    let src = r#"
import boto3

def publish_batch(sqs, entries):
    sqs.send_message_batch(
        QueueUrl="https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        Entries=entries,
    )
"#;
    let (result, pool) = run(src, &["boto3"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic for send_message_batch"
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(
        pool.resolve(&lit),
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// Variable QueueUrl → extractor refuses to fabricate; emits nothing.
#[test]
fn test_sqs_python_variable_queue_url_emits_nothing() {
    let src = r#"
import boto3

def publish_order(sqs, queue_url, payload):
    sqs.send_message(
        QueueUrl=queue_url,
        MessageBody=payload,
    )
"#;
    let (result, _pool) = run(src, &["boto3"]);
    assert!(
        result.is_empty(),
        "variable QueueUrl must not produce a RawEventTopic; got {:?}",
        result
    );
}

/// No boto3/aioboto3 import → import gate blocks all captures.
#[test]
fn test_sqs_python_no_import_no_captures() {
    let src = r#"
import my_queue_lib

def publish_order(sqs, payload):
    sqs.send_message(
        QueueUrl="https://sqs.us-east-1.amazonaws.com/123456789012/orders",
        MessageBody=payload,
    )
"#;
    let (result, _pool) = run(src, &["my_queue_lib"]);
    assert!(
        result.is_empty(),
        "non-SQS import must produce nothing; got {:?}",
        result
    );
}
