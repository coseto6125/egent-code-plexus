//! `EventTopicConfig` for RabbitMQ / AMQP Python clients.
//!
//! Covers three Python RabbitMQ libraries under a single config:
//! - `pika` (sync AMQP 0-9-1): `channel.basic_publish(routing_key=...)` / `channel.basic_consume(queue=...)`
//! - `aio_pika` (async AMQP 0-9-1): `exchange.publish(..., routing_key=...)` / `queue.consume(callback)`
//! - `kombu` (abstraction layer): `producer.publish(body, routing_key=...)`
//!
//! Direction dispatch: `classify_amqp_direction` (T5-1) maps call-method text
//! to `PubSub::Publish` / `PubSub::Subscribe` — the `direction_capture` slot
//! binds the method identifier so the classifier sees `"basic_publish"`,
//! `"basic_consume"`, `"publish"`, or `"consume"`.
//!
//! # Topic literal semantics
//! - Publish: `routing_key` kwarg value (AMQP's canonical routing unit).
//! - Subscribe: `queue` kwarg / positional arg value (what the subscriber binds to).
//!
//! Both are stored in `RawEventTopic::topic_literal`. The queue/exchange topology
//! distinction (direct vs topic vs fanout, exchange name, binding key) cannot be
//! expressed in the current `RawEventTopic` schema — see **Schema gap** below.
//!
//! # Schema gap (deferred to schema-migration PR)
//! `RawEventTopic` has no `kind` field. For RabbitMQ this loses:
//! - Whether the topic string is a routing_key, a queue name, or an exchange name.
//! - The exchange type (`direct` / `topic` / `fanout` / `headers`) — relevant because
//!   fanout ignores routing_key entirely, so the stored literal would be meaningless.
//! - Binding topology: a single queue can be bound to multiple routing patterns.
//!
//! Concrete LLM-query example that the missing field blocks:
//!   "Find all publishers whose routing_key matches subscriber queues bound to
//!    the `orders.direct` exchange" — without a `kind: Option<StrRef>` field,
//!    the graph cannot distinguish publish-side routing_key strings from
//!    subscribe-side queue names in the same `EventTopic` node set, forcing
//!    the LLM to guess topology from naming conventions rather than graph edges.
//!
//! The fix is `RawEventTopic { kind: Option<StrRef> }` added append-only, with
//! values `"routing_key"`, `"queue"`, `"exchange"` — this PR intentionally defers
//! that migration so the schema change ships in one coordinated PR.
//!
//! # LLM-utility justification (graph-completeness criterion A)
//! Without this config, `ecp impact` is blind to RabbitMQ message paths. A
//! refactor of a `publish_order` function that emits to `routing_key='orders'`
//! would show zero downstream consumers, causing the LLM to declare the change
//! safe when it actually breaks every order-processing service bound to that queue.

use super::config::EventTopicConfig;
use super::extract::classify_amqp_direction;
use ecp_core::analyzer::types::FrameworkId;

/// RabbitMQ Python detector — fires for `pika`, `aio_pika`, and `kombu` imports.
///
/// `direction_capture: "amqp.direction"` binds the call-method identifier
/// (e.g. `basic_publish`, `basic_consume`, `publish`, `consume`) so that
/// `classify_amqp_direction` can resolve `PubSub` direction without fabrication.
pub const RABBITMQ_PYTHON: EventTopicConfig = EventTopicConfig {
    framework: FrameworkId::RabbitMq,
    topic_capture: "amqp.topic",
    producer_capture: "amqp.fn",
    direction_capture: "amqp.direction",
    import_gate: &["pika", "aio_pika", "kombu"],
    direction_classifier: classify_amqp_direction,
    canonicalize: true,
};
