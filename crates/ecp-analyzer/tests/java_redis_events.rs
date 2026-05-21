//! T5-29 integration tests: Redis pub/sub Java event-topic detector.
//!
//! Exercises the production `REDIS_JAVA` const and the real `frameworks.scm`
//! query string against spring-data-redis, Jedis, and Lettuce patterns.

use ecp_analyzer::event_topic::{extract_event_topics, REDIS_JAVA};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/java/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(&lang, FRAMEWORKS_SCM).expect("query compile");
    let imports: Vec<RawImport> = import_sources
        .iter()
        .map(|s| RawImport {
            source: (*s).to_string(),
            imported_name: s.split('.').next_back().unwrap_or("*").to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    extract_event_topics(&tree, src.as_bytes(), &query, &[REDIS_JAVA], &imports)
}

/// spring-data-redis: redisTemplate.convertAndSend("orders", msg) → Publish, topic="orders".
#[test]
fn test_spring_redis_convert_and_send_literal_channel() {
    let src = r#"
import org.springframework.data.redis.core.RedisTemplate;

public class OrderPublisher {
    public void publishOrder(RedisTemplate<String, String> redisTemplate, String data) {
        redisTemplate.convertAndSend("orders", data);
    }
}
"#;
    let result = run(src, &["org.springframework.data.redis.core.RedisTemplate"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders"
    );
}

/// Jedis: jedis.publish("payments", msg) → Publish, topic="payments".
#[test]
fn test_jedis_publish_literal_channel() {
    let src = r#"
import redis.clients.jedis.Jedis;

public class PaymentPublisher {
    public void sendPayment(Jedis jedis, String msg) {
        jedis.publish("payments", msg);
    }
}
"#;
    let result = run(src, &["redis.clients.jedis.Jedis"]);
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "payments"
    );
}

/// Lettuce: commands.subscribe("events") → Subscribe, topic="events".
#[test]
fn test_lettuce_subscribe_literal_channel() {
    let src = r#"
import io.lettuce.core.pubsub.StatefulRedisPubSubConnection;

public class EventSubscriber {
    public void listenEvents(StatefulRedisPubSubConnection<String, String> conn) {
        conn.sync().subscribe("events");
    }
}
"#;
    let result = run(
        src,
        &["io.lettuce.core.pubsub.StatefulRedisPubSubConnection"],
    );
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "events"
    );
}

/// Lettuce: commands.psubscribe("orders.*") → Subscribe, pattern stored as topic.
#[test]
fn test_lettuce_psubscribe_literal_pattern() {
    let src = r#"
import io.lettuce.core.pubsub.StatefulRedisPubSubConnection;

public class PatternSubscriber {
    public void listenPattern(StatefulRedisPubSubConnection<String, String> conn) {
        conn.sync().psubscribe("orders.*");
    }
}
"#;
    let result = run(
        src,
        &["io.lettuce.core.pubsub.StatefulRedisPubSubConnection"],
    );
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic from psubscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    // canonicalize: true converts "orders.*" → "orders/*"
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "orders/*"
    );
}

/// Lettuce reactive: commands.pSubscribe("events.*") → Subscribe (camelCase).
#[test]
fn test_lettuce_p_subscribe_camel_case() {
    let src = r#"
import io.lettuce.core.pubsub.api.reactive.RedisPubSubReactiveCommands;

public class ReactiveSubscriber {
    public void listenReactive(RedisPubSubReactiveCommands<String, String> commands) {
        commands.pSubscribe("events.*");
    }
}
"#;
    let result = run(
        src,
        &["io.lettuce.core.pubsub.api.reactive.RedisPubSubReactiveCommands"],
    );
    assert_eq!(
        result.len(),
        1,
        "expected 1 RawEventTopic from pSubscribe; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Redis);
    assert_eq!(result[0].direction, PubSub::Subscribe);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "events/*"
    );
}

/// Variable channel → no capture (no fabrication).
#[test]
fn test_redis_java_variable_channel_emits_nothing() {
    let src = r#"
import redis.clients.jedis.Jedis;

public class DynamicPublisher {
    public void publish(Jedis jedis, String channel, String msg) {
        jedis.publish(channel, msg);
    }
}
"#;
    let result = run(src, &["redis.clients.jedis.Jedis"]);
    assert!(
        result.is_empty(),
        "variable channel must produce nothing; got {:?}",
        result
    );
}

/// No redis import → empty output (import gate).
#[test]
fn test_no_redis_import_no_captures() {
    let src = r#"
import java.util.Map;

public class Handler {
    public void process(Map<String, String> data) {
        String channel = data.get("channel");
    }
}
"#;
    let result = run(src, &["java.util.Map"]);
    assert!(
        result.is_empty(),
        "non-redis import must produce nothing; got {:?}",
        result
    );
}
