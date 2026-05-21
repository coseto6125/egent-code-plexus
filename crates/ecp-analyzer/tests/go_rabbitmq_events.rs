//! T5-12 integration tests: RabbitMQ Go event-topic detector.
//!
//! Exercises the production `RABBITMQ_GO` const and the real `queries.scm`
//! query string against streadway/amqp and rabbitmq/amqp091-go patterns.

use ecp_analyzer::event_topic::{extract_event_topics, RABBITMQ_GO};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

const QUERIES_SCM: &str = include_str!("../src/go/queries.scm");

fn run(
    src: &str,
    import_sources: &[&str],
) -> (Vec<ecp_core::analyzer::types::RawEventTopic>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(&lang, QUERIES_SCM).expect("query compile");
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
        &[RABBITMQ_GO],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// streadway/amqp: channel.Publish(exchange, routingKey, ...) → Publish.
#[test]
fn test_amqp_publish_literal_routing_key() {
    let src = r#"
package main

import amqp "github.com/streadway/amqp"

func publishOrder(ch *amqp.Channel, body []byte) error {
    return ch.Publish("exchange", "orders", false, false, amqp.Publishing{
        Body: body,
    })
}
"#;
    let (result, pool) = run(src, &["streadway/amqp"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// streadway/amqp: channel.Consume(queue, ...) → Subscribe.
#[test]
fn test_amqp_consume_literal_queue() {
    let src = r#"
package main

import amqp "github.com/streadway/amqp"

func consumeOrders(ch *amqp.Channel) (<-chan amqp.Delivery, error) {
    return ch.Consume("orders", "", true, false, false, false, nil)
}
"#;
    let (result, pool) = run(src, &["streadway/amqp"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "orders");
}

/// rabbitmq/amqp091-go: channel.Publish with same API surface.
#[test]
fn test_amqp091_publish_literal_routing_key() {
    let src = r#"
package main

import amqp "github.com/rabbitmq/amqp091-go"

func sendNotification(ch *amqp.Channel, body []byte) error {
    return ch.Publish("notifications", "email", false, false, amqp.Publishing{
        Body: body,
    })
}
"#;
    let (result, pool) = run(src, &["rabbitmq/amqp091-go"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "email");
}

/// channel.Get(queue, ...) → Subscribe direction.
#[test]
fn test_amqp_get_literal_queue() {
    let src = r#"
package main

import amqp "github.com/streadway/amqp"

func pollQueue(ch *amqp.Channel) (amqp.Delivery, bool, error) {
    return ch.Get("task_queue", true)
}
"#;
    let (result, pool) = run(src, &["streadway/amqp"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "task/queue");
}

/// Variable routing key → no capture (no fabrication).
#[test]
fn test_variable_routing_key_emits_nothing() {
    let src = r#"
package main

import amqp "github.com/streadway/amqp"

func publishDynamic(ch *amqp.Channel, routingKey string, body []byte) error {
    return ch.Publish("exchange", routingKey, false, false, amqp.Publishing{Body: body})
}
"#;
    let (result, _pool) = run(src, &["streadway/amqp"]);
    assert!(
        result.is_empty(),
        "variable routing_key must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No RabbitMQ import → empty output (import gate enforces isolation).
#[test]
fn test_no_rabbitmq_import_emits_nothing() {
    let src = r#"
package main

import "fmt"

func publishOrder() {
    ch.Publish("exchange", "orders", false, false, nil)
    fmt.Println("done")
}
"#;
    let (result, _pool) = run(src, &["fmt"]);
    assert!(
        result.is_empty(),
        "non-rabbitmq import must produce nothing; got {:?}",
        result
    );
}

/// Both Publish and Consume in the same file → 2 topics.
#[test]
fn test_publish_and_consume_same_file() {
    let src = r#"
package main

import amqp "github.com/streadway/amqp"

func sendEvent(ch *amqp.Channel, body []byte) error {
    return ch.Publish("x", "payments", false, false, amqp.Publishing{Body: body})
}

func receiveEvent(ch *amqp.Channel) (<-chan amqp.Delivery, error) {
    return ch.Consume("payments", "", true, false, false, false, nil)
}
"#;
    let (result, pool) = run(src, &["streadway/amqp"]);
    assert_eq!(result.len(), 2, "expected 2 topics; got {:?}", result);
    let publish = result.iter().find(|r| r.direction == PubSub::Publish);
    let subscribe = result.iter().find(|r| r.direction == PubSub::Subscribe);
    assert!(publish.is_some(), "must have a Publish event");
    assert!(subscribe.is_some(), "must have a Subscribe event");
    let pub_lit = publish
        .unwrap()
        .topic_literal
        .expect("publish topic must be Some");
    let sub_lit = subscribe
        .unwrap()
        .topic_literal
        .expect("subscribe topic must be Some");
    assert_eq!(pool.resolve(&pub_lit), "payments");
    assert_eq!(pool.resolve(&sub_lit), "payments");
}
