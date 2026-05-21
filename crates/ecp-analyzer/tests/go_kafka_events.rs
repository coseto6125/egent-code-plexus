//! T5-6 integration tests: Kafka Go event-topic detector.
//!
//! Exercises the production `KAFKA_GO` const and the real `frameworks.scm`
//! query string against segmentio/kafka-go (WriteMessages) and
//! Shopify/sarama (ProducerMessage struct literal) patterns.

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_GO};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/go/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/go/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let combined = format!(
        "{}\n;; ---- framework queries ----\n{}",
        QUERIES_SCM, FRAMEWORKS_SCM
    );
    let query = Query::new(&lang, &combined).expect("query compile");
    let imports: Vec<RawImport> = import_sources
        .iter()
        .map(|s| RawImport {
            source: (*s).to_string(),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    extract_event_topics(&tree, src.as_bytes(), &query, &[KAFKA_GO], &imports)
}

/// segmentio/kafka-go: WriteMessages with literal Topic → Publish.
#[test]
fn test_go_segmentio_write_messages_literal_topic() {
    let src = r#"
package main

import kafka "github.com/segmentio/kafka-go"

func publishOrder(writer *kafka.Writer, ctx interface{}) {
    writer.WriteMessages(ctx, kafka.Message{
        Topic: "orders",
        Value: []byte("hello"),
    })
}
"#;
    let result = run(src, &["github.com/segmentio/kafka-go"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from WriteMessages; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
}

/// Shopify/sarama: ProducerMessage with literal Topic → Publish.
#[test]
fn test_go_sarama_producer_message_literal_topic() {
    let src = r#"
package main

import sarama "github.com/Shopify/sarama"

func sendSarama(producer sarama.SyncProducer) {
    msg := &sarama.ProducerMessage{
        Topic: "payments",
        Value: sarama.StringEncoder("hello"),
    }
    producer.SendMessage(msg)
}
"#;
    let result = run(src, &["github.com/Shopify/sarama"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from sarama.ProducerMessage; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "payments"
    );
}

/// Variable Topic field → no capture (no fabrication).
#[test]
fn test_go_variable_topic_emits_nothing() {
    let src = r#"
package main

import kafka "github.com/segmentio/kafka-go"

func publishDynamic(writer *kafka.Writer, ctx interface{}, topicName string) {
    writer.WriteMessages(ctx, kafka.Message{
        Topic: topicName,
        Value: []byte("hello"),
    })
}
"#;
    let result = run(src, &["github.com/segmentio/kafka-go"]);
    assert!(
        result.is_empty(),
        "variable Topic must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No Kafka import → import gate must reject all captures.
#[test]
fn test_go_no_kafka_import_emits_nothing() {
    let src = r#"
package main

import "net/http"

func sendMessage(w http.ResponseWriter, topic string) {
    w.WriteMessages(nil, struct{ Topic string }{"orders"})
}
"#;
    let result = run(src, &["net/http"]);
    assert!(
        result.is_empty(),
        "non-kafka import must produce nothing; got {:?}",
        result
    );
}

/// segmentio/kafka-go: WriteMessages inside a method_declaration → Publish.
#[test]
fn test_go_segmentio_write_messages_method_decl() {
    let src = r#"
package main

import kafka "github.com/segmentio/kafka-go"

type EventService struct {
    writer *kafka.Writer
}

func (s *EventService) publishEvent(ctx interface{}) {
    s.writer.WriteMessages(ctx, kafka.Message{
        Topic: "events",
        Value: []byte("data"),
    })
}
"#;
    let result = run(src, &["github.com/segmentio/kafka-go"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from method WriteMessages; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "events"
    );
}

/// Enclosing function name is captured correctly.
#[test]
fn test_go_enclosing_fn_name_captured() {
    let src = r#"
package main

import kafka "github.com/segmentio/kafka-go"

func publishUserEvent(writer *kafka.Writer, ctx interface{}) {
    writer.WriteMessages(ctx, kafka.Message{
        Topic: "users",
        Value: []byte("x"),
    })
}
"#;
    let result = run(src, &["github.com/segmentio/kafka-go"]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].enclosing_fn.as_ref(), "publishUserEvent");
}
