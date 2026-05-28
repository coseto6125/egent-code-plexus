//! `ecp usage` — human-facing usage dashboard over CLI + MCP telemetry.
//!
//! Reads `cli-calls.jsonl` (+ MCP `calls.jsonl`) for a repo (or all repos),
//! aggregates invocation counts, p50/p99 latency, error rate, and per-kind
//! error tallies. Default output is a terminal ASCII dashboard (Task 7);
//! `--format json` emits the machine-readable shape below.

use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::registry::resolve_home_ecp;
use ecp_core::time::parse_rfc3339_secs;
use ecp_core::EcpError;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const BUDGET_MS: u64 = 30;

#[derive(Args, Debug, Clone)]
pub struct UsageArgs {
    /// Scope to the current repository only (default: all repos).
    #[arg(short = 'p', long)]
    pub project: bool,
    /// Show recent failing commands with messages instead of the dashboard.
    #[arg(long)]
    pub failures: bool,
    /// Show the full per-subcommand table (no truncation).
    #[arg(long)]
    pub all: bool,
    /// Output format: text (default) or json.
    #[arg(long, default_value = "text")]
    pub format: Option<String>,
    /// Force color off (also honored: NO_COLOR env, non-TTY stdout).
    #[arg(long)]
    pub no_color: bool,
    /// Delete the CLI telemetry log (cli-calls.jsonl); MCP calls.jsonl is kept.
    /// Honors -p to scope to the current repo; otherwise clears every repo.
    #[arg(long)]
    pub clear: bool,
    /// Hidden: read a single explicit telemetry dir (tests).
    #[arg(long, hide = true)]
    pub telemetry_dir: Option<PathBuf>,
}

pub struct Rec {
    pub ts_secs: u64,
    pub tool: String,
    pub duration_ms: u64,
    pub ok: bool,
    pub source: String,
    pub error_kind: Option<String>,
    /// v2 nested verb (e.g. `"gc"` for tool=`"admin"`). Absent on pre-v2 lines.
    pub subcommand: Option<String>,
    /// v2 sanitized error message. Absent on pre-v2 lines and on success.
    pub error_msg: Option<String>,
    pub raw: String,
}

pub fn run(args: UsageArgs) -> Result<(), EcpError> {
    if args.clear {
        return run_clear(&args);
    }
    let format = OutputFormat::parse(args.format.as_deref());
    let recs = collect_records(&args)?;
    if matches!(format, OutputFormat::Json) {
        return emit(&build_json(&recs), format);
    }
    let want_color = crate::commands::usage_render::color_enabled(&args, &format);
    let text = if args.failures {
        crate::commands::usage_render::render_failures(&recs, want_color)
    } else {
        crate::commands::usage_render::render_dashboard(&recs, want_color, args.all)
    };
    println!("{text}");
    Ok(())
}

/// Delete `cli-calls.jsonl` in each scanned telemetry dir. MCP `calls.jsonl`
/// is left intact (it belongs to the MCP path / `ecp insight`). Reports how
/// many logs were removed. No interactive confirmation — matches the
/// non-interactive style of `ecp admin drop`; telemetry is cheap to re-accrue.
fn run_clear(args: &UsageArgs) -> Result<(), EcpError> {
    let mut removed = 0usize;
    for dir in scan_dirs(args)? {
        let log = dir.join("cli-calls.jsonl");
        if log.exists() && std::fs::remove_file(&log).is_ok() {
            removed += 1;
        }
    }
    let scope = if args.project {
        "current repo"
    } else {
        "all repos"
    };
    println!("cleared CLI telemetry: {removed} log(s) removed ({scope}); MCP calls.jsonl kept");
    Ok(())
}

fn scan_dirs(args: &UsageArgs) -> Result<Vec<PathBuf>, EcpError> {
    if let Some(d) = &args.telemetry_dir {
        return Ok(vec![d.clone()]);
    }
    let root = resolve_home_ecp().join("telemetry");
    if args.project {
        let cwd =
            std::env::current_dir().map_err(|e| EcpError::InvalidArgument(format!("cwd: {e}")))?;
        let key = crate::repo_identity::repo_dir_name_for_cwd(&cwd)
            .map_err(|e| EcpError::InvalidArgument(format!("repo identity: {e}")))?;
        return Ok(vec![root.join(key)]);
    }
    let mut dirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }
    Ok(dirs)
}

fn collect_records(args: &UsageArgs) -> Result<Vec<Rec>, EcpError> {
    let mut recs = Vec::new();
    for dir in scan_dirs(args)? {
        prune_retention(&dir);
        for name in ["cli-calls.jsonl", "calls.jsonl"] {
            read_file(&dir.join(name), &mut recs);
        }
    }
    Ok(recs)
}

/// Rewrite `cli-calls.jsonl` dropping lines older than `retention_days`.
/// Off the hot path: only `ecp usage` and `ecp admin gc` call this. Best-effort.
/// MCP `calls.jsonl` is intentionally NOT touched.
pub(crate) fn prune_retention(dir: &Path) {
    let days = retention_days();
    let cutoff = now_unix_secs().saturating_sub(days * 86_400);
    let path = dir.join("cli-calls.jsonl");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let line_count = body.lines().count();
    let kept: Vec<&str> = body
        .lines()
        .filter(|l| {
            serde_json::from_str::<Value>(l)
                .ok()
                .and_then(|v| v.get("ts").and_then(Value::as_str).map(str::to_string))
                .and_then(|ts| parse_rfc3339_secs(&ts))
                .map(|secs| secs >= cutoff)
                .unwrap_or(true) // keep unparseable lines
        })
        .collect();
    if kept.len() != line_count {
        let _ = std::fs::write(&path, kept.join("\n") + "\n");
    }
}

fn retention_days() -> u64 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ecp_core::config::load(&cwd)
        .map(|c| c.telemetry.retention_days)
        .unwrap_or(7)
}

fn now_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_file(path: &Path, out: &mut Vec<Rec>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(ts) = v.get("ts").and_then(Value::as_str) else {
            continue;
        };
        out.push(Rec {
            ts_secs: parse_rfc3339_secs(ts).unwrap_or(0),
            tool: v
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            duration_ms: v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(true),
            source: v
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("mcp")
                .to_string(),
            error_kind: v
                .get("error_kind")
                .and_then(Value::as_str)
                .map(str::to_string),
            subcommand: v
                .get("subcommand")
                .and_then(Value::as_str)
                .map(str::to_string),
            error_msg: v
                .get("error_msg")
                .and_then(Value::as_str)
                .map(str::to_string),
            raw: line.to_string(),
        });
    }
}

pub(crate) fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * pct) / 100]
}

pub struct CmdStat {
    pub cmd: String,
    pub count: usize,
    pub p50: u64,
    pub p99: u64,
    pub errors: usize,
}

pub fn by_command(recs: &[Rec]) -> Vec<CmdStat> {
    let mut durs: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut errs: BTreeMap<&str, usize> = BTreeMap::new();
    for r in recs {
        durs.entry(&r.tool).or_default().push(r.duration_ms);
        if !r.ok {
            *errs.entry(&r.tool).or_default() += 1;
        }
    }
    let mut stats: Vec<CmdStat> = durs
        .into_iter()
        .map(|(cmd, mut d)| {
            d.sort_unstable();
            CmdStat {
                cmd: cmd.to_string(),
                count: d.len(),
                p50: percentile(&d, 50),
                p99: percentile(&d, 99),
                errors: *errs.get(cmd).unwrap_or(&0),
            }
        })
        .collect();
    stats.sort_by_key(|s| std::cmp::Reverse(s.count));
    stats
}

pub fn errors_by_kind(recs: &[Rec]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for r in recs.iter().filter(|r| !r.ok) {
        let k = r.error_kind.clone().unwrap_or_else(|| "other".to_string());
        *m.entry(k).or_default() += 1;
    }
    m
}

fn build_json(recs: &[Rec]) -> Value {
    let total = recs.len();
    let errors = recs.iter().filter(|r| !r.ok).count();
    let within = recs.iter().filter(|r| r.duration_ms <= BUDGET_MS).count();
    let by_command: Vec<Value> = by_command(recs)
        .iter()
        .map(|s| {
            let er = if s.count > 0 {
                (s.errors as f64 / s.count as f64 * 10000.0).round() / 10000.0
            } else {
                0.0
            };
            json!({"cmd": s.cmd, "count": s.count, "p50_ms": s.p50, "p99_ms": s.p99, "err_rate": er})
        })
        .collect();
    let mut ebk = Map::new();
    for (k, n) in errors_by_kind(recs) {
        ebk.insert(k, json!(n));
    }
    let err_rate = if total > 0 {
        (errors as f64 / total as f64 * 10000.0).round() / 10000.0
    } else {
        0.0
    };
    let within_pct = if total > 0 {
        (within as f64 / total as f64 * 10000.0).round() / 10000.0
    } else {
        0.0
    };
    json!({
        "total": total,
        "error_rate": err_rate,
        "by_command": by_command,
        "errors_by_kind": Value::Object(ebk),
        "within_budget_pct": within_pct
    })
}
