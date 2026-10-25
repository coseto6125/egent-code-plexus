//! P8d: an event topic keeps its source spelling on the record and the
//! `EventTopic` node name; the canonical form is only the pairing key.
//!
//! Contract: C4 `RecordKind::EventTopic`. A spelling assertion here stands in
//! for "an LLM reading the node sees what the source wrote"; a change that
//! renames the node after its canonical key is a regression, not a refresh.
//!
//! Edge list: empty / absent topics emit no record (dynamic-topic tests live
//! in the per-language `*_events.rs` files); a whitespace-only or list-valued
//! topic is not a string literal and matches no detector query, so it does not
//! apply here; the dependency (import gate) is covered per language below;
//! duplicate call sites are the two-spellings-one-file test.
//!
//! Python, JavaScript, TypeScript, Go, Java, Kotlin and Rust each have a
//! row. The other mainstream languages have none because no event-topic
//! detector exists for them (`ecp_analyzer::event_topic` holds Kafka, Redis,
//! RabbitMQ, SQS and Celery specs for those seven only), so no record of
//! theirs carries a topic spelling: C#, PHP, Ruby, Swift, C, C++ and Dart.

use ecp_analyzer::event_topic::{extract_event_topics, normalize::canonicalize, KAFKA_PYTHON};
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawEventTopic, RawImport};
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use tree_sitter::{Parser, Query};

const SPELLING: &str = "Order.Created";
const KEY: &str = "order/created";

fn parse_with<P: LanguageProvider>(
    provider: anyhow::Result<P>,
    path: &str,
    src: &str,
) -> LocalGraph {
    provider
        .expect("provider")
        .parse_file(path.as_ref(), src.as_bytes())
        .expect("parse_file")
}

fn parse_python(path: &str, src: &str) -> LocalGraph {
    parse_with(PythonProvider::new(), path, src)
}

fn build(local_graphs: Vec<LocalGraph>) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for lg in local_graphs {
        builder.add_graph(lg);
    }
    builder.build()
}

fn event_topic_names(graph: &ZeroCopyGraph) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EventTopic)
        .map(|n| n.name.resolve(pool).to_string())
        .collect()
}

fn assert_keeps_spelling(lg: &LocalGraph) {
    let topics: &[RawEventTopic] = lg.event_topics.as_deref().unwrap_or(&[]);
    assert_eq!(topics.len(), 1, "expected one record; got {topics:?}");
    assert_eq!(topics[0].topic_literal.as_deref(), Some(SPELLING));
    assert_eq!(topics[0].topic_key.as_deref(), Some(KEY));
}

#[test]
fn test_extract_topic_canonicalized_keeps_source_spelling() {
    let src = "from kafka import KafkaProducer\n\ndef publish_order(p):\n    p.send(\"Order.Created\", b\"x\")\n";
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(&lang, include_str!("../src/python/frameworks.scm")).expect("query");
    let imports = [RawImport {
        source: "kafka".to_string(),
        imported_name: "*".to_string(),
        alias: None,
        binding_kind: None,
    }];
    let result = extract_event_topics(&tree, src.as_bytes(), &query, &[KAFKA_PYTHON], &imports);

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].topic_literal.as_deref(), Some(SPELLING));
    assert_eq!(
        result[0].topic_key.as_deref(),
        Some(canonicalize("order_created").as_str())
    );
    assert_eq!(result[0].topic_key.as_deref(), Some(KEY));
}

#[test]
fn test_event_topic_mirrors_two_spellings_pair_on_canonical_key() {
    let publisher = parse_python(
        "services/publisher.py",
        "import redis\n\ndef publish_order(r, data):\n    r.publish(\"Order.Created\", data)\n",
    );
    let subscriber = parse_python(
        "services/subscriber.py",
        "import redis\n\ndef listen(pubsub):\n    pubsub.subscribe(\"order-created\")\n",
    );
    let graph = build(vec![publisher, subscriber]);

    let mut names = event_topic_names(&graph);
    names.sort();
    assert_eq!(
        names,
        ["Order.Created", "order-created"],
        "each node keeps its own spelling"
    );
    let mirrors = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::EventTopicMirror)
        .count();
    assert_eq!(mirrors, 1, "the two spellings share one canonical key");
}

#[test]
fn test_event_topic_nodes_two_spellings_one_file_first_spelling_names_both() {
    let lg = parse_python(
        "services/both.py",
        "import redis\n\ndef a(r):\n    r.publish(\"Order.Created\", 1)\n\ndef b(r):\n    r.publish(\"order-created\", 2)\n",
    );
    let graph = build(vec![lg]);

    let names = event_topic_names(&graph);
    assert_eq!(
        names,
        ["Order.Created", "Order.Created"],
        "first spelling by source order names the node"
    );
    let uids: Vec<u64> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EventTopic)
        .map(|n| n.uid)
        .collect();
    assert_eq!(uids.len(), 2);
    assert_eq!(
        uids[0], uids[1],
        "the uid hashes the canonical key, so one topic is one identity"
    );
}

#[test]
fn test_event_topic_nodes_unrelated_topics_keep_distinct_names() {
    let lg = parse_python(
        "services/two.py",
        "import redis\n\ndef a(r):\n    r.publish(\"Order.Created\", 1)\n\ndef b(r):\n    r.publish(\"Order.Shipped\", 2)\n",
    );
    let mut names = event_topic_names(&build(vec![lg]));
    names.sort();
    assert_eq!(names, ["Order.Created", "Order.Shipped"]);
}

#[test]
fn test_python_kafka_topic_spelling_survives_build() {
    assert_keeps_spelling(&parse_python(
        "a.py",
        "from kafka import KafkaProducer\n\ndef f(p):\n    p.send(\"Order.Created\", b\"x\")\n",
    ));
}

#[test]
fn test_javascript_kafka_topic_spelling_survives_build() {
    assert_keeps_spelling(&parse_with(
        JavaScriptProvider::new(),
        "a.js",
        "import { Kafka } from 'kafkajs';\n\nasync function f(data) {\n    const producer = kafka.producer();\n    await producer.send({ topic: 'Order.Created', messages: [{ value: data }] });\n}\n",
    ));
}

#[test]
fn test_typescript_kafka_topic_spelling_survives_build() {
    assert_keeps_spelling(&parse_with(
        TypeScriptProvider::new(),
        "a.ts",
        "import { Kafka } from 'kafkajs';\n\nfunction f(data) {\n    const producer = kafka.producer();\n    producer.send({ topic: 'Order.Created', messages: [{ value: data }] });\n}\n",
    ));
}

#[test]
fn test_go_kafka_topic_spelling_survives_build() {
    assert_keeps_spelling(&parse_with(
        GoProvider::new(),
        "a.go",
        "package main\n\nimport kafka \"github.com/segmentio/kafka-go\"\n\nfunc publishOrder(writer *kafka.Writer, ctx interface{}) {\n    writer.WriteMessages(ctx, kafka.Message{\n        Topic: \"Order.Created\",\n        Value: []byte(\"hello\"),\n    })\n}\n",
    ));
}

#[test]
fn test_java_kafka_topic_spelling_survives_build() {
    assert_keeps_spelling(&parse_with(
        JavaProvider::new(),
        "A.java",
        "import org.apache.kafka.clients.producer.KafkaProducer;\nimport org.apache.kafka.clients.producer.ProducerRecord;\n\npublic class A {\n    public void f(KafkaProducer<String, String> producer, String data) {\n        producer.send(new ProducerRecord<>(\"Order.Created\", data));\n    }\n}\n",
    ));
}

#[test]
fn test_kotlin_kafka_topic_spelling_survives_build() {
    assert_keeps_spelling(&parse_with(
        KotlinProvider::new(),
        "a.kt",
        "import org.apache.kafka.clients.producer.KafkaProducer\nimport org.apache.kafka.clients.producer.ProducerRecord\n\nfun f(producer: KafkaProducer<String, String>, data: String) {\n    producer.send(ProducerRecord(\"Order.Created\", data))\n}\n",
    ));
}

#[test]
fn test_rust_kafka_topic_spelling_survives_build() {
    assert_keeps_spelling(&parse_with(
        RustProvider::new(),
        "a.rs",
        "use rdkafka::producer::{FutureProducer, FutureRecord};\n\nasync fn f(producer: &FutureProducer) {\n    producer.send(\n        FutureRecord::to(\"Order.Created\").payload(\"hello\"),\n        std::time::Duration::from_secs(0),\n    ).await;\n}\n",
    ));
}
