//! T5-19 integration tests: AWS SQS Rust SDK event-topic detector.
//!
//! Exercises the production `SQS_RUST` const and the real `queries.scm` +
//! `frameworks.scm` query strings — a typo in either breaks these tests.

use ecp_analyzer::event_topic::{extract_event_topics, SQS_RUST};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/rust/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/rust/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<ecp_core::analyzer::types::RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
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
            imported_name: s.split("::").last().unwrap_or(s).to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    extract_event_topics(&tree, src.as_bytes(), &query, &[SQS_RUST], &imports)
}

/// Literal queue_url in send_message fluent chain → RawEventTopic Publish.
#[test]
fn test_sqs_rust_send_message_literal_queue_url() {
    let src = r#"
use aws_sdk_sqs::Client;

async fn publish_order(client: &Client, payload: &str) {
    client
        .send_message()
        .queue_url("https://sqs.us-east-1.amazonaws.com/123456789012/orders")
        .message_body(payload)
        .send()
        .await
        .unwrap();
}
"#;
    let result = run(src, &["aws_sdk_sqs"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic; got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some for literal queue_url");
    assert_eq!(
        lit,
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// queue_url assigned from a variable → extractor refuses to fabricate; emits nothing.
#[test]
fn test_sqs_rust_dynamic_queue_url_emits_nothing() {
    let src = r#"
use aws_sdk_sqs::Client;

async fn publish_order(client: &Client, queue_url: &str, payload: &str) {
    client
        .send_message()
        .queue_url(queue_url)
        .message_body(payload)
        .send()
        .await
        .unwrap();
}
"#;
    let result = run(src, &["aws_sdk_sqs"]);
    assert!(
        result.is_empty(),
        "variable queue_url must not produce a RawEventTopic; got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
}

/// No aws-sdk-sqs import → import gate blocks all captures.
#[test]
fn test_sqs_rust_no_import_no_captures() {
    let src = r#"
use std::collections::HashMap;

async fn publish_order(payload: &str) {
    let mut map = HashMap::new();
    map.insert("queue_url", "https://sqs.us-east-1.amazonaws.com/123456789012/orders");
}
"#;
    let result = run(src, &["std::collections::HashMap"]);
    assert!(
        result.is_empty(),
        "non-SQS import must produce nothing; got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
}
