//! Provable-only verdict layer over `ecp diff` section payloads.
//!
//! Verdicts are derived purely from delta facts that `diff` already
//! produces. We never invent new heuristics here — every verdict cites
//! the exact section + record that triggered it. The verdict layer's job
//! is **interpretation packaging**: severity tagging, cross-section
//! synthesis (e.g., "changed symbol + cross-file callers in current
//! graph"), and pruning to API-surface kinds so LLM context doesn't drown
//! in internal renames.
//!
//! Rejected by design: any verdict requiring semantic guesswork
//! ("looks like X", "probably broken"). If we cannot point at a graph
//! edge / node / hash, the verdict is not emitted.

use crate::commands::diff::routes::{RouteConsumer, RoutesDiff};
use crate::commands::diff::symbols::{
    BlindSpotRef, CallerRef, CrossFileCaller, IndirectDispatchRef, SymbolRef, SymbolsDiff,
};
use crate::commands::diff::DiffPayload;
use rustc_hash::FxHashMap;
use serde::Serialize;

/// Single review verdict. `kind` is the discriminator; `detail` is a
/// short human-readable line; structured payload lives in the optional
/// fields. Serialization is flat so LLM JSON traversal stays cheap.
#[derive(Debug, Serialize)]
pub struct Verdict {
    pub kind: VerdictKind,
    pub severity: Severity,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intra_callers: Option<Vec<VerdictCaller>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_callers: Option<Vec<VerdictCaller>>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VerdictKind {
    /// `symbols.certain.symbols_changed` — at least one byte inside the
    /// symbol's source span differs. Cannot distinguish signature vs body
    /// without per-parser support (v1 limitation).
    SignatureOrBodyChanged,
    /// `symbols.certain.symbols_added` with kind in {Function, Method,
    /// Constructor, Class, Struct, Enum, Trait, Interface, Route}.
    NewPublicSurface,
    /// `symbols.certain.symbols_removed` with kind in same surface set.
    /// Always Risk — removal of a public symbol without explicit confirmation
    /// is the most common silent-break vector.
    RemovedPublicSurface,
    /// `routes.added` / `routes.removed` / `routes.modified` — Route node
    /// or its RouteShape changed.
    RouteContractChanged,
    /// `symbols.unknown.blindspots_in_diff_region` — graph has a
    /// BlindSpotRecord inside one of the modified files; callers downstream
    /// of that site cannot be enumerated.
    BlindspotInDiffRegion,
    /// `symbols.unknown.indirect_dispatches_in_diff_region` — `CallMeta`
    /// marks an indirect dispatch (vtable / trait-object / interface call,
    /// or callback / Fn / fn-ptr) inside a function in the diff region.
    /// Candidate targets ARE in the graph (unlike BlindSpot), but the
    /// refactor must verify all impls / closure-passing call sites still
    /// satisfy the contract. Currently emitted by `indirect_dispatch.rs`
    /// for C / C++ / Rust / JS / TS / Python; other langs see no signal
    /// here yet (FU-001 P1–P7 BlindSpot emitter rollout).
    IndirectDispatchInDiffRegion,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
    /// Informational — no action required, surfaced for context.
    Info,
    /// Verify required — change has reachable callers or non-trivial blast.
    Warn,
    /// Manual confirmation required — removal / contract break /
    /// blindspot inside diff. Skipping these is the silent-break vector.
    Risk,
}

#[derive(Debug, Serialize, Clone)]
pub struct VerdictCaller {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// API-surface kinds — symbols where add/remove warrants a verdict.
/// Excludes Variable/Property/Const because intra-module data deltas
/// generate too much noise without semantic value at the review layer.
fn is_public_surface(kind: &str) -> bool {
    matches!(
        kind,
        "Function"
            | "Method"
            | "Constructor"
            | "Class"
            | "Struct"
            | "Enum"
            | "Trait"
            | "Interface"
            | "Route"
            | "EventTopic"
            | "SchemaField"
    )
}

/// Derive verdicts from a complete `ecp diff` payload. Sections that are
/// `None` in the payload simply skip their corresponding verdict family.
pub fn derive(payload: &DiffPayload) -> Vec<Verdict> {
    let mut out: Vec<Verdict> = Vec::new();

    if let Some(s) = payload.symbols.as_ref() {
        out.extend(verdicts_from_symbols(s));
    }
    if let Some(r) = payload.routes.as_ref() {
        out.extend(verdicts_from_routes(r));
    }

    out.sort_by(|a, b| {
        severity_rank(b.severity)
            .cmp(&severity_rank(a.severity))
            .then(a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    out
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Risk => 2,
        Severity::Warn => 1,
        Severity::Info => 0,
    }
}

fn verdicts_from_symbols(s: &SymbolsDiff) -> Vec<Verdict> {
    let mut out: Vec<Verdict> = Vec::new();

    // Build (target_path, target_name) → caller-slice maps once before the
    // changed-symbol loop. Without these, attaching callers to each verdict
    // is O(M × N) — for M=100 changed symbols × N=50 caller buckets, that's
    // 5 000 string compares on a hot path. The map-lookup variant is O(M).
    let intra_by_target: FxHashMap<(&str, &str), &[CallerRef]> = s
        .certain
        .intra_file_callers
        .iter()
        .map(|p| {
            (
                (p.target_path.as_str(), p.target_name.as_str()),
                p.callers.as_slice(),
            )
        })
        .collect();
    let cross_by_target: FxHashMap<(&str, &str), &[CrossFileCaller]> = s
        .heuristic
        .cross_file_callers
        .iter()
        .map(|p| {
            (
                (p.target_path.as_str(), p.target_name.as_str()),
                p.candidates.as_slice(),
            )
        })
        .collect();

    // ── SignatureOrBodyChanged ──────────────────────────────────────────
    // Synthesize cross-section: attach matching intra-file + cross-file
    // callers to each changed symbol. Severity escalates with caller
    // count (Risk if cross-file callers exist, Warn if intra-file only,
    // Info if no callers — internal-only change).
    for ch in &s.certain.symbols_changed {
        let key = (ch.path.as_str(), ch.name.as_str());
        let intra: Vec<VerdictCaller> = intra_by_target
            .get(&key)
            .map(|callers| {
                callers
                    .iter()
                    .map(|c| VerdictCaller {
                        path: ch.path.clone(),
                        name: c.name.clone(),
                        kind: c.kind.clone(),
                        line: c.line,
                        confidence: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let cross: Vec<VerdictCaller> = cross_by_target
            .get(&key)
            .map(|candidates| {
                candidates
                    .iter()
                    .map(|c| VerdictCaller {
                        path: c.path.clone(),
                        name: c.name.clone(),
                        kind: c.kind.clone(),
                        line: c.line,
                        confidence: Some(c.confidence),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let severity = if !cross.is_empty() {
            Severity::Risk
        } else if !intra.is_empty() {
            Severity::Warn
        } else {
            Severity::Info
        };
        let detail = format!(
            "{} {} changed (hash {} → {}); {} intra-file caller(s), {} cross-file candidate(s)",
            ch.kind,
            display_qualified(&ch.owner_class, &ch.name),
            &ch.baseline_hash[..7.min(ch.baseline_hash.len())],
            &ch.current_hash[..7.min(ch.current_hash.len())],
            intra.len(),
            cross.len(),
        );
        out.push(Verdict {
            kind: VerdictKind::SignatureOrBodyChanged,
            severity,
            path: ch.path.clone(),
            line: Some(ch.line),
            symbol: Some(display_qualified(&ch.owner_class, &ch.name)),
            detail,
            intra_callers: (!intra.is_empty()).then_some(intra),
            cross_callers: (!cross.is_empty()).then_some(cross),
        });
    }

    // ── NewPublicSurface ────────────────────────────────────────────────
    for sym in &s.certain.symbols_added {
        if !is_public_surface(&sym.kind) {
            continue;
        }
        out.push(symbol_verdict(
            sym,
            VerdictKind::NewPublicSurface,
            Severity::Info,
            "new public surface",
        ));
    }

    // ── RemovedPublicSurface ────────────────────────────────────────────
    for sym in &s.certain.symbols_removed {
        if !is_public_surface(&sym.kind) {
            continue;
        }
        out.push(symbol_verdict(
            sym,
            VerdictKind::RemovedPublicSurface,
            Severity::Risk,
            "removed public symbol — verify no external callers remain",
        ));
    }

    // ── BlindspotInDiffRegion ───────────────────────────────────────────
    for bs in &s.unknown.blindspots_in_diff_region {
        out.push(blindspot_verdict(bs));
    }

    // ── IndirectDispatchInDiffRegion ────────────────────────────────────
    // Surface CallMeta-flagged indirect calls in the diff region. Severity
    // stays Warn (mirrors BlindSpot): targets exist in graph, so it's not
    // RISK, but a refactor still needs LLM attention because direct caller
    // chasing won't enumerate the actual runtime target.
    out.extend(
        s.unknown
            .indirect_dispatches_in_diff_region
            .iter()
            .map(indirect_dispatch_verdict),
    );

    out
}

fn verdicts_from_routes(r: &RoutesDiff) -> Vec<Verdict> {
    let mut out: Vec<Verdict> = Vec::new();
    for added in &r.added {
        // New route + consumers landing together is an intentional pairing,
        // not a break. Severity stays Info; surface consumers for context.
        let cross = consumers_as_callers(&added.consumers);
        out.push(Verdict {
            kind: VerdictKind::RouteContractChanged,
            severity: Severity::Info,
            path: added.handler_file.clone(),
            line: Some(added.handler_line),
            symbol: Some(format!("{} {}", added.method, added.path)),
            detail: format!(
                "route added: {} {}{}",
                added.method,
                added.path,
                consumer_suffix(added.consumers.len()),
            ),
            intra_callers: None,
            cross_callers: (!cross.is_empty()).then_some(cross),
        });
    }
    for removed in &r.removed {
        let cross = consumers_as_callers(&removed.consumers);
        out.push(Verdict {
            kind: VerdictKind::RouteContractChanged,
            severity: Severity::Risk,
            path: removed.handler_file.clone(),
            line: Some(removed.handler_line),
            symbol: Some(format!("{} {}", removed.method, removed.path)),
            detail: format!(
                "route removed: {} {} — verify all consumers migrated{}",
                removed.method,
                removed.path,
                consumer_suffix(removed.consumers.len()),
            ),
            intra_callers: None,
            cross_callers: (!cross.is_empty()).then_some(cross),
        });
    }
    for chg in &r.modified {
        // Consumers come from the current snapshot — they're what may break.
        let cross = consumers_as_callers(&chg.after.consumers);
        // Modification with known cross-language consumers is the silent-
        // break vector this verdict exists to catch: escalate Warn → Risk.
        let severity = if cross.is_empty() {
            Severity::Warn
        } else {
            Severity::Risk
        };
        out.push(Verdict {
            kind: VerdictKind::RouteContractChanged,
            severity,
            path: chg.after.handler_file.clone(),
            line: Some(chg.after.handler_line),
            symbol: Some(format!("{} {}", chg.after.method, chg.after.path)),
            detail: format!(
                "route modified: {} {} (handler relocated){}",
                chg.after.method,
                chg.after.path,
                consumer_suffix(chg.after.consumers.len()),
            ),
            intra_callers: None,
            cross_callers: (!cross.is_empty()).then_some(cross),
        });
    }
    out
}

fn consumers_as_callers(consumers: &[RouteConsumer]) -> Vec<VerdictCaller> {
    consumers
        .iter()
        .map(|c| VerdictCaller {
            path: c.path.clone(),
            name: String::new(),
            kind: "Consumer".into(),
            line: 0,
            confidence: Some(c.confidence),
        })
        .collect()
}

fn consumer_suffix(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(
            " ({n} cross-lang consumer{})",
            if n == 1 { "" } else { "s" }
        )
    }
}

fn symbol_verdict(
    sym: &SymbolRef,
    kind: VerdictKind,
    severity: Severity,
    detail_lead: &str,
) -> Verdict {
    Verdict {
        kind,
        severity,
        path: sym.path.clone(),
        line: Some(sym.line),
        symbol: Some(display_qualified(&sym.owner_class, &sym.name)),
        detail: format!(
            "{}: {} {}",
            detail_lead,
            sym.kind,
            display_qualified(&sym.owner_class, &sym.name)
        ),
        intra_callers: None,
        cross_callers: None,
    }
}

fn indirect_dispatch_verdict(id: &IndirectDispatchRef) -> Verdict {
    let dispatch_label = if id.dispatch_type.is_empty() {
        "<unknown>"
    } else {
        id.dispatch_type.as_str()
    };
    let detail = format!(
        "indirect {} via {} in {} — verify all impls / callback sites still satisfy contract",
        id.kind, dispatch_label, id.caller,
    );
    Verdict {
        kind: VerdictKind::IndirectDispatchInDiffRegion,
        severity: Severity::Warn,
        path: id.path.clone(),
        line: Some(id.line),
        symbol: Some(id.caller.clone()),
        detail,
        intra_callers: None,
        cross_callers: None,
    }
}

fn blindspot_verdict(bs: &BlindSpotRef) -> Verdict {
    Verdict {
        kind: VerdictKind::BlindspotInDiffRegion,
        severity: Severity::Warn,
        path: bs.path.clone(),
        line: Some(bs.line),
        symbol: None,
        detail: if bs.hint.is_empty() {
            format!("blindspot ({}) inside modified file", bs.kind)
        } else {
            format!("blindspot ({}) inside modified file: {}", bs.kind, bs.hint)
        },
        intra_callers: None,
        cross_callers: None,
    }
}

fn display_qualified(owner_class: &str, name: &str) -> String {
    if owner_class.is_empty() {
        name.to_string()
    } else {
        format!("{owner_class}::{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::diff::symbols::{
        BlindSpotRef as BS, CertainBucket, CrossFileCallersOf, HeuristicBucket, IntraFileCallersOf,
        SymbolChange, SymbolsDiff, UnknownBucket,
    };

    fn ch(path: &str, name: &str, line: u32) -> SymbolChange {
        SymbolChange {
            path: path.into(),
            owner_class: String::new(),
            name: name.into(),
            kind: "Function".into(),
            line,
            baseline_hash: "aaaaaaa".into(),
            current_hash: "bbbbbbb".into(),
            current_node_idx: 0,
        }
    }

    #[test]
    fn severity_escalates_with_caller_set() {
        // Internal-only change (no callers) → Info
        let s_info = SymbolsDiff {
            certain: CertainBucket {
                symbols_changed: vec![ch("a.rs", "internal", 10)],
                ..Default::default()
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s_info);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].severity, Severity::Info);

        // Intra-file callers only → Warn
        let s_warn = SymbolsDiff {
            certain: CertainBucket {
                symbols_changed: vec![ch("a.rs", "shared", 10)],
                intra_file_callers: vec![IntraFileCallersOf {
                    target_path: "a.rs".into(),
                    target_name: "shared".into(),
                    target_kind: "Function".into(),
                    callers: vec![crate::commands::diff::symbols::CallerRef {
                        name: "caller_in_same_file".into(),
                        kind: "Function".into(),
                        line: 50,
                    }],
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s_warn);
        assert_eq!(v[0].severity, Severity::Warn);

        // Cross-file candidates → Risk
        let s_risk = SymbolsDiff {
            certain: CertainBucket {
                symbols_changed: vec![ch("a.rs", "exported", 10)],
                ..Default::default()
            },
            heuristic: HeuristicBucket {
                cross_file_callers: vec![CrossFileCallersOf {
                    target_path: "a.rs".into(),
                    target_name: "exported".into(),
                    target_kind: "Function".into(),
                    min_confidence: 0.85,
                    candidates: vec![CrossFileCaller {
                        path: "b.rs".into(),
                        name: "external_caller".into(),
                        kind: "Function".into(),
                        line: 20,
                        confidence: 0.85,
                        reason: "import".into(),
                    }],
                }],
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s_risk);
        assert_eq!(v[0].severity, Severity::Risk);
    }

    #[test]
    fn removed_public_surface_is_risk() {
        let s = SymbolsDiff {
            certain: CertainBucket {
                symbols_removed: vec![SymbolRef {
                    path: "a.rs".into(),
                    owner_class: String::new(),
                    name: "public_fn".into(),
                    kind: "Function".into(),
                    line: 5,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s);
        assert_eq!(v[0].kind, VerdictKind::RemovedPublicSurface);
        assert_eq!(v[0].severity, Severity::Risk);
    }

    #[test]
    fn blindspot_emits_warn() {
        let s = SymbolsDiff {
            unknown: UnknownBucket {
                blindspots_in_diff_region: vec![BS {
                    path: "a.rs".into(),
                    line: 42,
                    kind: "dynamic_call".into(),
                    hint: "callback".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s);
        assert_eq!(v[0].kind, VerdictKind::BlindspotInDiffRegion);
        assert_eq!(v[0].severity, Severity::Warn);
    }

    #[test]
    fn indirect_dispatch_emits_warn_with_dispatch_type_in_detail() {
        let s = SymbolsDiff {
            unknown: UnknownBucket {
                indirect_dispatches_in_diff_region: vec![IndirectDispatchRef {
                    path: "src/handler.rs".into(),
                    line: 17,
                    kind: "dynamic_dispatch".into(),
                    dispatch_type: "Box<dyn Handler>".into(),
                    caller: "run".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, VerdictKind::IndirectDispatchInDiffRegion);
        assert_eq!(v[0].severity, Severity::Warn);
        assert_eq!(v[0].symbol.as_deref(), Some("run"));
        assert!(
            v[0].detail.contains("Box<dyn Handler>"),
            "expected dispatch_type in detail; got: {}",
            v[0].detail
        );
        assert!(
            v[0].detail.contains("dynamic_dispatch"),
            "expected kind in detail; got: {}",
            v[0].detail
        );
    }

    #[test]
    fn indirect_dispatch_callback_with_unknown_dispatch_type_renders_placeholder() {
        let s = SymbolsDiff {
            unknown: UnknownBucket {
                indirect_dispatches_in_diff_region: vec![IndirectDispatchRef {
                    path: "src/loop.c".into(),
                    line: 99,
                    kind: "callback".into(),
                    dispatch_type: String::new(),
                    caller: "dispatch_loop".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s);
        assert_eq!(v[0].kind, VerdictKind::IndirectDispatchInDiffRegion);
        assert!(v[0].detail.contains("<unknown>"));
        assert!(v[0].detail.contains("callback"));
    }

    #[test]
    fn is_public_surface_kind_filter() {
        assert!(is_public_surface("Function"));
        assert!(is_public_surface("Method"));
        assert!(is_public_surface("Route"));
        assert!(is_public_surface("EventTopic"));
        assert!(!is_public_surface("Variable"));
        assert!(!is_public_surface("Property"));
        assert!(!is_public_surface("Const"));
    }

    #[test]
    fn route_modified_with_consumers_escalates_to_risk() {
        use crate::commands::diff::routes::{RouteChange, RouteEntry, RoutesDiff};
        let before = RouteEntry {
            method: "POST".into(),
            path: "/api/orders".into(),
            handler_file: "server/orders.ts".into(),
            handler_line: 10,
            consumers: vec![],
        };
        let after = RouteEntry {
            method: "POST".into(),
            path: "/api/orders".into(),
            handler_file: "server/orders.ts".into(),
            handler_line: 42, // handler relocated
            consumers: vec![RouteConsumer {
                path: "web/src/api/orders.ts".into(),
                confidence: 0.9,
            }],
        };
        let r = RoutesDiff {
            modified: vec![RouteChange { before, after }],
            ..Default::default()
        };
        let v = verdicts_from_routes(&r);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, VerdictKind::RouteContractChanged);
        assert_eq!(v[0].severity, Severity::Risk);
        let cross = v[0].cross_callers.as_ref().expect("cross_callers attached");
        assert_eq!(cross.len(), 1);
        assert_eq!(cross[0].path, "web/src/api/orders.ts");
        assert_eq!(cross[0].confidence, Some(0.9));
    }

    #[test]
    fn route_modified_without_consumers_stays_warn() {
        use crate::commands::diff::routes::{RouteChange, RouteEntry, RoutesDiff};
        let mk = |line| RouteEntry {
            method: "GET".into(),
            path: "/api/health".into(),
            handler_file: "server/health.ts".into(),
            handler_line: line,
            consumers: vec![],
        };
        let r = RoutesDiff {
            modified: vec![RouteChange {
                before: mk(5),
                after: mk(7),
            }],
            ..Default::default()
        };
        let v = verdicts_from_routes(&r);
        assert_eq!(v[0].severity, Severity::Warn);
        assert!(v[0].cross_callers.is_none());
    }

    #[test]
    fn route_added_with_consumers_stays_info() {
        use crate::commands::diff::routes::{RouteEntry, RoutesDiff};
        let r = RoutesDiff {
            added: vec![RouteEntry {
                method: "GET".into(),
                path: "/api/users".into(),
                handler_file: "server/users.ts".into(),
                handler_line: 1,
                consumers: vec![RouteConsumer {
                    path: "web/users.ts".into(),
                    confidence: 0.9,
                }],
            }],
            ..Default::default()
        };
        let v = verdicts_from_routes(&r);
        assert_eq!(v[0].severity, Severity::Info);
        assert!(v[0].cross_callers.is_some());
    }

    #[test]
    fn verdicts_sorted_risk_first() {
        let s = SymbolsDiff {
            certain: CertainBucket {
                symbols_changed: vec![ch("z.rs", "internal", 1)], // Info
                symbols_added: vec![SymbolRef {
                    path: "a.rs".into(),
                    owner_class: String::new(),
                    name: "new_fn".into(),
                    kind: "Function".into(),
                    line: 1,
                }], // Info (new is Info-level)
                symbols_removed: vec![SymbolRef {
                    path: "m.rs".into(),
                    owner_class: String::new(),
                    name: "gone_fn".into(),
                    kind: "Function".into(),
                    line: 1,
                }], // Risk
                ..Default::default()
            },
            ..Default::default()
        };
        let v = verdicts_from_symbols(&s);
        // Sort via derive()-style sorting.
        let mut sorted = v;
        sorted.sort_by_key(|v| std::cmp::Reverse(severity_rank(v.severity)));
        assert_eq!(sorted[0].severity, Severity::Risk);
    }
}
