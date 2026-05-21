//! `EventTopicConfig` for Celery Python task-invocation clients (T5-20).
//!
//! Celery is a distributed task queue backed by a broker (Redis, RabbitMQ,
//! etc.). Task invocations (`delay`, `apply_async`, `send_task`) enqueue a
//! message on the broker; the task-function body is the consumer side.
//!
//! # Scope: publish-side only
//! This detector captures call sites that *enqueue* tasks — the producer half.
//! The consumer (task definition) is already captured by the existing
//! `@celery_app.task` / `@shared_task` FrameworkRef logic in
//! `python/parser.rs:663`. Emitting a second detector for decorators would
//! duplicate FrameworkRef data into `RawEventTopic` without LLM benefit:
//! `ecp impact` already navigates from call site → task definition via the
//! FrameworkRef node. What was missing is the **topic literal** (task name)
//! on the call site so that `ecp impact` can ask "who enqueues the 'add'
//! task?" — that is the gap T5-20 fills.
//!
//! # Direction: Publish only
//! All three call forms (`delay`, `apply_async`, `send_task`) are enqueue
//! operations. The consumer is the decorated function — already in the graph
//! as a FrameworkRef, not as a `RawEventTopic`. Emitting Subscribe here would
//! require re-parsing decorator captures, which belong to a different graph
//! layer. This simplification is intentional; T5-20 tracks the enqueue path.
//!
//! # Topic literal semantics
//! - `add.delay(2, 3)` → topic `"add"` (receiver identifier of `delay`).
//! - `add.apply_async(args=[2, 3])` → topic `"add"` (same).
//! - `app.send_task("tasks.add", ...)` → topic `"tasks.add"` (first string arg).
//! - `module.task.delay(...)` → topic `"task"` (last attribute segment before `.delay`).
//! - Variable task name on `send_task` → no capture (no fabrication).
//!
//! # LLM-utility justification (graph-completeness criterion A)
//! Without this detector, `ecp impact` cannot answer "which functions enqueue
//! task 'add'?" — the call sites are invisible to the graph. A refactor that
//! renames the Celery task must find all `add.delay(...)` and
//! `app.send_task("add", ...)` call sites; without the `RawEventTopic` the
//! impact query returns zero, causing silent breakage across worker services.

use super::config::EventTopicConfig;
use ecp_core::analyzer::types::{FrameworkId, PubSub};

/// Direction classifier for Celery task-invocation call sites.
///
/// All three invocation methods (`delay`, `apply_async`, `send_task`) are
/// producer-side enqueue operations → always `Publish`. The subscriber side
/// (decorated task function) is captured by the existing FrameworkRef path.
fn classify_celery_direction(_raw: &str) -> PubSub {
    PubSub::Publish
}

/// Celery Python task-invocation detector — fires for `celery` imports.
///
/// `direction_capture: ""` signals to the extractor that no direction capture
/// is used; `classify_celery_direction` is called with an empty string and
/// always returns `Publish`.
///
/// `topic_capture: "celery.topic"` captures either the receiver identifier
/// from `<task>.delay(...)` / `<task>.apply_async(...)`, or the first string
/// literal from `app.send_task("<name>", ...)`.
pub const CELERY_PYTHON: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::Celery,
    topic_capture: "celery.topic",
    producer_capture: "celery.fn",
    direction_capture: "",
    import_gate: &["celery"],
    direction_classifier: classify_celery_direction,
    canonicalize: true,
};
