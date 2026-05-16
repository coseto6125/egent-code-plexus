//! `gnx scan <file>` — file-level hallucination check.
//!
//! Parses a source file via `graph_nexus_analyzer::identifier_finder`,
//! extracts all unique identifier references, then checks each against the
//! graph's symbol set. Unresolved references are listed with top-3
//! Levenshtein "did you mean?" suggestions.

use crate::commands::scan_filters;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Args, Debug, Clone)]
pub struct ScanArgs {
    /// File path to scan for symbol references
    pub file: String,

    /// Also flag identifiers that are common keywords / builtins
    #[arg(long, default_value_t = false)]
    pub strict: bool,

    /// Drop unresolved references that match the language's stdlib /
    /// keyword / common-types denylist. Per-language; based on file
    /// extension. Cuts noise by 43–100% empirically; the leftover
    /// `unresolved[]` is dominated by project symbols a typo check
    /// can actually act on. Output payload gains `filtered_count`.
    #[arg(long, default_value_t = false)]
    pub filter_stdlib: bool,

    /// Repository selector (default: cwd)
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: ScanArgs, engine: &Engine) -> Result<(), GnxError> {
    let source = std::fs::read_to_string(&args.file).map_err(GnxError::Io)?;

    // Extract all unique identifier names from the file via tree-sitter.
    let refs = graph_nexus_analyzer::identifier_finder::find_all_identifier_names(
        &args.file,
        source.as_bytes(),
    )
    .ok_or_else(|| {
        GnxError::Output(crate::hint::error_with_cause(
            &format!("scan failed: unknown language for \"{}\"", args.file),
            "extension not in supported language list",
            "run `gnx coverage --detailed` to see supported languages",
        ))
    })?;

    // Collect all symbol names from the graph into a vec for fuzzy lookup.
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let all_names: Vec<String> = graph
        .nodes
        .iter()
        .map(|n| n.name.resolve(&graph.string_pool).to_string())
        .collect();
    let name_strs: Vec<&str> = all_names.iter().map(String::as_str).collect();

    // Build the unresolved pair list first; defer JSON wrapping until after
    // filtering so we can skip the per-entry fuzzy search on dropped names.
    let mut unresolved_pairs: Vec<(String, usize)> = refs
        .into_iter()
        .filter(|(name, _)| !name_strs.contains(&name.as_str()))
        .collect();

    let filtered_count = if args.filter_stdlib {
        let (kept, dropped) = scan_filters::filter_refs(unresolved_pairs, Path::new(&args.file));
        unresolved_pairs = kept;
        dropped
    } else {
        0
    };

    let unresolved: Vec<Value> = unresolved_pairs
        .iter()
        .map(|(name, line)| {
            let suggestions = fuzzy_top_k(&name_strs, name, 3);
            json!({
                "name": name,
                "line": line,
                "did_you_mean": suggestions,
            })
        })
        .collect();

    let payload = if unresolved.is_empty() {
        let mut ok = serde_json::Map::new();
        ok.insert("status".into(), json!("ok"));
        ok.insert("file".into(), json!(args.file));
        ok.insert(
            "message".into(),
            json!(format!("File OK, 0 unresolved references")),
        );
        if filtered_count > 0 {
            ok.insert("filtered_count".into(), json!(filtered_count));
        }
        Value::Object(ok)
    } else {
        let mut issues = serde_json::Map::new();
        issues.insert("status".into(), json!("issues"));
        issues.insert("file".into(), json!(args.file));
        issues.insert("unresolved_count".into(), json!(unresolved.len()));
        if filtered_count > 0 {
            issues.insert("filtered_count".into(), json!(filtered_count));
        }
        issues.insert("unresolved".into(), Value::Array(unresolved));
        Value::Object(issues)
    };
    emit(&payload, OutputFormat::Toon)
}

/// **Phase 5 stub**: returns an empty rkyv-archived fragment. Real per-file
/// parse integration is deferred until Task 5.4 (overlay merge) is designed
/// — the merge mechanism informs what container shape this function must
/// produce. For now, `write_dirty_fragment` exercises the file-write atomic
/// semantics and manifest plumbing; queries against L1 fragments will be
/// no-ops until both this shim and Task 5.4 land.
pub fn parse_single_file_to_fragment(rel_path: &str, content: &[u8]) -> std::io::Result<Vec<u8>> {
    let _ = (rel_path, content);
    Ok(vec![])
}

// ----- helpers -----

/// Return the top-`k` names from `names` whose Levenshtein distance to
/// `query` is ≤ 3, sorted ascending by distance.
fn fuzzy_top_k<'a>(names: &[&'a str], query: &str, k: usize) -> Vec<&'a str> {
    let mut scored: Vec<(usize, &str)> = names
        .iter()
        .filter_map(|n| {
            let d = levenshtein(query, n);
            (d <= 3).then_some((d, *n))
        })
        .collect();
    scored.sort_by_key(|(d, _)| *d);
    scored.into_iter().take(k).map(|(_, n)| n).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}
