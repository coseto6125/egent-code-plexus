//! T5-1 unit tests: config-table dispatch + import-gate filtering.
//!
//! Deliberately framework-agnostic — no Kafka / RabbitMQ / NATS specifics.
//! Those belong to T5-2..T5-N.

use ecp_analyzer::event_topic::{
    classify_amqp_direction, classify_kafka_direction, extract_event_topics, EventTopicConfig,
};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Query used by all simple-assignment dispatch tests; one of the call-form
/// tests overrides this via the `query` arg to `run()`.
///
/// Pattern: `topic = "User.Created"` — captures the string node as `@topic_name`.
const TOPIC_QUERY: &str = r#"
(assignment
  left: (identifier) @var
  right: (string) @topic_name)
"#;

/// Strip Python string delimiters for assertion comparison.
fn strip_quotes(s: &str) -> &str {
    s.trim_matches('"').trim_matches('\'')
}

/// Parse `src`, build `query`, fabricate `RawImport`s from `import_sources`
/// (modelling `from <source> import *`), then run `extract_event_topics`.
/// Returns the extracted vec plus the pool that owns the interned strings.
fn run(
    src: &str,
    query: &str,
    configs: &[EventTopicConfig],
    import_sources: &[&str],
) -> (Vec<RawEventTopic>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser
        .parse(src.as_bytes(), None)
        .expect("parse returned None");
    let query = Query::new(&lang, query).expect("query compile");
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
    let result = extract_event_topics(&tree, src.as_bytes(), &query, configs, &imports, &mut pool);
    (result, pool)
}

// ---------------------------------------------------------------------------
// Synthetic configs — no real framework imports.
// ---------------------------------------------------------------------------

const CONFIG_KAFKA: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Kafka,
    topic_capture: "topic_name",
    producer_capture: "",
    direction_capture: "",
    import_gate: &["kafka"],
    direction_classifier: classify_kafka_direction,
    canonicalize: true,
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// No configs → empty Vec, no panic.
#[test]
fn test_empty_configs_returns_empty() {
    let (result, _pool) = run(r#"topic = "user.created""#, TOPIC_QUERY, &[], &[]);
    assert!(result.is_empty(), "no configs must return empty");
}

/// Config requires `kafka` import; file has no such import → empty Vec.
#[test]
fn test_import_gate_blocks_when_absent() {
    let (result, _pool) = run(
        r#"topic = "order.created""#,
        TOPIC_QUERY,
        &[CONFIG_KAFKA],
        &["pika"], // rabbitmq, not kafka
    );
    assert!(result.is_empty(), "import gate must block");
}

/// Config requires `kafka` import; file imports `kafka.producer` → emits.
#[test]
fn test_import_gate_passes_when_present() {
    let (result, _pool) = run(
        r#"topic = "order.created""#,
        TOPIC_QUERY,
        &[CONFIG_KAFKA],
        &["kafka.producer"],
    );
    assert_eq!(
        result.len(),
        1,
        "import gate should pass for kafka.producer"
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
}

/// config.canonicalize=true — raw topic `"User.Created"` is emitted as the
/// canonical form produced by T5-0 `canonicalize()`.
#[test]
fn test_canonicalize_applied_when_enabled() {
    use ecp_analyzer::event_topic::normalize::canonicalize;

    let raw_topic = "User.Created";
    let src = format!(r#"topic = "{raw_topic}""#);
    let (result, pool) = run(&src, TOPIC_QUERY, &[CONFIG_KAFKA], &["kafka"]);

    assert_eq!(result.len(), 1);
    let str_ref = result[0].topic_literal.expect("topic_literal");
    let emitted = pool.resolve(&str_ref).to_string();
    let expected = canonicalize(raw_topic);
    assert_eq!(
        emitted, expected,
        "canonicalize=true must apply T5-0 normalization"
    );
}

/// config.canonicalize=false — raw text is emitted verbatim (minus the Python
/// string delimiters that the tree-sitter capture includes).
#[test]
fn test_canonicalize_skipped_when_disabled() {
    const CONFIG_RAW: EventTopicConfig = EventTopicConfig {
        framework: FrameworkId::Kafka,
        topic_capture: "topic_name",
        producer_capture: "",
        direction_capture: "",
        import_gate: &["kafka"],
        direction_classifier: classify_kafka_direction,
        canonicalize: false,
    };

    let raw_topic = "User.Created";
    let src = format!(r#"topic = "{raw_topic}""#);
    let (result, pool) = run(&src, TOPIC_QUERY, &[CONFIG_RAW], &["kafka"]);

    assert_eq!(result.len(), 1);
    let str_ref = result[0].topic_literal.expect("topic_literal");
    let emitted = pool.resolve(&str_ref).to_string();
    assert_eq!(
        strip_quotes(&emitted),
        raw_topic,
        "canonicalize=false must not transform the topic"
    );
}

/// Two configs both gated on the same import; declaration order determines
/// which fires (first match wins).
#[test]
fn test_multiple_configs_first_match_wins() {
    const CONFIG_FIRST: EventTopicConfig = EventTopicConfig {
        framework: FrameworkId::Kafka,
        topic_capture: "topic_name",
        producer_capture: "",
        direction_capture: "",
        import_gate: &["kafka"],
        direction_classifier: classify_kafka_direction,
        canonicalize: true,
    };
    const CONFIG_SECOND: EventTopicConfig = EventTopicConfig {
        framework: FrameworkId::Sns, // different framework, same gate
        topic_capture: "topic_name",
        producer_capture: "",
        direction_capture: "",
        import_gate: &["kafka"],
        direction_classifier: classify_kafka_direction,
        canonicalize: true,
    };

    let (result, _pool) = run(
        r#"topic = "order.placed""#,
        TOPIC_QUERY,
        &[CONFIG_FIRST, CONFIG_SECOND],
        &["kafka"],
    );

    assert_eq!(
        result.len(),
        1,
        "first match wins — second config must not fire"
    );
    assert_eq!(
        result[0].lib,
        FrameworkId::Kafka,
        "first config in declaration order must win"
    );
}

/// Synthetic direction capture `"consume"` with `classify_amqp_direction`
/// must produce `PubSub::Subscribe`.
///
/// Uses a Python call expression `send("consume", "order.placed")` so that
/// both @direction_capture and @topic_name land in the same tree-sitter match.
/// (Separate assignment statements produce separate matches with only one
/// capture each — they cannot satisfy a config that requires both captures.)
#[test]
fn test_direction_classifier_invoked() {
    const CALL_QUERY: &str = r#"
(call
  arguments: (argument_list
    (string) @direction_capture
    (string) @topic_name))
"#;
    const CONFIG_AMQP: EventTopicConfig = EventTopicConfig {
        framework: FrameworkId::RabbitMq,
        topic_capture: "topic_name",
        producer_capture: "",
        direction_capture: "direction_capture",
        import_gate: &["pika"],
        direction_classifier: classify_amqp_direction,
        canonicalize: false,
    };

    let (result, pool) = run(
        r#"send("consume", "order.placed")"#,
        CALL_QUERY,
        &[CONFIG_AMQP],
        &["pika"],
    );

    assert_eq!(result.len(), 1, "expected one topic from call expression");
    assert_eq!(
        result[0].direction,
        PubSub::Subscribe,
        "classify_amqp_direction(\"consume\") must produce PubSub::Subscribe"
    );
    let str_ref = result[0].topic_literal.expect("topic_literal");
    assert_eq!(
        pool.resolve(&str_ref),
        "order.placed",
        "topic text must be emitted verbatim (canonicalize=false)"
    );
}
