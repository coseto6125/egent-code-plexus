//! T5-20 integration tests: Celery Python task-invocation event-topic detector.
//!
//! Exercises the production `CELERY_PYTHON` const and the real `frameworks.scm`
//! query string against `delay`, `apply_async`, and `send_task` invocation forms.
//! Also re-verifies Kafka regression isolation.

use ecp_analyzer::event_topic::{extract_event_topics, CELERY_PYTHON, KAFKA_PYTHON};
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
        &[KAFKA_PYTHON, CELERY_PYTHON],
        &imports,
        &mut pool,
    );
    (result, pool)
}

/// `add.delay(2, 3)` → topic="add", Publish direction.
#[test]
fn test_celery_delay_literal_task_name() {
    let src = r#"
from celery import shared_task

def enqueue_add(x, y):
    add.delay(x, y)
"#;
    let (result, pool) = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from delay; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "add");
}

/// `add.apply_async(args=[2, 3])` → topic="add", Publish direction.
#[test]
fn test_celery_apply_async_literal_task_name() {
    let src = r#"
from celery import shared_task

def enqueue_add(x, y):
    add.apply_async(args=[x, y])
"#;
    let (result, pool) = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from apply_async; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "add");
}

/// `app.send_task("tasks.add", ...)` → topic="tasks.add", Publish direction.
#[test]
fn test_celery_send_task_string_literal() {
    let src = r#"
from celery import Celery

app = Celery("myapp")

def enqueue_add(x, y):
    app.send_task("tasks.add", args=[x, y])
"#;
    let (result, pool) = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from send_task; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "tasks/add");
}

/// `app.send_task(task_name_var, ...)` → no capture (variable, no fabrication).
#[test]
fn test_celery_send_task_variable_arg_emits_nothing() {
    let src = r#"
from celery import Celery

app = Celery("myapp")

def enqueue_dynamic(task_name, x):
    app.send_task(task_name, args=[x])
"#;
    let (result, _pool) = run(src, &["celery"]);
    assert!(
        result.is_empty(),
        "variable task name must produce no RawEventTopic; got {:?}",
        result
    );
}

/// No celery import → import gate stays closed, no captures emitted.
#[test]
fn test_no_celery_import_no_captures() {
    let src = r#"
import json

def enqueue():
    add.delay(1, 2)
"#;
    let (result, _pool) = run(src, &["json"]);
    assert!(
        result.is_empty(),
        "non-celery import must produce nothing; got {:?}",
        result
    );
}

/// Kafka regression: CELERY_PYTHON in slice does not break KAFKA_PYTHON.
#[test]
fn test_kafka_regression_fires_correctly_with_celery_in_slice() {
    let src = r#"
from kafka import KafkaProducer

def send_event(producer):
    producer.send("events", b"data")
"#;
    let (result, pool) = run(src, &["kafka"]);
    assert_eq!(
        result.len(),
        1,
        "Kafka regression: expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0]
        .topic_literal
        .expect("kafka topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "events");
}

/// Celery import does not fire Kafka config.
#[test]
fn test_celery_import_does_not_fire_kafka() {
    let src = r#"
from celery import shared_task

def enqueue():
    add.delay(1, 2)
"#;
    let (result, _pool) = run(src, &["celery"]);
    assert!(
        result.iter().all(|r| r.lib == FrameworkId::Celery),
        "celery import must not fire Kafka config; got libs: {:?}",
        result.iter().map(|r| r.lib).collect::<Vec<_>>()
    );
}

/// Chained `module.task.delay(...)` — topic should be "task" (last attribute
/// segment before `.delay`), not "module".
///
/// Tree-sitter resolves the `object` of the outer attribute access to the
/// `attribute` expression `module.task`; however, `@celery.topic` captures
/// the `object: (identifier)` node, which only matches when the receiver is a
/// plain identifier. A chained access like `module.task` has `object: (attribute)`,
/// not `object: (identifier)`, so it does NOT match the current pattern.
/// This test documents the current behavior (no capture) rather than assuming
/// topic="task". Tracked as T5-20-followup for chained attribute support.
#[test]
fn test_celery_chained_attribute_delay_no_capture() {
    let src = r#"
import celery

def enqueue():
    module.task.delay(1, 2)
"#;
    let (result, _pool) = run(src, &["celery"]);
    // Chained access `module.task.delay(...)` — `module.task` is an attribute
    // node (not a plain identifier), so the `object: (identifier)` capture in
    // the pattern does not match. Result is empty — documented limitation.
    assert!(
        result.is_empty(),
        "chained attribute delay: current pattern captures plain identifiers only; got {:?}",
        result
    );
}

/// Async wrapper: `await add.delay(...)` → topic="add", Publish.
#[test]
fn test_celery_await_delay_literal_task_name() {
    let src = r#"
from celery import shared_task

async def enqueue_add_async(x, y):
    await add.delay(x, y)
"#;
    let (result, pool) = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from await delay; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    let lit = result[0].topic_literal.expect("topic_literal must be Some");
    assert_eq!(pool.resolve(&lit), "add");
}
