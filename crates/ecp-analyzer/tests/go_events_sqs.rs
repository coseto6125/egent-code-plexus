//! T5-18 integration tests: AWS SQS Go SDK v2 event-topic detector.
//!
//! Exercises the production `SQS_GO` const and the real `queries.scm` +
//! `frameworks.scm` query strings — a typo in either breaks these tests.

use ecp_analyzer::event_topic::{extract_event_topics, SQS_GO};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/go/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/go/frameworks.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
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
            imported_name: s.rsplit('/').next().unwrap_or(s).to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    let mut pool = StringPool::new();
    let result = extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[SQS_GO],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// Literal QueueUrl in SendMessage struct literal → RawEventTopic with direction Publish.
#[test]
fn test_sqs_go_send_message_literal_queue_url() {
    let src = r#"package main

import (
	"context"
	"github.com/aws/aws-sdk-go-v2/service/sqs"
	"github.com/aws/aws-sdk-go-v2/aws"
)

func publishOrder(ctx context.Context, client *sqs.Client, body string) {
	client.SendMessage(ctx, &sqs.SendMessageInput{
		QueueUrl:    aws.String("https://sqs.us-east-1.amazonaws.com/123456789012/orders"),
		MessageBody: aws.String(body),
	})
}
"#;
    let (result, pool) = run(src, &["github.com/aws/aws-sdk-go-v2/service/sqs"]);
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
        .expect("topic_literal must be Some for literal QueueUrl");
    assert_eq!(
        pool.resolve(&lit),
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}

/// QueueUrl assigned from a variable (not aws.String literal) → no capture.
#[test]
fn test_sqs_go_dynamic_queue_url_emits_nothing() {
    let src = r#"package main

import (
	"context"
	"github.com/aws/aws-sdk-go-v2/service/sqs"
	"github.com/aws/aws-sdk-go-v2/aws"
)

func publishOrder(ctx context.Context, client *sqs.Client, queueURL string, body string) {
	client.SendMessage(ctx, &sqs.SendMessageInput{
		QueueUrl:    aws.String(queueURL),
		MessageBody: aws.String(body),
	})
}
"#;
    let (result, _pool) = run(src, &["github.com/aws/aws-sdk-go-v2/service/sqs"]);
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
fn test_sqs_go_no_import_no_captures() {
    let src = r#"package main

import (
	"context"
	"fmt"
)

func publishOrder(ctx context.Context, queueURL string, body string) {
	fmt.Println("sending to", queueURL)
}
"#;
    let (result, _pool) = run(src, &["fmt"]);
    assert!(
        result.is_empty(),
        "non-SQS import must produce nothing; got {:?}",
        result
            .iter()
            .map(|r| (r.lib, r.direction))
            .collect::<Vec<_>>()
    );
}

/// ReceiveMessage with literal QueueUrl → direction Subscribe.
#[test]
fn test_sqs_go_receive_message_direction_subscribe() {
    let src = r#"package main

import (
	"context"
	"github.com/aws/aws-sdk-go-v2/service/sqs"
	"github.com/aws/aws-sdk-go-v2/aws"
)

func consumeOrders(ctx context.Context, client *sqs.Client) {
	client.ReceiveMessage(ctx, &sqs.ReceiveMessageInput{
		QueueUrl:            aws.String("https://sqs.us-east-1.amazonaws.com/123456789012/orders"),
		MaxNumberOfMessages: 10,
	})
}
"#;
    let (result, pool) = run(src, &["github.com/aws/aws-sdk-go-v2/service/sqs"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic for ReceiveMessage"
    );
    assert_eq!(result[0].lib, FrameworkId::Sqs);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(
        pool.resolve(&lit),
        "https://sqs.us-east-1.amazonaws.com/123456789012/orders"
    );
}
