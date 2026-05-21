//! T5-11 integration tests: RabbitMQ Java event-topic detector.
//!
//! Exercises the production `RABBITMQ_JAVA` const and the real `frameworks.scm`
//! query string against Spring AMQP and plain Java AMQP client patterns.

use ecp_analyzer::event_topic::{extract_event_topics, RABBITMQ_JAVA};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/java/frameworks.scm");
const QUERIES_SCM: &str = include_str!("../src/java/queries.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<ecp_core::analyzer::types::RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
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
    let result = extract_event_topics(&tree, src.as_bytes(), &query, &[RABBITMQ_JAVA], &imports);
    result
}

/// Spring: rabbitTemplate.convertAndSend(exchange, routingKey, payload) → Publish.
#[test]
fn test_spring_convert_and_send_literal_routing_key() {
    let src = r#"
import org.springframework.amqp.rabbit.core.RabbitTemplate;

public class OrderService {
    void publishOrder(Object payload) {
        rabbitTemplate.convertAndSend("exchange", "orders", payload);
    }
}
"#;
    let result = run(src, &["org.springframework.amqp"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "orders");
}

/// Spring: @RabbitListener(queues = "orders") → Subscribe.
#[test]
fn test_spring_rabbit_listener_annotation() {
    let src = r#"
import org.springframework.amqp.rabbit.annotation.RabbitListener;

public class OrderConsumer {
    @RabbitListener(queues = "orders")
    void handleOrder(Object msg) {}
}
"#;
    let result = run(src, &["org.springframework.amqp"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "orders");
}

/// Java AMQP client: channel.basicPublish(exchange, routingKey, ...) → Publish.
#[test]
fn test_basic_publish_literal_routing_key() {
    let src = r#"
import com.rabbitmq.client.Channel;

public class Publisher {
    void send(Channel channel, byte[] body) throws Exception {
        channel.basicPublish("", "payments", null, body);
    }
}
"#;
    let result = run(src, &["com.rabbitmq.client"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "payments");
}

/// Java AMQP client: channel.basicConsume(queue, ...) → Subscribe.
#[test]
fn test_basic_consume_literal_queue() {
    let src = r#"
import com.rabbitmq.client.Channel;

public class Consumer {
    void start(Channel channel) throws Exception {
        channel.basicConsume("notifications", true, deliverCallback, cancelCallback);
    }
}
"#;
    let result = run(src, &["com.rabbitmq.client"]);
    assert_eq!(result.len(), 1, "expected 1 topic; got {:?}", result);
    assert_eq!(result[0].lib, FrameworkId::RabbitMq);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    let lit = result[0]
        .topic_literal
        .as_deref()
        .expect("topic_literal must be Some");
    assert_eq!(lit, "notifications");
}

/// Variable routing key → no capture (no fabrication).
#[test]
fn test_variable_routing_key_emits_nothing() {
    let src = r#"
import org.springframework.amqp.rabbit.core.RabbitTemplate;

public class DynamicPublisher {
    void publish(String routingKey, Object payload) {
        rabbitTemplate.convertAndSend("exchange", routingKey, payload);
    }
}
"#;
    let result = run(src, &["org.springframework.amqp"]);
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
import java.util.logging.Logger;

public class LogService {
    void doSomething() {
        channel.basicPublish("", "orders", null, new byte[0]);
    }
}
"#;
    let result = run(src, &["java.util.logging"]);
    assert!(
        result.is_empty(),
        "non-rabbitmq import must produce nothing; got {:?}",
        result
    );
}

/// Both publish and consume in the same class → 2 topics.
#[test]
fn test_publish_and_consume_same_class() {
    let src = r#"
import com.rabbitmq.client.Channel;

public class Broker {
    void send(Channel channel, byte[] body) throws Exception {
        channel.basicPublish("", "events", null, body);
    }
    void receive(Channel channel) throws Exception {
        channel.basicConsume("events", true, deliverCallback, cancelCallback);
    }
}
"#;
    let result = run(src, &["com.rabbitmq.client"]);
    assert_eq!(result.len(), 2, "expected 2 topics; got {:?}", result);
    let publish = result.iter().find(|r| r.direction == PubSub::Publish);
    let subscribe = result.iter().find(|r| r.direction == PubSub::Subscribe);
    assert!(publish.is_some(), "must have a Publish event");
    assert!(subscribe.is_some(), "must have a Subscribe event");
    let pub_lit = publish
        .unwrap()
        .topic_literal
        .as_deref()
        .expect("publish topic must be Some");
    let sub_lit = subscribe
        .unwrap()
        .topic_literal
        .as_deref()
        .expect("subscribe topic must be Some");
    assert_eq!(pub_lit, "events");
    assert_eq!(sub_lit, "events");
}
