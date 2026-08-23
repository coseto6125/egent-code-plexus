//! `ecp pattern` — syntax-pattern search over the files the graph already knows.
//!
//! The graph stores declarations, so a question about statement shape
//! ("which `try` swallows its exception", "which request omits a timeout")
//! has no node to match. This command answers those by running an ast-grep
//! pattern through [`ecp_analyzer::pattern_finder`].
//!
//! The pipeline mirrors `ecp rename`'s first two stages, minus the write:
//!
//! 1. **Plan (graph)** — pick candidate files. Default is every indexed file,
//!    which already excludes ignored paths and unsupported languages.
//!    `--callers-of` narrows to the files holding a symbol's callers, which
//!    is the part a standalone `ast-grep` run cannot express.
//! 2. **Match (AST)** — parse each candidate and collect pattern hits.
//!
//! A pattern is language-specific, so it is compiled once per extension. Three
//! outcomes stay distinct, because collapsing them blames the pattern for a
//! scan that never reached a supported file:
//!
//! - the extension has no pattern support — skip the file
//! - the extension is supported but the pattern does not parse there — skip
//!   the file, and hold the error in case no language accepts the pattern
//! - the pattern compiles — scan the file
//!
//! The run fails only when at least one supported language was tried and every
//! attempt failed; the message then carries the parser's own diagnostic. A scan
//! that met no supported file returns an ordinary empty result.

use crate::commands::symbol_id::{resolve_owner_class, split_fqn_target};
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_analyzer::pattern_finder::{
    compile, lang_for_path, match_source, CompiledPattern, PatternError, PatternLang,
};
use ecp_core::graph::ArchivedRelType;
use ecp_core::EcpError;
use rayon::prelude::*;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

const MAX_PATTERN_WORKERS: usize = 8;

#[derive(Args, Debug, Clone)]
pub struct PatternArgs {
    /// ast-grep pattern, e.g. `requests.get($URL)`. `$NAME` captures one
    /// node, `$$$NAME` captures a run of them.
    #[arg(short = 'p', long)]
    pub pattern: String,

    /// Restrict the scan to files that call this symbol. Accepts the same
    /// `Owner.method` qualification as `ecp rename`.
    #[arg(long)]
    pub callers_of: Option<String>,

    /// Restrict the scan to one file extension, without the dot (`py`,
    /// `ts`). A pattern that fails to compile for it is then an error.
    #[arg(long)]
    pub lang: Option<String>,

    /// Cap on reported matches. 0 means no cap.
    #[arg(long, default_value_t = 200)]
    pub limit: usize,

    #[arg(long)]
    pub repo: Option<String>,

    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: PatternArgs, engine: &Engine) -> Result<(), EcpError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let payload = build_payload(&args, engine)?;
    emit(&payload, format)
}

pub fn build_payload(args: &PatternArgs, engine: &Engine) -> Result<serde_json::Value, EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let repo_root = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    if args.pattern.trim().is_empty() {
        return Err(EcpError::InvalidArgument("--pattern is required".into()));
    }

    // Stage 2 caches one compiled pattern per extension. `None` marks an
    // extension without pattern support, `Some(Err(_))` a supported one the
    // pattern does not parse in.
    type Compiled = Option<Result<(PatternLang, CompiledPattern), PatternError>>;
    let mut compiled: HashMap<String, Compiled> = HashMap::new();

    // `--lang` is an explicit request, so both of its failure modes are user
    // errors worth failing on before any file is read: a language with no
    // pattern support, and a pattern that language rejects. Compile it here
    // rather than leaving it to the per-extension cache — with no file of that
    // language in scope the cache is never populated, and a pattern the
    // language rejects would pass as an empty result having never been parsed.
    let wanted_ext: Option<String> = match args.lang.as_deref() {
        None => None,
        Some(want) => {
            let ext = want.trim_start_matches('.').to_lowercase();
            let Some(lang) = lang_for_path(&format!("x.{ext}")) else {
                return Err(EcpError::InvalidArgument(format!(
                    "--lang '{want}' has no pattern support"
                )));
            };
            let pattern = compile(&args.pattern, &lang).map_err(|e| {
                EcpError::InvalidArgument(format!("pattern does not parse as {ext}: {e}"))
            })?;
            compiled.insert(ext.clone(), Some(Ok((lang, pattern))));
            Some(ext)
        }
    };

    // Stage 1: candidate files.
    let candidates: Vec<&str> = match args.callers_of.as_deref() {
        None => graph
            .files
            .iter()
            .map(|f| f.path.resolve(&graph.string_pool))
            .collect(),
        // An empty set here means no node carried the name: `caller_files`
        // seeds the target's own file before walking callers, so a symbol that
        // exists always yields at least one. Report the miss the way `impact`
        // and `inspect` do rather than returning a silent empty result.
        Some(symbol) => match caller_files(symbol, graph) {
            files if files.is_empty() => {
                return Err(EcpError::InvalidArgument(format!(
                    "no symbol named '{symbol}' in the graph — try `ecp find {symbol} --mode fuzzy`"
                )))
            }
            files => files,
        },
    };

    // Compile once per candidate extension before entering the parallel scan.
    // The compiled language and pattern are both Sync, so every worker can
    // share them without rebuilding parser state per file.
    for rel_path in &candidates {
        let Some((_, ext)) = rel_path.rsplit_once('.') else {
            continue;
        };
        let ext = ext.to_lowercase();
        if wanted_ext.as_ref().is_some_and(|want| &ext != want) {
            continue;
        }
        compiled.entry(ext).or_insert_with_key(|ext| {
            let lang = lang_for_path(&format!("x.{ext}"))?;
            Some(compile(&args.pattern, &lang).map(|pattern| (lang, pattern)))
        });
    }

    // Every supported language rejected the pattern: that is a pattern error.
    // Meeting no supported language at all is an empty result, not an error.
    let attempts: Vec<&Result<_, PatternError>> = compiled.values().flatten().collect();
    if !attempts.is_empty() && attempts.iter().all(|r| r.is_err()) {
        let detail = attempts
            .iter()
            .find_map(|r| r.as_ref().err())
            .expect("all() proved every attempt carries an error");
        return Err(EcpError::InvalidArgument(format!(
            "pattern does not parse: {detail}"
        )));
    }

    let mut matches: Vec<serde_json::Value> = Vec::new();
    let mut scanned_files = 0usize;
    let mut total = 0usize;

    // Start with a small batch so a common pattern can satisfy the report
    // limit without retaining results from every file. Grow sparse scans
    // geometrically to avoid paying the Rayon dispatch cost per small batch.
    // Unlimited output stays bounded because every match must be retained.
    let worker_count = pattern_worker_count(rayon::current_num_threads());
    let worker_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .map_err(|error| {
            std::io::Error::other(format!("failed to build pattern workers: {error}"))
        })?;
    let mut batch_size = worker_count.saturating_mul(2).max(1);
    let mut batch_start = 0usize;
    while batch_start < candidates.len() {
        let batch_end = batch_start.saturating_add(batch_size).min(candidates.len());
        let batch = &candidates[batch_start..batch_end];
        let report_limit = if args.limit == 0 {
            usize::MAX
        } else {
            args.limit - matches.len()
        };
        let scans: Vec<Option<(usize, Vec<serde_json::Value>)>> = worker_pool.install(|| {
            batch
                .par_iter()
                .map(|rel_path| {
                    let ext = rel_path.rsplit_once('.')?.1.to_lowercase();
                    if wanted_ext.as_ref().is_some_and(|want| &ext != want) {
                        return None;
                    }
                    let Some(Ok((lang, pattern))) = compiled.get(&ext)?.as_ref() else {
                        return None;
                    };
                    let bytes = std::fs::read(repo_root.join(rel_path)).ok()?;
                    let hits = match_source(&bytes, lang, pattern);
                    let total = hits.len();
                    let reported = hits
                        .into_iter()
                        .take(report_limit)
                        .map(|hit| {
                            json!({
                                "file": rel_path,
                                "line": hit.row + 1,
                                "col": hit.col + 1,
                                "text": first_line(&bytes[hit.start_byte..hit.end_byte]),
                            })
                        })
                        .collect();
                    Some((total, reported))
                })
                .collect()
        });

        for (file_total, file_matches) in scans.into_iter().flatten() {
            scanned_files += 1;
            total += file_total;
            if args.limit == 0 {
                matches.extend(file_matches);
            } else {
                matches.extend(file_matches.into_iter().take(args.limit - matches.len()));
            }
        }

        batch_start = batch_end;
        batch_size = if args.limit == 0 {
            worker_count.saturating_mul(8).max(1)
        } else if matches.len() == args.limit {
            candidates.len() - batch_start
        } else {
            batch_size.saturating_mul(2)
        };
    }
    let truncated = args.limit > 0 && total > args.limit;

    let languages: Vec<&str> = {
        let mut names: Vec<&str> = compiled
            .iter()
            .filter(|(_, v)| matches!(v, Some(Ok(_))))
            .map(|(k, _)| k.as_str())
            .collect();
        names.sort_unstable();
        names
    };

    Ok(json!({
        "pattern": args.pattern,
        "scanned_files": scanned_files,
        "languages": languages,
        "total": total,
        "truncated": truncated,
        "matches": matches,
    }))
}

fn pattern_worker_count(available: usize) -> usize {
    available.clamp(1, MAX_PATTERN_WORKERS)
}

/// Repo-relative paths of files holding a caller of `symbol`, plus the file
/// declaring it. `Owner.method` qualification comes from the shared
/// `split_fqn_target`, so `Foo.run` and `Bar.run` stay apart and a bare name
/// matches top-level symbols only — an unqualified argument yields
/// `owner_filter == None`, which pairs with an empty `owner_class`.
fn caller_files<'g>(
    symbol: &str,
    graph: &'g ecp_core::graph::ArchivedZeroCopyGraph,
) -> Vec<&'g str> {
    let (owner_filter, bare_name) = split_fqn_target(symbol);
    let targets: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(idx, n)| {
            n.has_owning_file()
                && n.name.resolve(&graph.string_pool) == bare_name
                && resolve_owner_class(graph, *idx) == owner_filter
        })
        .map(|(i, _)| i)
        .collect();

    let mut file_idx: HashSet<usize> = HashSet::new();
    for target in targets {
        file_idx.insert(graph.nodes[target].file_idx.to_native() as usize);
        let start = graph.in_offsets[target].to_native() as usize;
        let end = graph.in_offsets[target + 1].to_native() as usize;
        for i in start..end {
            let edge = &graph.edges[graph.in_edge_idx[i].to_native() as usize];
            if !matches!(edge.rel_type, ArchivedRelType::Calls) {
                continue;
            }
            let src = &graph.nodes[edge.source.to_native() as usize];
            if src.has_owning_file() {
                file_idx.insert(src.file_idx.to_native() as usize);
            }
        }
    }

    let mut file_idx: Vec<usize> = file_idx.into_iter().collect();
    file_idx.sort_unstable();
    file_idx
        .into_iter()
        .map(|i| graph.files[i].path.resolve(&graph.string_pool))
        .collect()
}

/// First line of a match, so a multi-line hit stays one output row.
fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match text.split_once('\n') {
        Some((head, _)) => format!("{}…", head.trim_end()),
        None => text.trim_end().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_worker_count_above_limit_returns_eight() {
        assert_eq!(pattern_worker_count(16), MAX_PATTERN_WORKERS);
    }
}
