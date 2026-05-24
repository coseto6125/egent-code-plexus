# T5 event-topic detector coverage

One row per (lib, lang) pair planned in roadmap tasks T5-2 through T5-31.

## Status legend

- ✅ shipped: detector merged to main, has integration tests
- 🟡 in-flight: open PR, CI passing
- ❌ SKIP: explicitly out of scope per roadmap — no first-class client or <1% adoption

## Semantic differences (LLM-critical)

The five transports represented by `FrameworkId::*` have distinct durability
and delivery semantics. An LLM querying `ecp impact` must interpret the
`Publishes` and `Subscribes` edges differently depending on the framework:

- **Kafka** (`FrameworkId::Kafka`): durable, partitioned log. Subscribers can
  replay from any offset. A `RawEventTopic` with `direction: Publish` means the
  message is written to the log and retained; no subscriber being online at
  publish time does not lose the message.

- **RabbitMQ** (`FrameworkId::RabbitMq`): queued broker, AMQP 0-9-1. The
  broker holds messages in a queue until a consumer ACKs them. Topic strings in
  the graph may be routing keys, queue names, or exchange names — the `kind`
  field to distinguish these is deferred (see **Schema gaps** below). A missing
  subscriber in the graph means the queue accumulates; messages are not lost
  immediately.

- **SQS** (`FrameworkId::Sqs`): queued, AWS-managed, at-least-once delivery.
  Topic strings are full QueueUrl strings (not canonicalized). Renaming a
  QueueUrl breaks in-flight messages; `ecp impact` must trace both
  `SendMessage` producers and `ReceiveMessage` consumers across all affected
  services.

- **Celery** (`FrameworkId::Celery`): broker-backed task queue (Redis or
  RabbitMQ as backend). `RawEventTopic` captures call sites (`delay`,
  `apply_async`, `send_task`) — the enqueue / publish side. The consumer (task
  definition decorated with `@task`) is already in the graph as a `FrameworkRef`
  node from the existing decorator capture. Celery is Python-only in the ecp
  graph; cross-language Celery clients exist but are <1% adoption.

- **Redis pub/sub** (`FrameworkId::Redis`): fire-and-forget channel messaging.
  No broker queue; if no subscriber is online when `publish` fires, the message
  is silently discarded. An `ecp impact` query showing a Redis publish site with
  no active subscriber in the graph means the message is lost — not deferred.
  `psubscribe` glob patterns are canonicalized into slash-separated topic
  strings just like plain channel names; the `kind` field to distinguish the
  original source shape is deferred (see **Schema gaps** below).

---

## Kafka

Kafka is a durable, partitioned log transport. Producers write to named topics;
consumers replay from any offset. Both producer and consumer call sites are
captured where tree-sitter query patterns permit (consumer support was deferred
in some PRs; see Notes column). Java and Kotlin share identical import gates
since they target the same JVM libraries.

| Roadmap | Lib   | Lang       | Status              | Client libs (import gate)                                              | Notes                                                                               |
|---------|-------|------------|---------------------|------------------------------------------------------------------------|-------------------------------------------------------------------------------------|
| T5-2    | Kafka | Python     | ✅ shipped (#289)   | `kafka`, `aiokafka`, `confluent_kafka`, `faust`                        | Producer only (`send`, `produce`). Faust `app.send(Topic)` deferred (non-literal). |
| T5-3    | Kafka | TypeScript | 🟡 #303             | `kafkajs`, `node-rdkafka`                                              | Same config as T5-4 (`KAFKA_NODE` shared). Producer only.                           |
| T5-4    | Kafka | JavaScript | 🟡 #303             | `kafkajs`, `node-rdkafka`                                              | Shares `KAFKA_NODE` config with T5-3.                                               |
| T5-5    | Kafka | Java       | 🟡 #303             | `org.apache.kafka`, `org.springframework.kafka`                        | Producer + consumer (`send`, `subscribe`). Same gate as T5-5 Kotlin.               |
| T5-5    | Kafka | Kotlin     | 🟡 #303             | `org.apache.kafka`, `org.springframework.kafka`                        | Producer + consumer. JVM symmetry with Java; separate `KAFKA_KOTLIN` config.        |
| T5-6    | Kafka | Go         | 🟡 #303             | `github.com/segmentio/kafka-go`, `github.com/Shopify/sarama`, `github.com/confluentinc/confluent-kafka-go/kafka` | Producer + consumer (`WriteMessages`/`SendMessage`/`Produce` vs `ReadMessage`/`FetchMessage`/`ConsumeMessage`). |
| T5-7    | Kafka | Rust       | 🟡 #303             | `rdkafka`                                                              | Producer + consumer (`FutureRecord::to` vs `consumer.subscribe`).                  |

---

## RabbitMQ

RabbitMQ is a queued AMQP broker. Both routing-key (publish) and queue-name
(subscribe) strings are captured into `topic_literal`. The `kind` field to
distinguish them is deferred — see **Schema gaps**.

| Roadmap | Lib      | Lang       | Status              | Client libs (import gate)                                           | Notes                                                                                     |
|---------|----------|------------|---------------------|---------------------------------------------------------------------|-------------------------------------------------------------------------------------------|
| T5-8    | RabbitMQ | Python     | 🟡 #297             | `pika`, `aio_pika`, `kombu`                                         | `routing_key` (publish) and `queue` (subscribe). `direction_capture: "amqp.direction"`.   |
| T5-9    | RabbitMQ | TypeScript | 🟡 #297             | `amqplib`, `amqp-connection-manager`                                | `routingKey` (2nd positional) for publish; `queue` (1st positional) for consume/assertQueue. |
| T5-10   | RabbitMQ | JavaScript | 🟡 #297             | `amqplib`, `amqp-connection-manager`                                | Same gate and config as T5-9 (`RABBITMQ_JS`).                                            |
| T5-11   | RabbitMQ | Java       | 🟡 #297             | `org.springframework.amqp`, `com.rabbitmq.client`                   | Spring `convertAndSend` / `@RabbitListener`; bare client `basicPublish` / `basicConsume`. |
| T5-12   | RabbitMQ | Go         | 🟡 #297             | `github.com/streadway/amqp`, `github.com/rabbitmq/amqp091-go`       | `channel.Publish` (routingKey 2nd arg) and `channel.Consume` (queue 1st arg).             |
| T5-13   | RabbitMQ | Rust       | 🟡 #297             | `lapin`, `amiquip`                                                  | `basic_publish` (routing_key 2nd arg) and `basic_consume` (queue 1st arg).                |

---

## SQS

SQS is an AWS-managed durable queue with at-least-once delivery. The topic
literal is the full `QueueUrl` string; canonicalization is disabled
(`canonicalize: false`) to preserve the URL as-is. Producer verbs are
`send_message` / `SendMessage`; consumer verbs are `receive_message` /
`ReceiveMessage`.

| Roadmap | Lib | Lang       | Status              | Client libs (import gate)                                          | Notes                                                                                       |
|---------|-----|------------|---------------------|--------------------------------------------------------------------|----------------------------------------------------------------------------------------------|
| T5-14   | SQS | Python     | 🟡 #310             | `boto3`, `aioboto3`                                                | `QueueUrl` kwarg string literal. `canonicalize: false`.                                     |
| T5-15   | SQS | TypeScript | 🟡 #310             | `@aws-sdk/client-sqs`                                              | SDK v3 Command constructor `{ QueueUrl: "..." }` property literal.                          |
| T5-16   | SQS | JavaScript | 🟡 #310             | `@aws-sdk/client-sqs`                                              | Same gate and config as T5-15 (`SQS_JS`).                                                   |
| T5-17   | SQS | Java       | 🟡 #310             | `software.amazon.awssdk.services.sqs`                              | SDK v2 builder `.queueUrl("…")` call; string literal from builder chain.                    |
| T5-18   | SQS | Go         | 🟡 #310             | `github.com/aws/aws-sdk-go-v2/service/sqs`                         | Struct literal `&sqs.SendMessageInput{QueueUrl: aws.String("…")}`.                          |
| T5-19   | SQS | Rust       | 🟡 #310             | `aws_sdk_sqs`                                                      | Fluent builder `.queue_url("…")`; `producer_capture` empty (enclosing-fn anchor not viable in fluent chains). |

---

## Celery

Celery is a distributed task queue backed by a pluggable broker (Redis or
RabbitMQ). The detector captures task enqueue call sites (`delay`,
`apply_async`, `send_task`) — the publish side. The consumer (task function
decorated with `@task`) is already in the graph via the existing `FrameworkRef`
path and is not duplicated here. Non-Python Celery clients are SKIP per roadmap
T5-21..T5-25: `celery-java` exists but has <1% adoption; no first-class TS, JS,
Go, or Rust clients exist.

| Roadmap | Lib    | Lang       | Status              | Client libs (import gate) | Notes                                                                                                     |
|---------|--------|------------|---------------------|---------------------------|-----------------------------------------------------------------------------------------------------------|
| T5-20   | Celery | Python     | ✅ shipped (#307)   | `celery`                  | `delay`, `apply_async`, `send_task` → Publish only. Consumer captured by existing `FrameworkRef` path.   |
| T5-21   | Celery | TypeScript | ❌ SKIP             | —                         | No first-class TS Celery client.                                                                          |
| T5-22   | Celery | JavaScript | ❌ SKIP             | —                         | No first-class JS Celery client.                                                                          |
| T5-23   | Celery | Java       | ❌ SKIP             | —                         | `celery-java` exists but <1% adoption.                                                                    |
| T5-24   | Celery | Go         | ❌ SKIP             | —                         | No first-class Go Celery client; <1% adoption.                                                            |
| T5-25   | Celery | Rust       | ❌ SKIP             | —                         | No first-class Rust Celery client; <1% adoption.                                                          |

---

## Redis pub/sub

Redis pub/sub is a fire-and-forget channel transport with no broker persistence.
Messages are lost if no subscriber is online at publish time. Both plain channel
names (`publish`/`subscribe`) and glob patterns (`psubscribe`) are captured into
`topic_literal`; the `kind` field to distinguish them is deferred (see **Schema
gaps**).

| Roadmap | Lib   | Lang       | Status              | Client libs (import gate)                                                              | Notes                                                                             |
|---------|-------|------------|---------------------|----------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------|
| T5-26   | Redis | Python     | 🟡 #306             | `redis`, `aioredis`                                                                    | `publish` (Publish), `subscribe` / `psubscribe` (Subscribe). Glob patterns stored as-is. |
| T5-27   | Redis | TypeScript | 🟡 #306             | `redis`, `ioredis`                                                                     | `publish` (Publish), `subscribe` (Subscribe). Same import gate as T5-28.          |
| T5-28   | Redis | JavaScript | 🟡 #306             | `redis`, `ioredis`                                                                     | Same config as T5-27 (`REDIS_JS` / `REDIS_TS` share gate).                        |
| T5-29   | Redis | Java       | 🟡 #306             | `org.springframework.data.redis`, `redis.clients.jedis`, `io.lettuce.core`             | Spring Data Redis `convertAndSend` / Jedis `publish` / Lettuce `publish`.         |
| T5-30   | Redis | Go         | 🟡 #306             | `github.com/redis/go-redis`, `github.com/go-redis/redis`, `github.com/gomodule/redigo` | `Publish` (Publish), `Subscribe` / `PSubscribe` (Subscribe).                     |
| T5-31   | Redis | Rust       | 🟡 #306             | `redis`                                                                                | `publish` (Publish), `subscribe` / `psubscribe` (Subscribe).                     |

---

## Schema gaps

Two known gaps in `RawEventTopic` that block precise LLM queries. Both are
deferred to a dedicated schema-migration PR.

### Missing `kind: Option<StrRef>` on `RawEventTopic`

Currently `topic_literal` stores the raw string without semantic annotation.
Two cases where this causes ambiguity:

1. **RabbitMQ** (T5-8 followup): the stored string may be a `routing_key`, a
   `queue` name, or an `exchange` name. Fanout exchanges ignore routing keys
   entirely; without a `kind` field the graph cannot distinguish these. The fix
   is `RawEventTopic { kind: Option<StrRef> }` with values `"routing_key"`,
   `"queue"`, `"exchange"` — append-only.

2. **Redis psubscribe** (T5-26 followup): a `psubscribe("orders.*")` glob
   pattern and a plain `publish("orders.created", ...)` channel name both
   canonicalize to slash-separated topic strings. Without a `kind` field the
   graph still cannot tell whether the original source was a literal channel or
   a glob to expand. The fix adds `kind: "channel"` vs `"pattern"` —
   append-only.

### Single-arg subscribe capture (multi-arg deferred)

The tree-sitter query for Redis `subscribe` anchors on the **first positional
string literal only**. `pubsub.subscribe("ch1", "ch2")` produces one capture
(`"ch1"`); `"ch2"` is silently ignored. Each literal channel produces its own
`RawEventTopic` when written as separate subscribe calls. Multi-arg capture
requires either N separate capture names or a post-processing pass, neither of
which `EventTopicConfig` currently supports. Tracked as T5-26-followup.

---

## Coverage caveats

### Module-level calls are omitted

All detectors anchor the tree-sitter query to an enclosing function node
(`function_definition` in Python, `function_declaration` / `method_definition`
in TS/JS, `function_item` in Rust, etc.) so that `RawEventTopic.enclosing_fn`
can be populated alongside the topic literal in a single match. Module-level or
script-level calls (outside any function) are not captured. This is intentional:
module-level event emissions represent <1% of production usage and would produce
`enclosing_fn = ""` entries that degrade `ecp impact` query quality.

### Variable arguments produce no capture

Wherever a topic argument is a variable, expression, or interpolated string
rather than a string literal, the tree-sitter query produces no match and no
`RawEventTopic` is emitted. This is the "no fabrication" invariant: honest
`topic_literal: None` (→ `BlindSpot { kind: "<lib>-dynamic-topic" }`) always
beats a guessed value. LLMs querying the graph must treat a missing capture as
"unknown, not absent" and use grep as the fallback path.
