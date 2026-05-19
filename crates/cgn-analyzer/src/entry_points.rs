//! Cross-language entry-point scorer.
//!
//! Pure consumer of three signal sources already emitted by language
//! parsers — `RawRoute` (HTTP route consumer calls), `RawFrameworkRef`
//! (framework-decorated functions like Spring `@RestController`, FastAPI
//! `@app.get`, NestJS `@Controller`), and `RawNode` (any parsed symbol,
//! used here only for `main()` detection). Produces a deduped, scored
//! list of `EntryPoint`s the builder emits as `NodeKind::EntryPoint`
//! marker nodes alongside their underlying handler.
//!
//! ## Scoring (spec §9 Q1)
//!
//! | Signal | Score | EntryKind |
//! |---|---:|---|
//! | HTTP route handler | 1.0 | `HttpRoute` |
//! | Language `main` convention | 0.9 | `MainFunction` |
//! | Framework decorator (confidence ≥ 0.8) | 0.8 | `FrameworkRef` |
//! | Public exported symbol imported externally | 0.5 | `PublicExport` |
//!
//! `PublicExport` is **deferred** for v1 — see "Design decisions" in the
//! commit body. The scorer ships the `EntryKind::PublicExport` variant
//! but `score_entry_points` never emits it, so consumers don't break
//! when we light it up later.
//!
//! ## Dedup / collision rules
//!
//! A single underlying node (matched by `(file_path, symbol_name)`) can
//! be scored by multiple signals — e.g. a Java method that is both
//! `main()` AND carries a `@RestController` decorator. In that case the
//! highest-scoring signal wins; the loser is dropped so the graph
//! doesn't end up with duplicate `EntryPoint` markers. Provenance for
//! the dropped signal is preserved in the winner's `reason` string for
//! downstream LLM inspection.

use graph_nexus_core::analyzer::types::{RawFrameworkRef, RawNode, RawRoute};
use graph_nexus_core::graph::NodeKind;

/// One scored entry point. The builder emits one `NodeKind::EntryPoint`
/// marker node per `EntryPoint` and a `References` edge from the marker
/// to the underlying handler.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryPoint {
    /// Short name of the underlying handler node (matched via
    /// `(file_path, name)` against `LocalGraph.nodes`). Not a UID —
    /// resolution to a node index happens inside the builder where
    /// the `SymbolTable` lives.
    pub uid: String,
    /// What kind of signal scored this entry point.
    pub kind: EntryKind,
    /// 0.0..=1.0 score; higher means more likely to be a real entry point.
    pub score: f32,
    /// Human-readable provenance string consumed by downstream LLM
    /// tooling (rendered into the graph edge's `reason` field).
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// Function/method that handles an HTTP route (FastAPI `@app.get`,
    /// Express `app.get(...)`, Spring `@RequestMapping`, etc.).
    HttpRoute,
    /// Language `main()` per the convention table in `is_main_function`.
    MainFunction,
    /// Framework decorator with confidence ≥ 0.8 (Spring `@RestController`,
    /// NestJS `@Controller`, Celery `@app.task`, etc.).
    FrameworkRef,
    /// Public exported symbol imported by another file. **Not emitted in
    /// v1** — see module docs. Variant kept so the public surface is
    /// stable once we turn it on.
    PublicExport,
}

impl EntryKind {
    /// Stable lowercase identifier used in `EntryPoint.reason` strings
    /// and as the prefix in edge `reason` fields ingested by `gnx`.
    pub const fn tag(self) -> &'static str {
        match self {
            EntryKind::HttpRoute => "route",
            EntryKind::MainFunction => "main",
            EntryKind::FrameworkRef => "framework_ref",
            EntryKind::PublicExport => "public_export",
        }
    }
}

/// Score floor for framework decorators to count as an entry point.
/// Below this, the decorator is treated as decoration only — Python
/// `@lru_cache` (no framework_confidence assigned) or low-signal helper
/// macros wouldn't make sense as entry points. Matches the spec §9 Q1
/// proposal where the FrameworkRef bucket sits at 0.8.
const FRAMEWORK_CONFIDENCE_FLOOR: f32 = 0.8;

/// Score the three signal sources into a deduplicated, sorted (highest
/// first) entry-point list.
///
/// `routes`, `framework_refs`, and `nodes` are all expected to belong
/// to a **single file** (one `LocalGraph`'s slices). Cross-file
/// deduplication is irrelevant here because (file_path, name) keys are
/// unique inside a file by construction (the parser doesn't emit two
/// nodes with the same name at the same span — even overloads get
/// distinct synthetic names).
pub fn score_entry_points(
    routes: &[RawRoute],
    framework_refs: &[RawFrameworkRef],
    nodes: &[RawNode],
) -> Vec<EntryPoint> {
    // (handler_name, entry_point) pairs; we keep insertion order then
    // dedup by name keeping the highest score. Capacity heuristic:
    // routes + framework_refs + at most one main per file.
    let mut acc: Vec<EntryPoint> = Vec::with_capacity(routes.len() + framework_refs.len() + 1);

    // 1. Routes — strongest signal (1.0).
    for route in routes {
        let Some(handler) = &route.handler else {
            // Imperative `app.get(...)` with no extracted handler name
            // is registered as a Route node directly by the builder; it
            // doesn't have a function symbol to mark as an entry point.
            continue;
        };
        if handler.is_empty() {
            continue;
        }
        let reason = format!("route:{} {}", route.method, route.path);
        acc.push(EntryPoint {
            uid: handler.clone(),
            kind: EntryKind::HttpRoute,
            score: 1.0,
            reason,
        });
    }

    // Routes can also be inferred from decorators on RawNodes; those are
    // turned into Route nodes by the builder's Pass 1.5 but the decorated
    // function itself is the entry-point handler. Walk decorators here
    // (mirroring builder Pass 1.5's loop, but only to extract the
    // handler name) so the scorer doesn't miss decorator-style routes.
    for node in nodes {
        for dec in &node.decorators {
            if let Some(detected) = crate::route_detector::detect_from_decorator(dec) {
                let reason = format!("route:{} {}", detected.method, detected.path);
                acc.push(EntryPoint {
                    uid: node.name.clone(),
                    kind: EntryKind::HttpRoute,
                    score: 1.0,
                    reason,
                });
            }
        }
    }

    // 2. Language `main` convention (0.9). Detection is delegated to
    // `is_main_function` which inspects (name, kind, decorators) without
    // touching the source text — keeps the scorer parser-agnostic.
    for node in nodes {
        if is_main_function(node) {
            acc.push(EntryPoint {
                uid: node.name.clone(),
                kind: EntryKind::MainFunction,
                score: 0.9,
                reason: format!("main:{}", node.name),
            });
        }
    }

    // 3. Framework decorators with confidence ≥ 0.8.
    for fw in framework_refs {
        if fw.confidence < FRAMEWORK_CONFIDENCE_FLOOR {
            continue;
        }
        // RawFrameworkRef.source_name is sometimes the sentinel
        // `<module>` (see `framework_helpers::MODULE_LEVEL_SOURCE`) when
        // the decorator decorates a module-level statement rather than
        // a named function. Skip those — they don't have a callable
        // handler to mark as an entry point.
        if fw.source_name == crate::framework_helpers::MODULE_LEVEL_SOURCE
            || fw.source_name.is_empty()
        {
            continue;
        }
        acc.push(EntryPoint {
            uid: fw.source_name.clone(),
            kind: EntryKind::FrameworkRef,
            score: fw.confidence.clamp(0.0, 1.0),
            reason: format!("framework_ref:{} ({})", fw.reason, fw.confidence),
        });
    }

    // 4. PublicExport (deferred) — not emitted in v1. See module docs.

    // Dedup by handler name, keeping the highest-scoring signal. The
    // dropped signals' provenance is folded into the winner's reason
    // string with a "; also " separator so LLM tooling can read all
    // overlapping signals from a single edge.
    dedup_keep_highest(acc)
}

/// Per-language `main()` convention detector. Operates only on
/// `RawNode`'s shape (name + kind + decorators + heritage + decorators).
/// Avoids re-parsing source text by leaning on the data each language
/// parser already populates.
///
/// Coverage matrix:
///
/// | Language | Convention | RawNode signal |
/// |---|---|---|
/// | Java | `public static void main(String[] args)` | `name == "main"`, kind `Method`, **see note** |
/// | Kotlin | `fun main(args: Array<String>) {}` | `name == "main"`, kind `Function` |
/// | C# | `static void Main(string[] args)` / `static int Main` / `static async Task Main` / top-level statements | `name == "Main"`, kind `Method` |
/// | Go | `func main() {}` (in `package main`) | `name == "main"`, kind `Function` |
/// | Rust | `fn main()` | `name == "main"`, kind `Function` |
/// | Swift | `@main` attribute on a struct / class, or `main.swift` top-level code | decorator `@main`, OR `name == "main"` Function |
/// | C | `int main(int argc, char **argv)` | `name == "main"`, kind `Function` |
/// | C++ | `int main()` / `int main(int, char**)` | `name == "main"`, kind `Function` |
/// | Dart | `void main(List<String> args)` | `name == "main"`, kind `Function` |
/// | TS / JS / Python / PHP / Ruby | no language-level main convention | n/a (already ✓ via routes / scripts) |
///
/// **Java note**: the Java parser emits `main` as a `Method` whose
/// enclosing class can be any type. We don't enforce `public static void`
/// because that information isn't on `RawNode` (no modifiers field). False
/// positives (a method named `main` that isn't `public static void`) are
/// rare in idiomatic Java and the scorer's 0.9 is *not* a guarantee —
/// it's a heuristic, and LLM consumers see the `reason` for context.
pub fn is_main_function(node: &RawNode) -> bool {
    // Swift `@main` attribute on a struct/class/enum — language-level entry
    // marker independent of the symbol's name. Per Swift Evolution SE-0281
    // and the apple/swift-argument-parser examples, `@main` attaches to types
    // declared with `struct` / `class` / `enum`; tree-sitter-swift emits these
    // as NodeKind::Struct / Class / Enum respectively. Previously only Class
    // and Function were checked, so `@main struct Foo` (the idiomatic form in
    // every swift-argument-parser example) silently dropped its entry score.
    if matches!(
        node.kind,
        NodeKind::Class | NodeKind::Function | NodeKind::Struct | NodeKind::Enum
    ) && node
        .decorators
        .iter()
        .any(|d| d.trim_start_matches('@').trim() == "main")
    {
        return true;
    }

    // Convention: a function or method named "main" or "Main". Method
    // kind is in there for Java / C# / Swift / Kotlin where `main` lives
    // inside a class or companion object; Function kind covers Rust /
    // Go / C / C++ / Dart / Kotlin top-level / Swift `main.swift`.
    if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
        return false;
    }
    matches!(node.name.as_str(), "main" | "Main")
}

/// Collapse same-handler `EntryPoint`s by keeping the highest score and
/// folding the rest into a `"; also: ..."` suffix. Output is sorted
/// (score desc, kind, uid) for deterministic graph generation across
/// runs (rkyv hash stability).
fn dedup_keep_highest(mut items: Vec<EntryPoint>) -> Vec<EntryPoint> {
    if items.is_empty() {
        return items;
    }
    // Sort so the highest-score-per-uid entry sorts first.
    items.sort_by(|a, b| {
        a.uid
            .cmp(&b.uid)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.kind.tag().cmp(b.kind.tag()))
    });

    let mut out: Vec<EntryPoint> = Vec::with_capacity(items.len());
    for ep in items {
        match out.last_mut() {
            Some(prev) if prev.uid == ep.uid => {
                // ep is the lower-scoring sibling; fold its reason in,
                // but skip if the same fragment is already present —
                // happens when a decorator-style route is reported both
                // by the builder's Pass 1.5 (as RawRoute) and by the
                // scorer's own decorator walk, yielding identical
                // `reason` strings.
                if !prev.reason.contains(ep.reason.as_str()) {
                    prev.reason.push_str("; also: ");
                    prev.reason.push_str(&ep.reason);
                }
            }
            _ => out.push(ep),
        }
    }

    // Final sort: score desc, then by uid for determinism.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.uid.cmp(&b.uid))
            .then_with(|| a.kind.tag().cmp(b.kind.tag()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::analyzer::types::{RawFrameworkRef, RawNode, RawRoute};
    use graph_nexus_core::graph::NodeKind;

    fn mk_node(name: &str, kind: NodeKind) -> RawNode {
        RawNode {
            name: name.into(),
            kind,
            span: (0, 0, 0, 0),
            is_exported: false,
            heritage: vec![],
            type_annotation: None,
            decorators: vec![],
            calls: vec![],
        }
    }

    /// Rust `fn main()` — most basic case, validates the Function +
    /// "main" name path.
    #[test]
    fn rust_fn_main_detected_with_score_0_9() {
        let nodes = vec![mk_node("main", NodeKind::Function)];
        let eps = score_entry_points(&[], &[], &nodes);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::MainFunction);
        assert!((eps[0].score - 0.9).abs() < 1e-6);
        assert_eq!(eps[0].uid, "main");
    }

    /// Java `public static void main(String[] args)` — emitted by the
    /// Java parser as a Method (lives in a class), not a Function. The
    /// scorer must handle both Function and Method kinds.
    #[test]
    fn java_static_void_main_detected_as_method() {
        let nodes = vec![mk_node("main", NodeKind::Method)];
        let eps = score_entry_points(&[], &[], &nodes);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::MainFunction);
        assert!((eps[0].score - 0.9).abs() < 1e-6);
    }

    /// C# `static void Main(string[] args)` — capital M variant.
    #[test]
    fn csharp_capital_main_detected() {
        let nodes = vec![mk_node("Main", NodeKind::Method)];
        let eps = score_entry_points(&[], &[], &nodes);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::MainFunction);
    }

    /// Swift `@main` attribute on a struct — entry point even though
    /// the struct isn't named "main".
    #[test]
    fn swift_at_main_attribute_detected_on_struct() {
        let mut node = mk_node("MyApp", NodeKind::Class);
        node.decorators.push("@main".into());
        let eps = score_entry_points(&[], &[], &[node]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::MainFunction);
        assert_eq!(eps[0].uid, "MyApp");
    }

    /// Swift `@main struct Foo { … }` — the idiomatic form in every
    /// apple/swift-argument-parser example. Tree-sitter-swift emits Foo as
    /// NodeKind::Struct (not Class), so `is_main_function` must accept Struct.
    #[test]
    fn swift_at_main_struct_kind_detected() {
        let mut node = mk_node("Repeat", NodeKind::Struct);
        node.decorators.push("@main".into());
        let eps = score_entry_points(&[], &[], &[node]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::MainFunction);
        assert_eq!(eps[0].uid, "Repeat");
    }

    /// Swift `@main enum Foo { static func main() {…} }` — less common
    /// than struct but a documented `@main` form (SE-0281).
    #[test]
    fn swift_at_main_enum_kind_detected() {
        let mut node = mk_node("AppMain", NodeKind::Enum);
        node.decorators.push("@main".into());
        let eps = score_entry_points(&[], &[], &[node]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::MainFunction);
    }

    /// FastAPI `@app.get("/items")` — emitted by the Python parser as a
    /// `RawRoute` with `handler = Some("read_items")`. Score 1.0.
    #[test]
    fn fastapi_route_handler_scored_at_1_0() {
        let routes = vec![RawRoute {
            method: "GET".into(),
            path: "/items".into(),
            handler: Some("read_items".into()),
            span: (0, 0, 0, 0),
        }];
        let nodes = vec![mk_node("read_items", NodeKind::Function)];
        let eps = score_entry_points(&routes, &[], &nodes);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::HttpRoute);
        assert!((eps[0].score - 1.0).abs() < 1e-6);
        assert_eq!(eps[0].uid, "read_items");
        assert!(eps[0].reason.contains("GET"));
        assert!(eps[0].reason.contains("/items"));
    }

    /// Spring `@RestController` style: route extracted from a decorator
    /// on a Method RawNode (no RawRoute emitted). The scorer must catch
    /// this by walking decorators directly.
    #[test]
    fn spring_decorator_style_route_detected() {
        let mut node = mk_node("getUsers", NodeKind::Method);
        node.decorators.push("@GetMapping(\"/users\")".into());
        let eps = score_entry_points(&[], &[], &[node]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::HttpRoute);
        assert!((eps[0].score - 1.0).abs() < 1e-6);
    }

    /// FrameworkRef at confidence 0.9 → emitted at score 0.9 (not 0.8;
    /// the original confidence is preserved as the score). Floors at
    /// 0.8 are gating, not clamping.
    #[test]
    fn framework_ref_above_floor_keeps_original_confidence() {
        let fw = vec![RawFrameworkRef {
            source_name: "handler".into(),
            target_name: "wired_dep".into(),
            confidence: 0.9,
            reason: "spring-autowired".into(),
            span: (0, 0, 0, 0),
        }];
        let nodes = vec![mk_node("handler", NodeKind::Method)];
        let eps = score_entry_points(&[], &fw, &nodes);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].kind, EntryKind::FrameworkRef);
        assert!((eps[0].score - 0.9).abs() < 1e-6);
    }

    /// FrameworkRef below 0.8 → dropped.
    #[test]
    fn framework_ref_below_floor_dropped() {
        let fw = vec![RawFrameworkRef {
            source_name: "handler".into(),
            target_name: "low_signal_dep".into(),
            confidence: 0.5,
            reason: "fastapi-depends-low-conf".into(),
            span: (0, 0, 0, 0),
        }];
        let eps = score_entry_points(&[], &fw, &[]);
        assert!(eps.is_empty());
    }

    /// Module-level framework refs (`source_name == "<module>"`) are
    /// skipped — they don't have a callable handler to mark.
    #[test]
    fn module_level_framework_ref_skipped() {
        let fw = vec![RawFrameworkRef {
            source_name: crate::framework_helpers::MODULE_LEVEL_SOURCE.into(),
            target_name: "task_runner".into(),
            confidence: 0.95,
            reason: "celery-task".into(),
            span: (0, 0, 0, 0),
        }];
        let eps = score_entry_points(&[], &fw, &[]);
        assert!(eps.is_empty());
    }

    /// Collision: a Java method that is BOTH `main` AND carries a
    /// `@RestController` decorator. Route wins (1.0 > 0.9). The
    /// dropped signal's provenance survives in the reason.
    #[test]
    fn collision_route_outranks_main_provenance_preserved() {
        let mut node = mk_node("main", NodeKind::Method);
        node.decorators.push("@GetMapping(\"/health\")".into());
        let eps = score_entry_points(&[], &[], &[node]);
        assert_eq!(eps.len(), 1, "collision must dedupe to one entry");
        assert_eq!(eps[0].kind, EntryKind::HttpRoute);
        assert!((eps[0].score - 1.0).abs() < 1e-6);
        // Lower-scoring "main" signal folded into the reason.
        assert!(
            eps[0].reason.contains("also") && eps[0].reason.contains("main"),
            "expected dropped main signal to be folded into reason: {}",
            eps[0].reason
        );
    }

    /// Output ordering: score desc, then uid asc. Locks in determinism
    /// for rkyv graph.bin hash stability.
    #[test]
    fn output_sorted_score_desc_then_uid_asc() {
        let routes = vec![
            RawRoute {
                method: "GET".into(),
                path: "/a".into(),
                handler: Some("zzz_handler".into()),
                span: (0, 0, 0, 0),
            },
            RawRoute {
                method: "GET".into(),
                path: "/b".into(),
                handler: Some("aaa_handler".into()),
                span: (0, 0, 0, 0),
            },
        ];
        let nodes = vec![
            mk_node("zzz_handler", NodeKind::Function),
            mk_node("aaa_handler", NodeKind::Function),
            mk_node("main", NodeKind::Function),
        ];
        let eps = score_entry_points(&routes, &[], &nodes);
        assert_eq!(eps.len(), 3);
        // Two 1.0s (routes) sorted by uid asc, then 0.9 main.
        assert_eq!(eps[0].uid, "aaa_handler");
        assert_eq!(eps[1].uid, "zzz_handler");
        assert_eq!(eps[2].uid, "main");
    }

    /// Imperative route with no handler name (e.g. `app.get("/", () =>
    /// {...})`) is registered as a Route node by the builder but has no
    /// function symbol to mark — scorer skips it (would emit a dangling
    /// EntryPoint otherwise).
    #[test]
    fn imperative_route_without_handler_skipped() {
        let routes = vec![RawRoute {
            method: "GET".into(),
            path: "/".into(),
            handler: None,
            span: (0, 0, 0, 0),
        }];
        let eps = score_entry_points(&routes, &[], &[]);
        assert!(eps.is_empty());
    }

    /// Empty inputs → empty output (no panics on prefix-sum / dedup
    /// edge cases).
    #[test]
    fn empty_inputs_yield_empty_output() {
        let eps = score_entry_points(&[], &[], &[]);
        assert!(eps.is_empty());
    }

    /// Regression: FastAPI / Flask decorator-style routes are surfaced
    /// twice — once by the builder's Pass 1.5 (as a `RawRoute`) and once
    /// by `score_entry_points` itself walking `node.decorators`. Both
    /// paths emit identical `reason` strings, and the previous dedup
    /// would fold the second into `"X; also: X"`. Verify the duplicate
    /// reason fragment is not appended.
    #[test]
    fn decorator_double_report_does_not_duplicate_reason() {
        let mut handler = mk_node("list_items", NodeKind::Function);
        handler.decorators.push("@app.get(\"/items\")".to_string());

        let routes = vec![RawRoute {
            method: "GET".into(),
            path: "/items".into(),
            handler: Some("list_items".into()),
            span: (0, 0, 0, 0),
        }];

        let eps = score_entry_points(&routes, &[], &[handler]);
        assert_eq!(eps.len(), 1, "must dedup to one entry, got {:?}", eps);
        let reason = &eps[0].reason;
        assert!(
            !reason.contains("; also: route:GET /items"),
            "duplicate reason fragment must not be folded in; got {:?}",
            reason
        );
        // The base reason itself stays.
        assert_eq!(reason, "route:GET /items");
    }
}
