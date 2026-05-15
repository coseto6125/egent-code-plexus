//! `gnx scan <file>` — file-level hallucination check.
//!
//! Parses a source file via `graph_nexus_analyzer::identifier_finder`,
//! extracts all unique identifier references, then checks each against the
//! graph's symbol set. Unresolved references are listed with top-3
//! Levenshtein "did you mean?" suggestions.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;
use serde_json::{json, Value};

#[derive(Args, Debug, Clone)]
pub struct ScanArgs {
    /// File path to scan for symbol references
    pub file: String,

    /// Also flag identifiers that are common keywords / builtins
    #[arg(long, default_value_t = false)]
    pub strict: bool,

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

    let mut unresolved: Vec<Value> = vec![];
    for (name, line) in &refs {
        if !name_strs.contains(&name.as_str()) {
            let suggestions = fuzzy_top_k(&name_strs, name, 3);
            unresolved.push(json!({
                "name": name,
                "line": line,
                "did_you_mean": suggestions,
            }));
        }
    }

    let payload = if unresolved.is_empty() {
        json!({
            "status": "ok",
            "file": args.file,
            "message": format!("File OK, 0 unresolved references"),
        })
    } else {
        json!({
            "status": "issues",
            "file": args.file,
            "unresolved_count": unresolved.len(),
            "unresolved": unresolved,
        })
    };
    emit(&payload, OutputFormat::Toon)
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
