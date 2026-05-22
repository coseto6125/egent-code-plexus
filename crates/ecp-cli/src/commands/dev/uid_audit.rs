//! `ecp dev uid-audit` — cluster-collapsed view of `uid-collision` BlindSpot
//! records.
//!
//! **DEV-ONLY. NOT AN LLM SIGNAL.** This surface exists for ecp parser
//! maintainers tracking residual uid hash collisions after parser changes.
//! End-user / agent LLM consumption belongs in `ecp summary`.
//!
//! A single parser gap (e.g. missing `owner_class` on Go struct fields named
//! `File`) can fire thousands of distinct `BlindSpotRecord`s. The raw count
//! `uid-collision: N` hides the fact that those N records collapse into
//! 20-40 cluster identities. This command exposes the cluster view —
//! ranked by cluster size — so a parser developer can prioritise root-cause
//! fixes by impact rather than chasing one record at a time.
//!
//! Each cluster key is `(lang, second_kind, second_owner, second_name)`,
//! parsed from the BlindSpot's `hint` field (format
//! `"{bs_kind}: first={k}:{p}:{o}:{n} second={k}:{p}:{o}:{n}"`).

use crate::commit_lookup::find_latest_by_mtime;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::registry::{resolve_home_ecp, Registry};
use ecp_core::EcpError;
use memmap2::Mmap;
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;

/// `((lang, second_kind, owner_class, name), (count, sample_path))`.
/// Cluster key is the four-tuple; value carries cluster size and a sample
/// concrete path so the operator can jump to one occurrence.
type ClusterRow = ((String, String, String, String), (u32, String));

#[derive(Args, Debug, Clone)]
pub struct UidAuditArgs {
    /// Repository selector (path | name | dir_name). Defaults to cwd-resolved
    /// repo. Picks the most-recently-built `graph.bin` under
    /// `~/.ecp/<dir_name>/commits/`. For an arbitrary snapshot, pass
    /// `--graph <path>` at the top level (the global flag).
    #[arg(long)]
    pub repo: Option<String>,

    /// Maximum number of clusters to show (sorted by cluster size desc).
    #[arg(long, default_value_t = 40)]
    pub top: usize,

    /// Filter by `second_kind` (e.g. `Variable`, `Function`, `Method`).
    #[arg(long)]
    pub kind: Option<String>,

    /// Filter by derived language (e.g. `Python`, `Go`, `Java`).
    #[arg(long)]
    pub lang: Option<String>,

    /// Output format. Default `text` (table). `json` and `toon` available
    /// for downstream tooling.
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: UidAuditArgs, cli_graph: &std::path::Path) -> Result<(), EcpError> {
    let graph_path = resolve_graph_path(&args, cli_graph)?;
    let f = File::open(&graph_path)
        .map_err(|e| EcpError::InvalidArgument(format!("open {}: {e}", graph_path.display())))?;
    let mmap = unsafe {
        Mmap::map(&f)
            .map_err(|e| EcpError::InvalidArgument(format!("mmap {}: {e}", graph_path.display())))?
    };
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&mmap)
        .map_err(|e| EcpError::InvalidArgument(format!("rkyv access: {e}")))?;

    let report = build_report(graph, &args);

    // Warning header to stderr — fires regardless of format so the dev-only
    // nature is loud whether the caller is piping JSON or reading text.
    print_warning_header();

    let format = OutputFormat::parse(args.format.as_deref());
    match format {
        OutputFormat::Text => print_text_body(&report, &graph_path),
        _ => emit(
            &serde_json::to_value(&report).unwrap_or(Value::Null),
            format,
        )?,
    }
    Ok(())
}

/// Resolve which `graph.bin` to audit. Order of precedence:
///
/// 1. If the global `--graph <p>` was overridden from its default sentinel
///    `.ecp/graph.bin` → use that path verbatim (ad-hoc snapshot audit).
/// 2. If `--repo <sel>` is set → resolve via registry and pick the
///    most-recently-built commit dir's `graph.bin`.
/// 3. Otherwise → registry-resolve the cwd via the same path.
///
/// The `--graph` default sentinel is recognised by string match against
/// `LEGACY_DEFAULT` (mirrors `graph_path::resolve`); any other value counts
/// as an explicit user override.
fn resolve_graph_path(
    args: &UidAuditArgs,
    cli_graph: &std::path::Path,
) -> Result<PathBuf, EcpError> {
    const LEGACY_DEFAULT: &str = ".ecp/graph.bin";
    if cli_graph.as_os_str() != LEGACY_DEFAULT {
        return Ok(cli_graph.to_path_buf());
    }
    let home_ecp = resolve_home_ecp();
    let registry = Registry::open(&home_ecp)
        .map_err(|e| EcpError::InvalidArgument(format!("registry open: {e}")))?;
    let reg = registry.snapshot();
    let cwd = std::env::current_dir().unwrap_or_default();
    let sel = args.repo.as_deref().unwrap_or(".");
    let selector =
        crate::repo_selector::parse(sel).map_err(|e| EcpError::Output(format!("selector: {e}")))?;
    let cwd_str = cwd.to_string_lossy();
    let resolved =
        crate::repo_selector::resolve_top_level(&selector, reg, &cwd_str, "dev uid-audit")
            .map_err(|e| EcpError::Output(format!("selector: {e}")))?;
    let r = resolved
        .first()
        .ok_or_else(|| EcpError::InvalidArgument("no repo resolved from selector".into()))?;
    let commits_dir = home_ecp.join(&r.dir_name).join("commits");
    find_latest_by_mtime(&commits_dir)
        .map(|d| d.join("graph.bin"))
        .ok_or_else(|| {
            EcpError::InvalidArgument(format!(
                "no graph.bin under {} — run `ecp admin index` first",
                commits_dir.display()
            ))
        })
}

/// Map a file extension to its display language. Mirrors the dispatch in
/// `crates/ecp-analyzer/src/pipeline.rs` so cluster labels stay consistent
/// with the rest of ecp.
fn lang_from_path(p: &str) -> &'static str {
    let ext = p.rsplit('.').next().unwrap_or("");
    match ext {
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "cs" => "CSharp",
        "go" => "Go",
        "rs" => "Rust",
        "php" => "PHP",
        "rb" => "Ruby",
        "swift" => "Swift",
        "c" => "C",
        "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => "C++",
        "dart" => "Dart",
        "sh" | "bash" => "Bash",
        "lua" | "luau" => "Lua",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "yml" | "yaml" => "YAML",
        _ => "?",
    }
}

/// Parse `BlindSpotRecord.hint` of shape
/// `"<bs_kind>: first=K:P:O:N second=K:P:O:N"` and return the four `second=`
/// fields. Returns `None` if the format is unexpected — callers count these
/// as "unparsed" and surface the figure so silent parser drift is visible.
///
/// The hint's analyzer-side emitter (`resolution::builder.rs::format!(...)`)
/// joins four `:`-separated fields. Three of them (`K`/`P`/`N`) are normally
/// `:`-free, but `O` (`owner_class`) carries Rust path syntax — `crate::mod::Type`
/// — so a naive `splitn(4, ':')` mis-attributes part of the owner into the
/// `N` field. Concrete drift caught on Rust corpus: clusters reported
/// `owner='sealed'` + `name=':FromStreamPriv<T>:InternalCollection'` when the
/// true split is `owner='sealed::FromStreamPriv<T>'` + `name='InternalCollection'`.
///
/// Strategy:
///   1. `splitn(3, ':')` to peel `kind` and `path` from the front (these are
///      `:`-free in every parser).
///   2. `rsplit_once(':')` on the remainder to peel `name` off the right
///      (names are `:`-free in every parser the uid-collision path touches).
///   3. Whatever's left in between is `owner_class`, `::` preserved verbatim.
///
/// Limitation: if a parser ever emits a name containing `:` (e.g. Swift
/// selector form `init(foo:bar:)`), the rsplit picks up that trailing `:`
/// and mis-attributes. uid-collision records aren't issued for Swift
/// selectors today, so this is theoretical — but worth knowing.
fn parse_hint(hint: &str) -> Option<(&str, &str, &str, &str)> {
    let second = hint.split(" second=").nth(1)?;
    let mut head = second.splitn(3, ':');
    let kind = head.next()?;
    let path = head.next()?;
    let rest = head.next()?; // owner_class + ':' + name
    let (owner, name) = rest.rsplit_once(':')?;
    Some((kind, path, owner, name))
}

#[derive(serde::Serialize)]
struct Report {
    /// Total `uid-collision` records scanned (pre-filter).
    total: u32,
    /// Records whose `hint` couldn't be parsed into the
    /// `first=…/second=…` shape — silent parser drift if non-zero.
    hint_unparsed: u32,
    /// Distinct `(lang, second_kind, owner, name)` cluster identities
    /// after filters applied.
    distinct_clusters: usize,
    /// Top-N clusters by size (descending).
    top: Vec<Cluster>,
    /// Fraction (0..1) of total records covered by `top`.
    top_coverage: f64,
}

#[derive(serde::Serialize)]
struct Cluster {
    count: u32,
    lang: String,
    second_kind: String,
    owner_class: String,
    name: String,
    sample_path: String,
}

fn build_report(graph: &ArchivedZeroCopyGraph, args: &UidAuditArgs) -> Report {
    let mut clusters: HashMap<(String, String, String, String), (u32, String)> = HashMap::new();
    let mut total_uid_collision: u32 = 0;
    let mut total_hint_unparsed: u32 = 0;

    for bs in graph.blind_spots.iter() {
        let kind = bs.kind.resolve(&graph.string_pool);
        if kind != "uid-collision" {
            continue;
        }
        total_uid_collision += 1;
        let hint = bs.hint.resolve(&graph.string_pool);
        let Some((second_kind, second_path, second_owner, second_name)) = parse_hint(hint) else {
            total_hint_unparsed += 1;
            continue;
        };

        // Apply filters AFTER parsing — that way the "unparsed" count is
        // honest even when filters are narrow.
        if let Some(want_kind) = args.kind.as_deref() {
            if second_kind != want_kind {
                continue;
            }
        }
        let lang = lang_from_path(second_path);
        if let Some(want_lang) = args.lang.as_deref() {
            if !lang.eq_ignore_ascii_case(want_lang) {
                continue;
            }
        }

        let key = (
            lang.to_string(),
            second_kind.to_string(),
            second_owner.to_string(),
            second_name.to_string(),
        );
        clusters
            .entry(key)
            .and_modify(|(c, _)| *c += 1)
            .or_insert((1, second_path.to_string()));
    }

    let distinct = clusters.len();
    let mut rows: Vec<ClusterRow> = clusters.into_iter().collect();
    rows.sort_by_key(|r| Reverse(r.1 .0));

    let top: Vec<Cluster> = rows
        .iter()
        .take(args.top)
        .map(|((lang, kind, owner, name), (count, sample))| Cluster {
            count: *count,
            lang: lang.clone(),
            second_kind: kind.clone(),
            owner_class: owner.clone(),
            name: name.clone(),
            sample_path: sample.clone(),
        })
        .collect();

    let covered: u32 = top.iter().map(|c| c.count).sum();
    let top_coverage = if total_uid_collision > 0 {
        covered as f64 / total_uid_collision as f64
    } else {
        0.0
    };

    Report {
        total: total_uid_collision,
        hint_unparsed: total_hint_unparsed,
        distinct_clusters: distinct,
        top,
        top_coverage,
    }
}

/// Warning banner — keeps the dev-only nature loud. Emitted to stderr so
/// it shows up in interactive use without polluting piped JSON / TOON
/// stdout. The same banner fires for every format.
fn print_warning_header() {
    eprintln!("┌─ ecp dev uid-audit ─────────────────────────────────────────┐");
    eprintln!("│ DEV-ONLY · NOT an LLM signal · for ecp parser maintainers   │");
    eprintln!("│ For source-code opacity / LLM-actionable blind spots, run:  │");
    eprintln!("│   ecp summary --repo .                                      │");
    eprintln!("└─────────────────────────────────────────────────────────────┘");
}

fn print_text_body(report: &Report, graph_path: &std::path::Path) {
    println!("graph                        : {}", graph_path.display());
    println!("total uid-collision records  : {}", report.total);
    println!(
        "distinct (lang,kind,own,name): {}",
        report.distinct_clusters
    );
    println!("hint parse failures          : {}", report.hint_unparsed);
    println!();
    println!("count lang         kind           owner_class                  name                         sample_path");
    println!("{}", "-".repeat(120));
    for c in &report.top {
        let owner_disp = if c.owner_class.is_empty() {
            "(none)"
        } else {
            c.owner_class.as_str()
        };
        let sample_short = if c.sample_path.len() > 50 {
            format!("...{}", &c.sample_path[c.sample_path.len() - 47..])
        } else {
            c.sample_path.clone()
        };
        println!(
            "{:>5} {:<12} {:<14} {:<28} {:<28} {}",
            c.count, c.lang, c.second_kind, owner_disp, c.name, sample_short
        );
    }
    println!();
    println!(
        "top {} clusters cover {} / {} ({:.1}%)",
        report.top.len(),
        report.top.iter().map(|c| c.count).sum::<u32>(),
        report.total,
        100.0 * report.top_coverage
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hint_extracts_second_fields() {
        let hint =
            "uid-collision: first=Variable:src/a.py:Outer:File second=Variable:src/b.py:Inner:File";
        let got = parse_hint(hint).expect("hint must parse");
        assert_eq!(got, ("Variable", "src/b.py", "Inner", "File"));
    }

    #[test]
    fn parse_hint_missing_second_returns_none() {
        assert!(parse_hint("uid-collision: first=Variable:src/a.py:Outer:File").is_none());
    }

    #[test]
    fn parse_hint_empty_owner_is_kept_as_empty() {
        let hint = "uid-collision: first=Variable:src/a.py::File second=Function:src/b.go::main";
        let got = parse_hint(hint).expect("hint must parse");
        assert_eq!(got.2, ""); // owner_class can be empty (top-level)
        assert_eq!(got.3, "main");
    }

    /// Regression: a naive `splitn(4, ':')` mis-attributes `::` inside Rust
    /// `owner_class`, surfacing in real `dev uid-audit` output as
    /// `owner='sealed'` + `name=':FromStreamPriv<T>:InternalCollection'`
    /// (one cluster per affected file → false fan-out, blocking root-cause
    /// triage). The current `splitn(3) + rsplit_once` recovers the true split.
    #[test]
    fn parse_hint_rust_owner_with_double_colon_preserves_owner() {
        let hint = "uid-collision: \
            first=Typedef:tokio-stream/src/stream_ext/collect.rs:sealed::FromStreamPriv<T>:InternalCollection \
            second=Typedef:tokio-stream/src/stream_ext/collect.rs:sealed::FromStreamPriv<T>:InternalCollection";
        let (kind, path, owner, name) = parse_hint(hint).expect("hint must parse");
        assert_eq!(kind, "Typedef");
        assert_eq!(path, "tokio-stream/src/stream_ext/collect.rs");
        assert_eq!(owner, "sealed::FromStreamPriv<T>");
        assert_eq!(name, "InternalCollection");
    }

    /// Deeply-nested Rust / C++ owner path with multiple `::` separators must
    /// also survive — `crate::a::b::c::Type::Inner` → owner stays whole.
    #[test]
    fn parse_hint_deep_owner_chain_preserved() {
        let hint = "uid-collision: first=Method:src/a.rs::nop second=Method:src/x.rs:crate::a::b::c::Outer:inner";
        let (_, _, owner, name) = parse_hint(hint).expect("hint must parse");
        assert_eq!(owner, "crate::a::b::c::Outer");
        assert_eq!(name, "inner");
    }

    /// Exhaustive coverage: every match arm in `lang_from_path` must have at
    /// least one extension exercised here. Drift surfaces immediately if a
    /// new ext is added (or an existing one renamed) without test sync.
    #[test]
    fn lang_from_path_covers_every_arm() {
        let cases: &[(&str, &str)] = &[
            // TypeScript
            ("src/x.ts", "TypeScript"),
            ("src/x.tsx", "TypeScript"),
            // JavaScript
            ("src/x.js", "JavaScript"),
            ("src/x.jsx", "JavaScript"),
            ("src/x.mjs", "JavaScript"),
            ("src/x.cjs", "JavaScript"),
            // Python / Java / Kotlin
            ("src/x.py", "Python"),
            ("src/x.java", "Java"),
            ("src/x.kt", "Kotlin"),
            ("src/x.kts", "Kotlin"),
            // C# / Go / Rust
            ("src/x.cs", "CSharp"),
            ("src/x.go", "Go"),
            ("src/x.rs", "Rust"),
            // PHP / Ruby / Swift
            ("src/x.php", "PHP"),
            ("src/x.rb", "Ruby"),
            ("src/x.swift", "Swift"),
            // C family (each `.h*` / `.c*` variant)
            ("src/x.c", "C"),
            ("src/x.h", "C++"),
            ("src/x.cc", "C++"),
            ("src/x.cpp", "C++"),
            ("src/x.cxx", "C++"),
            ("src/x.hpp", "C++"),
            ("src/x.hxx", "C++"),
            ("src/x.hh", "C++"),
            // Dart / Shell / Lua / Vue / Svelte / YAML
            ("src/x.dart", "Dart"),
            ("scripts/x.sh", "Bash"),
            ("scripts/x.bash", "Bash"),
            ("scripts/x.lua", "Lua"),
            ("scripts/x.luau", "Lua"),
            ("ui/X.vue", "Vue"),
            ("ui/X.svelte", "Svelte"),
            ("ci/x.yml", "YAML"),
            ("ci/x.yaml", "YAML"),
            // Unknown extension + no extension
            ("README.md", "?"),
            ("Makefile", "?"),
            ("noext", "?"),
        ];
        for (path, want) in cases {
            assert_eq!(
                lang_from_path(path),
                *want,
                "lang_from_path mismatch for {path:?}"
            );
        }
    }
}
