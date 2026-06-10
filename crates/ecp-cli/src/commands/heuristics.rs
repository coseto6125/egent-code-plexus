//! `ecp heuristics <kind>` — consolidated heuristic-surfacer subcommands.
//!
//! ## LLM-utility framing
//!
//! All findings from every subcommand carry `requires_verification: true`.
//! Confidence semantics are kind-specific:
//!
//! - `saga`          — `<0.75` → `BLIND_SPOT`; `0.75–0.85` → `POSSIBLY_RELATED`
//!   (backed by `CompensatedBy` in-graph edge; queryable via
//!   `ecp cypher 'MATCH ()-[r:CompensatedBy]->() …'`).
//! - `schema-bindings` — `>=0.85` → `LIKELY_RELATED`; below → `BLIND_SPOT`
//!   (backed by `MirrorsField` in-graph edge emitted by T4-8).
//! - `event-mirrors`   — all edges at `confidence=0.85` → `HEURISTIC`
//!   (backed by `EventTopicMirror` in-graph edge emitted by T5-33;
//!   `--lib` filter is a no-op until FrameworkId is persisted on archived nodes).
//!
//! No finding is promoted into the graph by query-time re-scanning.
//! Use `ecp impact --heuristics` to include heuristic edges in blast-radius.

use crate::commands::{find_event_mirrors, find_schema_bindings, find_tx_patterns};
use crate::engine::Engine;
use clap::{Args, Subcommand};
use ecp_core::EcpError;

#[derive(Args, Debug, Clone)]
pub struct HeuristicsArgs {
    #[command(subcommand)]
    pub kind: HeuristicsKind,
}

#[derive(Subcommand, Debug, Clone)]
pub enum HeuristicsKind {
    /// Heuristic Saga compensate/undo/rollback name-pair detector + Outbox pattern scanner.
    ///
    /// Reads `CompensatedBy` edges from the graph (emitted at index time) for Saga pairs.
    /// Outbox detection is a query-time name-scan: `OutboxEvent`/`event_outbox`/`message_outbox`
    /// class names reachable via `Calls` → `Publishes` within BFS depth 5.
    ///
    /// All findings carry `requires_verification: true`. Saga pairs are backed by the
    /// in-graph `CompensatedBy` edge; Outbox findings remain query-time only.
    Saga(find_tx_patterns::FindTxPatternsArgs),

    /// Surface `MirrorsField` heuristic edges for a `SchemaField`; list blind-spot candidates.
    ///
    /// Blind spots are cross-owner-class fields that share the name but have no mirror edge.
    /// Accepts `Class.field` or bare `field`. Confidence `>=0.85` → `LIKELY_RELATED`;
    /// below → `BLIND_SPOT`. All findings carry `requires_verification: true`.
    #[command(name = "schema-bindings")]
    SchemaBindings(find_schema_bindings::FindSchemaBindingsArgs),

    /// List `EventTopicMirror` heuristic edges: (publisher_fn, subscriber_fn, topic, confidence).
    ///
    /// Edges are emitted by T5-33 at `confidence=0.85`. Filter with `--min-confidence`,
    /// `--topic` (glob), `--lib` (currently a no-op — FrameworkId not persisted in archived graph).
    /// All findings carry `requires_verification: true`.
    #[command(name = "event-mirrors")]
    EventMirrors(find_event_mirrors::FindEventMirrorsArgs),
}

pub fn run(args: HeuristicsArgs, engine: &Engine) -> Result<(), EcpError> {
    match args.kind {
        HeuristicsKind::Saga(a) => find_tx_patterns::run(a, engine),
        HeuristicsKind::SchemaBindings(a) => find_schema_bindings::run(a, engine),
        HeuristicsKind::EventMirrors(a) => find_event_mirrors::run(a, engine),
    }
}
