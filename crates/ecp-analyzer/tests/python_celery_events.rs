//! T5-20 integration tests: Celery Python task-invocation event-topic detector.
//!
//! Exercises the production `CELERY_PYTHON` const and the real `frameworks.scm`
//! query string against `delay`, `apply_async`, and `send_task` invocation forms.
//! Also re-verifies Kafka regression isolation.

use ecp_analyzer::event_topic::{extract_event_topics, CELERY_PYTHON, KAFKA_PYTHON};
use ecp_core::analyzer::types::{FrameworkId, PubSub, RawEventTopic, RawImport};
use tree_sitter::{Parser, Query};

const FRAMEWORKS_SCM: &str = include_str!("../src/python/frameworks.scm");

fn run(src: &str, import_sources: &[&str]) -> Vec<RawEventTopic> {
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
    extract_event_topics(
        &tree,
        src.as_bytes(),
        &query,
        &[KAFKA_PYTHON, CELERY_PYTHON],
        &imports,
    )
}

/// `add.delay(2, 3)` → topic="add", Publish direction.
#[test]
fn test_celery_delay_literal_task_name() {
    let src = r#"
from celery import shared_task

def enqueue_add(x, y):
    add.delay(x, y)
"#;
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from delay; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "add"
    );
}

/// `add.apply_async(args=[2, 3])` → topic="add", Publish direction.
#[test]
fn test_celery_apply_async_literal_task_name() {
    let src = r#"
from celery import shared_task

def enqueue_add(x, y):
    add.apply_async(args=[x, y])
"#;
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from apply_async; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "add"
    );
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
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from send_task; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "tasks/add"
    );
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
    let result = run(src, &["celery"]);
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
    let result = run(src, &["json"]);
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
    let result = run(src, &["kafka"]);
    assert_eq!(
        result.len(),
        1,
        "Kafka regression: expected one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Kafka);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("kafka topic_literal must be Some"),
        "events"
    );
}

/// Celery import does not fire Kafka config.
#[test]
fn test_celery_import_does_not_fire_kafka() {
    let src = r#"
from celery import shared_task

def enqueue():
    add.delay(1, 2)
"#;
    let result = run(src, &["celery"]);
    assert!(
        result.iter().all(|r| r.lib == FrameworkId::Celery),
        "celery import must not fire Kafka config; got libs: {:?}",
        result.iter().map(|r| r.lib).collect::<Vec<_>>()
    );
}

/// Chained `module.task.delay(...)` — topic="task" (last attribute segment
/// before `.delay`), matching Celery's unqualified registered task-name
/// convention. Query alternation handles both `(identifier)` and
/// `(attribute attribute: (identifier))` receivers; arbitrary nesting depth
/// (e.g. `pkg.mod.task.delay(...)`) captures the last identifier.
#[test]
fn test_celery_chained_attribute_delay_captures_last_segment() {
    let src = r#"
import celery

def enqueue():
    module.task.delay(1, 2)
"#;
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "chained `module.task.delay(...)` should emit one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "task"
    );
}

/// Chained `module.task.apply_async(...)` — topic="task". Same alternation as delay.
#[test]
fn test_celery_chained_attribute_apply_async_captures_last_segment() {
    let src = r#"
import celery

def enqueue():
    module.task.apply_async(args=[1, 2])
"#;
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "chained `module.task.apply_async(...)` should emit one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].topic_literal.as_deref(), Some("task"));
}

/// Deeper chain `pkg.mod.task.delay(...)` — captures rightmost identifier ("task").
#[test]
fn test_celery_deep_chained_attribute_delay_captures_last_segment() {
    let src = r#"
import celery

def enqueue():
    pkg.mod.task.delay(1, 2)
"#;
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "deep-chain `pkg.mod.task.delay(...)` should emit one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].topic_literal.as_deref(), Some("task"));
}

/// Assignment-form: `result = tasks.add.delay(...)` — idiomatic Flask/FastAPI
/// pattern where the AsyncResult is captured. Mirrors the SQS convention of
/// duplicating queries with `(assignment right: ...)` wrap.
#[test]
fn test_celery_chained_assignment_delay_captures_last_segment() {
    let src = r#"
from celery.result import AsyncResult
from . import tasks

def enqueue():
    result = tasks.add.delay(1, 2)
"#;
    let result = run(src, &["celery.result"]);
    assert_eq!(
        result.len(),
        1,
        "`result = tasks.add.delay(...)` should emit one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].topic_literal.as_deref(), Some("add"));
}

/// Assignment-form: `result = tasks.add.apply_async(...)`.
#[test]
fn test_celery_chained_assignment_apply_async_captures_last_segment() {
    let src = r#"
import celery

def enqueue():
    result = tasks.add.apply_async(args=[1, 2])
"#;
    let result = run(src, &["celery"]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].topic_literal.as_deref(), Some("add"));
}

/// Assignment-form async: `result = await tasks.add.delay(...)`.
#[test]
fn test_celery_chained_assignment_await_delay_captures_last_segment() {
    let src = r#"
import celery

async def enqueue():
    result = await tasks.add.delay(1, 2)
"#;
    let result = run(src, &["celery"]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].topic_literal.as_deref(), Some("add"));
}

/// Async-wrapper variant of chained receiver: `await module.task.delay(...)`.
#[test]
fn test_celery_chained_await_delay_captures_last_segment() {
    let src = r#"
import celery

async def enqueue():
    await module.task.delay(1, 2)
"#;
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "chained `await module.task.delay(...)` should emit one RawEventTopic; got {:?}",
        result
    );
    assert_eq!(result[0].topic_literal.as_deref(), Some("task"));
}

/// Async wrapper: `await add.delay(...)` → topic="add", Publish.
#[test]
fn test_celery_await_delay_literal_task_name() {
    let src = r#"
from celery import shared_task

async def enqueue_add_async(x, y):
    await add.delay(x, y)
"#;
    let result = run(src, &["celery"]);
    assert_eq!(
        result.len(),
        1,
        "expected one RawEventTopic from await delay; got {:?}",
        result
    );
    assert_eq!(result[0].lib, FrameworkId::Celery);
    assert_eq!(result[0].direction, PubSub::Publish);
    assert_eq!(
        result[0]
            .topic_literal
            .as_deref()
            .expect("topic_literal must be Some"),
        "add"
    );
}
