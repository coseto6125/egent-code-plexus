//! `gnx verify-resolver` — diff our resolver dump against a language oracle.
//!
//! Spec: `docs/superpowers/specs/2026-05-15-resolver-oracle-harness.md`.
//!
//! Reads two JSONL files (oracle output + gnx `--dump-resolver` output),
//! joins on `(src_file, name)`, classifies each match into TP / FP_ghost /
//! FP_overmatch / FN_dangling / tier_demoted (plus side-only buckets), and
//! prints a markdown report. Exit code is always 0 — this is a benchmark,
//! not a CI gate.

use clap::Args;
use std::collections::HashMap;
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

#[derive(Debug, Clone)]
struct Record {
    src_file: String,
    name: String,
    specifier: Option<String>,
    tier: String,
    target_file: Option<String>,
    alt_count: u32,
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

pub fn run(args: VerifyResolverArgs) -> Result<(), gnx_core::GnxError> {
    let oracle = read_jsonl(&args.oracle)
        .map_err(|e| gnx_core::GnxError::InvalidArgument(format!("read oracle: {e}")))?;
    let gnx = read_jsonl(&args.gnx)
        .map_err(|e| gnx_core::GnxError::InvalidArgument(format!("read gnx dump: {e}")))?;

    let normalize = pick_normalize(&args.lang);
    let (counts, worst, per_tier) = diff(&oracle, &gnx, normalize);
    let report = render_report(&args.lang, &counts, &worst, &per_tier, oracle.len(), gnx.len());

    match args.report.as_deref() {
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        gnx_core::GnxError::InvalidArgument(format!("mkdir report parent: {e}"))
                    })?;
                }
            }
            std::fs::write(p, &report)
                .map_err(|e| gnx_core::GnxError::InvalidArgument(format!("write report: {e}")))?;
            eprintln!("verify-resolver: report written to {}", p.display());
        }
        None => print!("{report}"),
    }
    Ok(())
}

fn read_jsonl(path: &std::path::Path) -> std::io::Result<Vec<Record>> {
    use std::io::BufRead;
    let f = std::fs::File::open(path)?;
    let r = std::io::BufReader::new(f);
    let mut out = Vec::new();
    for (lineno, line) in r.lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("{}:{}: invalid JSON: {}", path.display(), lineno + 1, e);
                continue;
            }
        };
        out.push(Record {
            src_file: v
                .get("src_file")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            specifier: v
                .get("specifier")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            tier: v
                .get("tier")
                .and_then(|x| x.as_str())
                .unwrap_or("Unresolved")
                .to_string(),
            target_file: v
                .get("target_file")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            alt_count: v
                .get("alt_count")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
        });
    }
    Ok(out)
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
    let s = strip_one_of(&s, &[".tsx", ".ts", ".jsx", ".js", ".mjs", ".cjs", ".d.ts"]);
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
) -> (Counts, Vec<WorstOffender>, HashMap<String, Counts>) {
    // Index gnx by (src_file, name) — there may be multiple gnx attempts per
    // key (e.g. heritage + type annotation + call). Pick the BEST attempt:
    //   resolved (tier != Unresolved) > unresolved
    //   SameFile/ImportScoped > Global > Unresolved
    let mut gnx_by_key: HashMap<(String, String), &Record> = HashMap::new();
    let tier_rank = |t: &str| -> u8 {
        match t {
            "SameFile" => 0,
            "ImportScoped" => 1,
            "Global" => 2,
            _ => 3,
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

    let oracle_keys: std::collections::HashSet<_> = oracle
        .iter()
        .map(|r| (r.src_file.clone(), r.name.clone()))
        .collect();

    let mut counts = Counts::default();
    let mut per_tier: HashMap<String, Counts> = HashMap::new();
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

fn render_report(
    lang: &str,
    counts: &Counts,
    worst: &[WorstOffender],
    per_tier: &HashMap<String, Counts>,
    oracle_total: usize,
    gnx_total: usize,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# verify-resolver report ({lang})\n\n"));
    s.push_str(&format!(
        "Oracle records: {oracle_total}\nGnx records: {gnx_total}\n\n"
    ));
    s.push_str("## Summary\n\n");
    s.push_str("| class | count |\n|---|---|\n");
    s.push_str(&format!("| TP (correct) | {} |\n", counts.tp));
    s.push_str(&format!("| FP_ghost (wrong target) | {} |\n", counts.fp_ghost));
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
        let oracle = vec![mk("s.ts", "X", Some("./y"), "ImportScoped", Some("y.ts"), 0)];
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
        let oracle = vec![mk("s.ts", "X", Some("@/y"), "ImportScoped", Some("y.ts"), 0)];
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
        let oracle = vec![mk("s.ts", "X", Some("@/y"), "ImportScoped", Some("y.ts"), 0)];
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
        let oracle = vec![mk("s.ts", "X", Some("@/y"), "ImportScoped", Some("y.ts"), 0)];
        let gnx = vec![mk("s.ts", "X", Some("@/y"), "Unresolved", None, 0)];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.fn_dangling, 1);
        assert_eq!(c.tp, 0);
    }

    #[test]
    fn diff_prefers_best_gnx_attempt_per_key() {
        let oracle = vec![mk("s.ts", "X", Some("@/y"), "ImportScoped", Some("y.ts"), 0)];
        // gnx records two attempts for same (src, name): Unresolved then Global → Global wins
        let gnx = vec![
            mk("s.ts", "X", None, "Unresolved", None, 0),
            mk("s.ts", "X", Some("@/y"), "Global", Some("y.ts"), 0),
        ];
        let (c, _, _) = diff(&oracle, &gnx, normalize_ts);
        assert_eq!(c.tp, 1, "should pick the Global resolution, not Unresolved");
    }
}
