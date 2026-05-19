//! `gnx verify-resolver` — diff our resolver dump against a language oracle.
//!
//! Spec: `docs/specs/2026-05-15-resolver-oracle-harness.md`.
//!
//! Reads two JSONL files (oracle output + gnx `--dump-resolver` output),
//! joins on `(src_file, name)`, classifies each match into TP / FP_ghost /
//! FP_overmatch / FN_dangling / tier_demoted (plus side-only buckets), and
//! prints a markdown report. Exit code is always 0 — this is a benchmark,
//! not a CI gate.

use clap::Args;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct VerifyResolverArgs {
    /// Path to the oracle JSONL file (one record per imported binding).
    #[arg(long)]
    pub oracle: PathBuf,

    /// Path to the gnx resolver dump JSONL (produced by
    /// `gnx analyze --dump-resolver`).
    #[arg(long)]
    pub gnx: PathBuf,

    /// Language selector — controls extension-equivalence rules for
    /// comparing `target_file`. Currently `ts`, `py`, or `rs`.
    #[arg(long)]
    pub lang: String,

    /// Optional markdown report output path. If omitted, the report is
    /// written to stdout.
    #[arg(long)]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct Record {
    src_file: String,
    name: String,
    specifier: Option<String>,
    /// `tier` is stored as a string here (not the analyzer's `DecisionTier`
    /// enum) because the JSONL is also produced by external oracle scripts
    /// that emit other values like `"External"` (Rust oracle). Keeping the
    /// reader tolerant lets one harness compare across producers.
    tier: String,
    target_file: Option<String>,
    alt_count: u32,
    // confidence is deliberately not read — diff logic doesn't use it,
    // and ignoring it lets us tolerate producers that omit the field.
}

#[derive(Debug, Default)]
struct Counts {
    tp: u32,
    fp_ghost: u32,
    fp_overmatch: u32,
    fn_dangling: u32,
    tier_demoted: u32,
    oracle_only: u32,
    gnx_only_same_file: u32,
    gnx_only_other: u32,
}

/// Result of parsing a JSONL dump file. Bad lines are surfaced as a count
/// rather than silently dropped — see `render_report` which prints
/// `bad_lines` in the summary so users notice silent producer drift.
struct ParsedJsonl {
    records: Vec<Record>,
    bad_lines: u32,
}

pub fn run(args: VerifyResolverArgs) -> Result<(), graph_nexus_core::GnxError> {
    let oracle = read_jsonl(&args.oracle)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("read oracle: {e}")))?;
    let gnx = read_jsonl(&args.gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("read gnx dump: {e}")))?;

    let normalize = pick_normalize(&args.lang);
    let (counts, worst, per_tier) = diff(&oracle.records, &gnx.records, normalize);
    let report = render_report(
        &args.lang,
        &counts,
        &worst,
        &per_tier,
        oracle.records.len(),
        gnx.records.len(),
        oracle.bad_lines,
        gnx.bad_lines,
    );

    match args.report.as_deref() {
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        graph_nexus_core::GnxError::InvalidArgument(format!(
                            "mkdir report parent: {e}"
                        ))
                    })?;
                }
            }
            std::fs::write(p, &report).map_err(|e| {
                graph_nexus_core::GnxError::InvalidArgument(format!("write report: {e}"))
            })?;
            eprintln!("verify-resolver: report written to {}", p.display());
        }
        None => print!("{report}"),
    }
    Ok(())
}

fn read_jsonl(path: &std::path::Path) -> std::io::Result<ParsedJsonl> {
    use std::io::BufRead;
    let f = std::fs::File::open(path)?;
    let r = std::io::BufReader::new(f);
    let mut records = Vec::new();
    let mut bad_lines = 0u32;
    for (lineno, line) in r.lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(rec) => records.push(rec),
            Err(e) => {
                bad_lines += 1;
                tracing::warn!("{}:{}: invalid JSON: {}", path.display(), lineno + 1, e);
            }
        }
    }
    Ok(ParsedJsonl { records, bad_lines })
}

/// Normalize a target file path for extension-equivalence comparison.
/// `a/b.ts` ≡ `a/b.tsx` ≡ `a/b/index.ts`.
type Normalizer = fn(&str) -> String;

fn pick_normalize(lang: &str) -> Normalizer {
    match lang {
        "ts" | "js" => normalize_ts,
        "py" => normalize_py,
        "rs" => normalize_rs,
        _ => normalize_passthrough,
    }
}

fn normalize_ts(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = s.trim_start_matches("./").to_string();
    // Order matters: longer suffixes first so `foo.d.ts` strips to `foo`, not `foo.d`.
    let s = strip_one_of(&s, &[".d.ts", ".tsx", ".jsx", ".mjs", ".cjs", ".ts", ".js"]);
    strip_one_of(&s, &["/index", "/main"])
}

fn normalize_py(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = strip_one_of(&s, &[".pyi", ".py"]);
    strip_one_of(&s, &["/__init__"])
}

fn normalize_rs(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = strip_one_of(&s, &[".rs"]);
    strip_one_of(&s, &["/mod", "/lib", "/main"])
}

fn normalize_passthrough(s: &str) -> String {
    s.replace('\\', "/")
}

fn strip_one_of(s: &str, suffixes: &[&str]) -> String {
    for suf in suffixes {
        if let Some(rest) = s.strip_suffix(suf) {
            return rest.to_string();
        }
    }
    s.to_string()
}

#[derive(Debug, Clone)]
struct WorstOffender {
    src_file: String,
    name: String,
    class: &'static str,
    detail: String,
}

fn diff(
    oracle: &[Record],
    gnx: &[Record],
    normalize: Normalizer,
) -> (Counts, Vec<WorstOffender>, FxHashMap<String, Counts>) {
    // Index gnx by (src_file, name) — there may be multiple gnx attempts per
    // key (e.g. heritage + type annotation + call). Pick the BEST attempt:
    //   resolved (tier != Unresolved) > unresolved
    //   SameFile/ImportScoped > Global > AmbiguousGlobal > Unresolved
    // AmbiguousGlobal ranks above Unresolved because it carries diagnostic
    // signal (candidates were found, just suppressed) — preferring it on
    // dedup surfaces "defence fired" over the silent "nothing found".
    let mut gnx_by_key: FxHashMap<(String, String), &Record> = FxHashMap::default();
    let tier_rank = |t: &str| -> u8 {
        match t {
            "SameFile" => 0,
            "ImportScoped" => 1,
            "Global" => 2,
            "AmbiguousGlobal" => 3,
            _ => 4,
        }
    };
    for r in gnx {
        let key = (r.src_file.clone(), r.name.clone());
        match gnx_by_key.get(&key) {
            Some(existing) if tier_rank(&existing.tier) <= tier_rank(&r.tier) => {}
            _ => {
                gnx_by_key.insert(key, r);
            }
        }
    }

    let oracle_keys: FxHashSet<_> = oracle
        .iter()
        .map(|r| (r.src_file.clone(), r.name.clone()))
        .collect();

    let mut counts = Counts::default();
    let mut per_tier: FxHashMap<String, Counts> = FxHashMap::default();
    let mut offenders: Vec<WorstOffender> = Vec::new();

    for o in oracle {
        let key = (o.src_file.clone(), o.name.clone());
        let bucket = per_tier.entry("oracle".into()).or_default();
        let _ = bucket; // touch to keep map populated even if no per-tier hits below

        match gnx_by_key.get(&key) {
            None => {
                counts.oracle_only += 1;
                if o.target_file.is_some() {
                    push_offender(&mut offenders, o, "oracle_only_resolved", "");
                }
            }
            Some(g) => {
                let entry = per_tier.entry(g.tier.clone()).or_default();
                match (o.target_file.as_deref(), g.target_file.as_deref()) {
                    (Some(ot), Some(gt)) => {
                        if normalize(ot) == normalize(gt) {
                            counts.tp += 1;
                            entry.tp += 1;
                            // tier_demoted: oracle says resolved, gnx fell back to Global
                            if g.tier == "Global" {
                                counts.tier_demoted += 1;
                                entry.tier_demoted += 1;
                                push_offender(
                                    &mut offenders,
                                    o,
                                    "tier_demoted",
                                    &format!("gnx=Global oracle→{}", normalize(ot)),
                                );
                            }
                            if g.alt_count > 0 {
                                counts.fp_overmatch += 1;
                                entry.fp_overmatch += 1;
                                push_offender(
                                    &mut offenders,
                                    o,
                                    "fp_overmatch",
                                    &format!("alt_count={}", g.alt_count),
                                );
                            }
                        } else {
                            counts.fp_ghost += 1;
                            entry.fp_ghost += 1;
                            push_offender(
                                &mut offenders,
                                o,
                                "fp_ghost",
                                &format!("gnx→{} oracle→{}", normalize(gt), normalize(ot)),
                            );
                        }
                    }
                    (Some(_), None) => {
                        counts.fn_dangling += 1;
                        entry.fn_dangling += 1;
                        push_offender(
                            &mut offenders,
                            o,
                            "fn_dangling",
                            &format!(
                                "oracle→{} specifier={}",
                                o.target_file.as_deref().unwrap_or("?"),
                                o.specifier.as_deref().unwrap_or("?"),
                            ),
                        );
                    }
                    (None, Some(_)) => {
                        // Oracle said unresolved but gnx connected → conservative FP_ghost
                        counts.fp_ghost += 1;
                        entry.fp_ghost += 1;
                    }
                    (None, None) => {
                        // Both unresolved — no defect, not counted.
                    }
                }
            }
        }
    }

    // gnx_only: gnx decisions for keys oracle never produced (often Tier 1 same-file)
    for g in gnx {
        let key = (g.src_file.clone(), g.name.clone());
        if !oracle_keys.contains(&key) {
            if g.tier == "SameFile" {
                counts.gnx_only_same_file += 1;
            } else {
                counts.gnx_only_other += 1;
            }
        }
    }

    // Cap offenders at 20 worst — they're already in oracle-order so sample.
    offenders.truncate(20);
    (counts, offenders, per_tier)
}

fn push_offender(buf: &mut Vec<WorstOffender>, o: &Record, class: &'static str, detail: &str) {
    if buf.len() >= 20 {
        return;
    }
    buf.push(WorstOffender {
        src_file: o.src_file.clone(),
        name: o.name.clone(),
        class,
        detail: detail.to_string(),
    });
}

#[allow(clippy::too_many_arguments)]
fn render_report(
    lang: &str,
    counts: &Counts,
    worst: &[WorstOffender],
    per_tier: &FxHashMap<String, Counts>,
    oracle_total: usize,
    gnx_total: usize,
    oracle_bad: u32,
    gnx_bad: u32,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# verify-resolver report ({lang})\n\n"));
    s.push_str(&format!(
        "Oracle records: {oracle_total} (bad lines: {oracle_bad})\nGnx records: {gnx_total} (bad lines: {gnx_bad})\n\n"
    ));
    if oracle_bad > 0 || gnx_bad > 0 {
        s.push_str(
            "> ⚠ Some input lines failed JSON parse — totals above are post-skip. \
                    Check the warn-level logs for line numbers.\n\n",
        );
    }
    s.push_str("## Summary\n\n");
    s.push_str("| class | count |\n|---|---|\n");
    s.push_str(&format!("| TP (correct) | {} |\n", counts.tp));
    s.push_str(&format!(
        "| FP_ghost (wrong target) | {} |\n",
        counts.fp_ghost
    ));
    s.push_str(&format!(
        "| FP_overmatch (Global with alts) | {} |\n",
        counts.fp_overmatch
    ));
    s.push_str(&format!(
        "| FN_dangling (oracle resolved, gnx didn't) | {} |\n",
        counts.fn_dangling
    ));
    s.push_str(&format!(
        "| tier_demoted (correct but fell back to Global) | {} |\n",
        counts.tier_demoted
    ));
    s.push_str(&format!(
        "| oracle_only (oracle imports gnx never saw) | {} |\n",
        counts.oracle_only
    ));
    s.push_str(&format!(
        "| gnx_only same-file (excluded from diff) | {} |\n",
        counts.gnx_only_same_file
    ));
    s.push_str(&format!(
        "| gnx_only other (no oracle counterpart) | {} |\n",
        counts.gnx_only_other
    ));
    s.push('\n');

    s.push_str("## Per-tier breakdown (gnx side)\n\n");
    s.push_str("| tier | TP | FP_ghost | FP_overmatch | tier_demoted | FN_dangling |\n");
    s.push_str("|---|---|---|---|---|---|\n");
    let mut tiers: Vec<&String> = per_tier.keys().collect();
    tiers.sort();
    for t in tiers {
        let c = per_tier.get(t).unwrap();
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            t, c.tp, c.fp_ghost, c.fp_overmatch, c.tier_demoted, c.fn_dangling
        ));
    }
    s.push('\n');

    s.push_str("## Top-20 worst offenders\n\n");
    if worst.is_empty() {
        s.push_str("(none)\n");
    } else {
        s.push_str("| src_file | name | class | detail |\n|---|---|---|---|\n");
        for w in worst {
            s.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                w.src_file, w.name, w.class, w.detail
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(
        src: &str,
        name: &str,
        spec: Option<&str>,
        tier: &str,
        tgt: Option<&str>,
        alt: u32,
    ) -> Record {
        Record {
            src_file: src.into(),
            name: name.into(),
            specifier: spec.map(String::from),
            tier: tier.into(),
            target_file: tgt.map(String::from),
            alt_count: alt,
        }
    }

    #[test]
    fn ts_extension_equivalence_treats_index_and_tsx_as_same() {
        assert_eq!(normalize_ts("a/b.tsx"), normalize_ts("a/b.ts"));
        assert_eq!(normalize_ts("a/b/index.ts"), normalize_ts("a/b.ts"));
        assert_eq!(normalize_ts("./a.ts"), normalize_ts("a.ts"));
    }

    #[test]
    fn py_extension_equivalence_treats_init_as_package() {
        assert_eq!(normalize_py("foo/__init__.py"), normalize_py("foo.py"));
        assert_eq!(normalize_py("foo.pyi"), normalize_py("foo.py"));
    }

    #[test]
    fn rs_extension_equivalence_treats_mod_and_lib_as_same() {
        assert_eq!(normalize_rs("a/mod.rs"), normalize_rs("a.rs"));
        assert_eq!(normalize_rs("a/lib.rs"), normalize_rs("a.rs"));
    }

    #[test]
    fn diff_classifies_tp_when_target_files_match_under_normalization() {
        let oracle = vec![mk(
            "s.ts",
            "X",
            Some("./y"),
            "ImportScoped",
            Some("y.ts"),
            0,
        )];
        let gnx = vec![mk(
            "s.ts",
            "X",
            Some("./y"),
            "ImportScoped",
            Some("y/index.ts"),
            0,
        )];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.tp, 1);
        assert_eq!(c.fp_ghost, 0);
        assert_eq!(c.fn_dangling, 0);
    }

    #[test]
    fn diff_classifies_fp_ghost_when_target_files_differ() {
        let oracle = vec![mk(
            "s.ts",
            "X",
            Some("@/y"),
            "ImportScoped",
            Some("y.ts"),
            0,
        )];
        let gnx = vec![mk(
            "s.ts",
            "X",
            Some("@/y"),
            "Global",
            Some("wrong/y.ts"),
            0,
        )];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.fp_ghost, 1);
        assert_eq!(c.tp, 0);
    }

    #[test]
    fn diff_classifies_fp_overmatch_when_global_has_alternatives() {
        let oracle = vec![mk(
            "s.ts",
            "X",
            Some("@/y"),
            "ImportScoped",
            Some("y.ts"),
            0,
        )];
        let gnx = vec![mk(
            "s.ts",
            "X",
            Some("@/y"),
            "Global",
            Some("y.ts"),
            3, // 3 alternative candidates also got edges
        )];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.tp, 1);
        assert_eq!(c.fp_overmatch, 1);
        assert_eq!(c.tier_demoted, 1, "Global + oracle-resolved = tier_demoted");
    }

    #[test]
    fn diff_classifies_fn_dangling_when_gnx_unresolved() {
        let oracle = vec![mk(
            "s.ts",
            "X",
            Some("@/y"),
            "ImportScoped",
            Some("y.ts"),
            0,
        )];
        let gnx = vec![mk("s.ts", "X", Some("@/y"), "Unresolved", None, 0)];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.fn_dangling, 1);
        assert_eq!(c.tp, 0);
    }

    #[test]
    fn diff_prefers_best_gnx_attempt_per_key() {
        let oracle = vec![mk(
            "s.ts",
            "X",
            Some("@/y"),
            "ImportScoped",
            Some("y.ts"),
            0,
        )];
        // gnx records two attempts for same (src, name): Unresolved then Global → Global wins
        let gnx = vec![
            mk("s.ts", "X", None, "Unresolved", None, 0),
            mk("s.ts", "X", Some("@/y"), "Global", Some("y.ts"), 0),
        ];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.tp, 1, "should pick the Global resolution, not Unresolved");
    }

    #[test]
    fn ts_extension_equivalence_strips_d_ts_before_ts() {
        // `foo.d.ts` and `foo.ts` should normalize identically. Caught a
        // bug where `.ts` was listed before `.d.ts` and stripped first,
        // leaving `foo.d` ≠ `foo`.
        assert_eq!(normalize_ts("a/b.d.ts"), normalize_ts("a/b.ts"));
    }

    #[test]
    fn diff_counts_oracle_only_when_gnx_never_saw_the_key() {
        // Oracle has a binding gnx never resolved (e.g. import in a file
        // with no callsite). Should land in `oracle_only` and not affect
        // any other counter.
        let oracle = vec![mk(
            "s.ts",
            "X",
            Some("./y"),
            "ImportScoped",
            Some("y.ts"),
            0,
        )];
        let gnx: Vec<Record> = vec![];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.oracle_only, 1);
        assert_eq!(c.tp, 0);
        assert_eq!(c.fp_ghost, 0);
        assert_eq!(c.fn_dangling, 0);
    }

    #[test]
    fn diff_treats_oracle_unresolved_plus_gnx_connected_as_fp_ghost() {
        // Oracle says the import didn't resolve; gnx still produced an
        // edge (likely Tier-3 same-name match). Conservative: count as
        // ghost since gnx is connecting things tsc couldn't.
        let oracle = vec![mk("s.ts", "X", Some("@/y"), "Unresolved", None, 0)];
        let gnx = vec![mk(
            "s.ts",
            "X",
            Some("@/y"),
            "Global",
            Some("somewhere.ts"),
            0,
        )];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.fp_ghost, 1);
        assert_eq!(c.tp, 0);
    }

    #[test]
    fn diff_ignores_both_unresolved_pairs() {
        // Neither side resolved — no defect, no signal. Should not bump
        // any of the headline counters.
        let oracle = vec![mk("s.ts", "X", Some("nope"), "Unresolved", None, 0)];
        let gnx = vec![mk("s.ts", "X", Some("nope"), "Unresolved", None, 0)];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.tp, 0);
        assert_eq!(c.fp_ghost, 0);
        assert_eq!(c.fn_dangling, 0);
        assert_eq!(c.oracle_only, 0);
    }

    #[test]
    fn diff_buckets_gnx_only_records_by_tier() {
        // gnx resolved Tier 1 SameFile entries oracle never sees — those
        // should land in `gnx_only_same_file` (excluded from diff). Other
        // tier hits oracle never sees go to `gnx_only_other`.
        let oracle: Vec<Record> = vec![];
        let gnx = vec![
            mk("s.ts", "X", None, "SameFile", Some("s.ts"), 0),
            mk("s.ts", "Y", Some("@/z"), "Global", Some("z.ts"), 0),
        ];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.gnx_only_same_file, 1);
        assert_eq!(c.gnx_only_other, 1);
    }

    #[test]
    fn read_jsonl_skips_malformed_lines_and_counts_them() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("gnx-jsonl-test-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            br#"{"src_file":"a.ts","name":"X","specifier":null,"tier":"SameFile","target_file":null,"alt_count":0}
this is not json
{"src_file":"b.ts","name":"Y","specifier":null,"tier":"Global","target_file":"y.ts","alt_count":1}
"#,
        )
        .unwrap();
        drop(f);
        let parsed = read_jsonl(&path).expect("read_jsonl succeeds even with bad lines");
        let _ = std::fs::remove_file(&path);
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.bad_lines, 1);
    }
}
