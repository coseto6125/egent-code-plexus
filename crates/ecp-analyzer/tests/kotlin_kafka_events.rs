//! T5-5 (JVM symmetry) integration tests: Kafka Kotlin event-topic detector.
//!
//! Exercises the production `KAFKA_KOTLIN` const and the real `frameworks.scm`
//! query string against `org.apache.kafka` (producer.send / consumer.subscribe)
//! and `org.springframework.kafka` (template.send) Kotlin patterns.

use ecp_analyzer::event_topic::{extract_event_topics, KAFKA_KOTLIN};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/kotlin/queries.scm");
const FRAMEWORKS_SCM: &str = include_str!("../src/kotlin/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_kotlin::LANGUAGE.into();
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
    extract_event_topics(&tree, src.as_bytes(), &query, &[KAFKA_KOTLIN], &imports)
}

/// Apache Kafka: producer.send(ProducerRecord("topic", ...)) → Publish.
#[test]
fn test_kotlin_kafka_producer_send_producerrecord_literal_topic() {
    let src = r#"
import org.apache.kafka.clients.producer.KafkaProducer
import org.apache.kafka.clients.producer.ProducerRecord

fun publishOrder(producer: KafkaProducer<String, String>, data: String) {
    producer.send(ProducerRecord("orders", data))
}
"#;
    let result = run(src, &["org.apache.kafka"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from ProducerRecord; got {:?}",
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

/// Spring Kafka: template.send("topic", msg) → Publish.
#[test]
fn test_kotlin_spring_kafka_template_send_literal_topic() {
    let src = r#"
import org.springframework.kafka.core.KafkaTemplate

fun publishPayment(template: KafkaTemplate<String, String>, msg: String) {
    template.send("payments", msg)
}
"#;
    let result = run(src, &["org.springframework.kafka"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from KafkaTemplate.send; got {:?}",
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

/// Variable topic argument → no capture (no fabrication).
#[test]
fn test_kotlin_variable_topic_emits_nothing() {
    let src = r#"
import org.apache.kafka.clients.producer.KafkaProducer
import org.apache.kafka.clients.producer.ProducerRecord

fun publishDynamic(producer: KafkaProducer<String, String>, topicName: String, data: String) {
    producer.send(ProducerRecord(topicName, data))
}
"#;
    let result = run(src, &["org.apache.kafka"]);
    assert!(
        result.is_empty(),
        "variable topic must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No Kafka import → import gate must reject all captures.
#[test]
fn test_kotlin_no_kafka_import_emits_nothing() {
    let src = r#"
fun sendMessage(logger: Any, msg: String) {
    logger.send("events", msg)
}
"#;
    let result = run(src, &["some.other.lib"]);
    assert!(
        result.is_empty(),
        "non-kafka import must produce nothing; got {:?}",
        result
    );
}

/// Spring Kafka and Apache Kafka both fire correctly in the same config slice.
#[test]
fn test_kotlin_spring_and_apache_kafka_both_fire() {
    let src = r#"
import org.apache.kafka.clients.producer.KafkaProducer
import org.apache.kafka.clients.producer.ProducerRecord
import org.springframework.kafka.core.KafkaTemplate

fun sendApache(producer: KafkaProducer<String, String>, data: String) {
    producer.send(ProducerRecord("events", data))
}

fun sendSpring(template: KafkaTemplate<String, String>, msg: String) {
    template.send("billing", msg)
}
"#;
    let result = run(src, &["org.apache.kafka", "org.springframework.kafka"]);
    let topics: Vec<&str> = result
        .iter()
        .map(|r| r.topic_literal.as_deref().unwrap())
        .collect();
    assert!(topics.contains(&"events"), "apache kafka topic must appear");
    assert!(
        topics.contains(&"billing"),
        "spring kafka topic must appear"
    );
}

/// Enclosing function name is captured correctly.
#[test]
fn test_kotlin_enclosing_fn_name_captured() {
    let src = r#"
import org.springframework.kafka.core.KafkaTemplate

fun publishUserEvent(template: KafkaTemplate<String, String>, msg: String) {
    template.send("users", msg)
}
"#;
    let result = run(src, &["org.springframework.kafka"]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].enclosing_fn.as_ref(), "publishUserEvent");
}
