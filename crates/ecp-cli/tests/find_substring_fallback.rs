//! The substring fallback (`ecp find --mode bm25` before a tantivy index
//! exists) feeds the PreToolUse hook through `compute_hits`, whose consumer
//! takes the leading rows as the top hits. The rows must therefore come back
//! best score first, and the ASCII fast path must score exactly like the
//! lowercase comparison it replaces.

use ecp_cli::commands::find::{compute_hits, run_for_repo, FindArgs, FindMode, ScoreSource};
use ecp_cli::commands::hook::pre_tool_use::format_hits;
use ecp_cli::engine::Engine;
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use tempfile::tempdir;

fn load_fixture(names: &[&str]) -> (tempfile::TempDir, Engine) {
    let mut fx = GraphFixture::new();
    for (i, name) in names.iter().enumerate() {
        let id = fx.func("src/lib.rs", name);
        fx.span(id, (i as u32 + 1, 0, i as u32 + 2, 0));
    }
    let graph = fx.build();
    let dir = tempdir().unwrap();
    // No `tantivy/` beside graph.bin: this is the pre-index state.
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, rkyv::to_bytes::<Error>(&graph).unwrap()).unwrap();
    let engine = Engine::load(&path).unwrap();
    (dir, engine)
}

fn bm25_args(pattern: &str) -> FindArgs {
    FindArgs {
        pattern: Some(pattern.to_string()),
        mode: FindMode::Bm25,
        fuzzy: false,
        all: false,
        include_tests: false,
        kind: None,
        file: None,
        repo: None,
        format: None,
        batch: false,
    }
}

#[test]
fn test_compute_hits_substring_fallback_puts_exact_match_first() {
    // Six 0.4 substring rows precede the exact match in node order; the hook
    // shows five rows, so node order would never have shown `handle`.
    let names = [
        "xhandle1", "xhandle2", "xhandle3", "xhandle4", "xhandle5", "xhandle6", "handle",
    ];
    let (_dir, engine) = load_fixture(&names);
    let hits = compute_hits(bm25_args("handle"), &engine).unwrap();
    assert_eq!(hits.len(), 7);
    assert_eq!(hits[0].name, "handle");
    assert_eq!(hits[0].score, 1.0);
    assert!(hits
        .iter()
        .all(|h| h.score_source == ScoreSource::Substring));
    assert!(
        hits.windows(2).all(|w| w[0].score >= w[1].score),
        "rows must be best score first"
    );
    let rendered = format_hits(&hits);
    let first_row = rendered.lines().nth(1).unwrap();
    assert!(first_row.contains("handle (src/lib.rs:8)"), "{rendered}");
}

/// `ecp group find` (both merge modes) consumes `run_for_repo`'s rows in
/// the order returned, with no re-sort of its own; on the fallback that
/// order is now best score first rather than node order.
#[test]
fn test_run_for_repo_fallback_orders_best_score_first() {
    let names = ["xhandle1", "xhandle2", "handleClick", "handle"];
    let (_dir, engine) = load_fixture(&names);
    let hits = run_for_repo(&engine, "member", "handle", None).unwrap();
    let ordered: Vec<(&str, f32)> = hits.iter().map(|h| (h.name.as_str(), h.score)).collect();
    assert_eq!(
        ordered,
        vec![
            ("handle", 1.0),
            ("handleClick", 0.7),
            ("xhandle1", 0.4),
            ("xhandle2", 0.4),
        ]
    );
    assert!(hits.iter().all(|h| h.repo.as_deref() == Some("member")));
}

/// `ecp find ""` is accepted by clap; the fallback must answer, not panic
/// on a zero-width window.
#[test]
fn test_substring_fallback_empty_pattern_matches_every_name_as_prefix() {
    let (_dir, engine) = load_fixture(&["alpha", "beta"]);
    let hits = compute_hits(bm25_args(""), &engine).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.score == 0.7));
}

/// The hook hands BM25 a space-joined term list; on the fallback a name
/// scores against its best single term, since no name contains a space.
#[test]
fn test_substring_fallback_scores_multiword_pattern_per_term() {
    let (_dir, engine) = load_fixture(&["HookInput", "pub_use_re", "unrelated"]);
    let hits = compute_hits(bm25_args("pub struct HookInput"), &engine).unwrap();
    let ordered: Vec<(&str, f32)> = hits.iter().map(|h| (h.name.as_str(), h.score)).collect();
    assert_eq!(ordered, vec![("HookInput", 1.0), ("pub_use_re", 0.7)]);
}

/// Rows are capped at MULTI_CAP after ranking, so the exact match survives
/// the cap however late it sits in node order.
#[test]
fn test_substring_fallback_caps_after_ranking() {
    let filler: Vec<String> = (0..150).map(|i| format!("xhandle{i}")).collect();
    let mut names: Vec<&str> = filler.iter().map(String::as_str).collect();
    names.push("handle");
    let (_dir, engine) = load_fixture(&names);
    let hits = compute_hits(bm25_args("handle"), &engine).unwrap();
    assert_eq!(hits.len(), 100);
    assert_eq!(hits[0].name, "handle");
}

#[test]
fn test_substring_fallback_scores_ascii_and_unicode_names_alike() {
    let names = [
        "Handle",      // exact, case-insensitive
        "handleClick", // prefix
        "unhandled",   // substring
        "HANDLE_ÜBER", // non-ASCII name, prefix via the lowercase path
        "händle",      // non-ASCII name, no match
        "unrelated",
    ];
    let (_dir, engine) = load_fixture(&names);
    let hits = compute_hits(bm25_args("Handle"), &engine).unwrap();
    let mut scored: Vec<(&str, f32)> = hits.iter().map(|h| (h.name.as_str(), h.score)).collect();
    scored.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(
        scored,
        vec![
            ("HANDLE_ÜBER", 0.7),
            ("Handle", 1.0),
            ("handleClick", 0.7),
            ("unhandled", 0.4),
        ]
    );
}
