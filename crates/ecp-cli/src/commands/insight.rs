//! `ecp insight` — MCP call telemetry aggregator (unstable v1).
//!
//! Reads `~/.ecp/telemetry/<repo>/calls.jsonl` and emits a per-tool
//! summary: total calls, p50/p99 latency, error rate, and hourly
//! bucket counts for the last N hours (default 24, max 168).
//!
//! **Schema is unstable.** The jsonl line format and the JSON output
//! shape may change in v2 without a semver bump.

use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::registry::resolve_home_ecp;
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct InsightArgs {
    /// Repository name. Defaults to the basename of the current directory.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: json (default), text, or toon.
    #[arg(long, default_value = "json")]
    pub format: Option<String>,

    /// Aggregation window in hours. Default 24, max 168 (one week).
    #[arg(long, default_value_t = 24)]
    pub hours: u64,

    /// Hidden: override the telemetry file path (for tests).
    #[arg(long, hide = true)]
    pub telemetry_path: Option<PathBuf>,
}

pub fn run(args: InsightArgs) -> Result<(), EcpError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let payload = build_payload(&args)?;
    emit(&payload, format)
}

pub fn build_payload(args: &InsightArgs) -> Result<Value, EcpError> {
    let hours = args.hours.min(168);

    let (jsonl_path, display_path) = resolve_path(args)?;

    if !jsonl_path.exists() {
        return Ok(json!({
            "status": "no_telemetry",
            "telemetry_path": display_path,
            "hint": "MCP not invoked yet, or telemetry was deleted"
        }));
    }

    let cutoff_secs = cutoff_unix_secs(hours);
    let records = read_window(&jsonl_path, cutoff_secs)?;

    if records.is_empty() {
        return Ok(json!({
            "status": "no_telemetry",
            "telemetry_path": display_path,
            "hint": "No calls in the requested time window"
        }));
    }

    let total_calls = records.len();
    let by_tool = aggregate_by_tool(&records);
    let hourly_buckets = hourly_buckets(&records, hours);

    Ok(json!({
        "status": "success",
        "telemetry_path": display_path,
        "total_calls": total_calls,
        "window_hours": hours,
        "by_tool": by_tool,
        "hourly_buckets": hourly_buckets
    }))
}

// ─── path resolution ──────────────────────────────────────────────────────────

fn resolve_path(args: &InsightArgs) -> Result<(PathBuf, String), EcpError> {
    if let Some(explicit) = &args.telemetry_path {
        let display = explicit.to_string_lossy().to_string();
        return Ok((explicit.clone(), display));
    }

    let repo_name = match &args.repo {
        Some(r) => r.clone(),
        None => std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .ok_or_else(|| {
                EcpError::InvalidArgument("cannot determine repo name from cwd".into())
            })?,
    };

    let base = resolve_home_ecp();
    let path = base.join("telemetry").join(&repo_name).join("calls.jsonl");
    let display = format!("~/.ecp/telemetry/{repo_name}/calls.jsonl");
    Ok((path, display))
}

// ─── record parsing ───────────────────────────────────────────────────────────

struct Record {
    ts_secs: u64,
    tool: String,
    duration_ms: u64,
    ok: bool,
}

fn read_window(path: &Path, cutoff_secs: u64) -> Result<Vec<Record>, EcpError> {
    let file = std::fs::File::open(path)
        .map_err(|e| EcpError::InvalidArgument(format!("open telemetry: {e}")))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue, // silently skip unreadable lines
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // skip malformed lines
        };
        let Some(ts_str) = v.get("ts").and_then(Value::as_str) else {
            continue;
        };
        let ts_secs = parse_rfc3339_secs(ts_str).unwrap_or(0);
        if ts_secs < cutoff_secs {
            continue;
        }
        let tool = v
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let duration_ms = v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
        let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(true);
        records.push(Record {
            ts_secs,
            tool,
            duration_ms,
            ok,
        });
    }

    Ok(records)
}

// ─── aggregation ─────────────────────────────────────────────────────────────

fn aggregate_by_tool(records: &[Record]) -> Value {
    // Group durations and error counts per tool.
    let mut tool_durations: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut tool_errors: BTreeMap<&str, u64> = BTreeMap::new();

    for r in records {
        tool_durations
            .entry(r.tool.as_str())
            .or_default()
            .push(r.duration_ms);
        if !r.ok {
            *tool_errors.entry(r.tool.as_str()).or_default() += 1;
        }
    }

    let mut by_tool = Vec::new();
    for (tool, mut durations) in tool_durations {
        let calls = durations.len();
        let errors = *tool_errors.get(tool).unwrap_or(&0);
        durations.sort_unstable();
        let p50 = percentile(&durations, 50);
        let p99 = percentile(&durations, 99);
        let error_rate = if calls > 0 {
            (errors as f64) / (calls as f64)
        } else {
            0.0
        };
        // Round to 4 decimals to match output.rs compress_for_llm convention.
        let error_rate = (error_rate * 10000.0).round() / 10000.0;
        by_tool.push(json!({
            "tool": tool,
            "calls": calls,
            "p50_ms": p50,
            "p99_ms": p99,
            "error_rate": error_rate
        }));
    }
    Value::Array(by_tool)
}

fn hourly_buckets(records: &[Record], hours: u64) -> Value {
    let now_secs = now_unix_secs();
    // Bucket by truncating to the hour.
    let mut buckets: BTreeMap<u64, u64> = BTreeMap::new();
    // Pre-seed all hours in the window (so empty hours appear).
    for h in 0..hours {
        let bucket_start = (now_secs / 3600 - (hours - 1 - h)) * 3600;
        buckets.insert(bucket_start, 0);
    }
    for r in records {
        let bucket = (r.ts_secs / 3600) * 3600;
        *buckets.entry(bucket).or_default() += 1;
    }
    let arr: Vec<Value> = buckets
        .into_iter()
        .map(|(secs, calls)| {
            json!({
                "hour": unix_secs_to_rfc3339(secs),
                "calls": calls
            })
        })
        .collect();
    Value::Array(arr)
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * pct) / 100;
    sorted[idx]
}

// ─── time helpers (stdlib-only, mirrors telemetry.rs) ────────────────────────

fn now_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn cutoff_unix_secs(hours: u64) -> u64 {
    now_unix_secs().saturating_sub(hours * 3600)
}

/// Minimal RFC3339 parser: handles `YYYY-MM-DDTHH:MM:SSZ` only.
/// Fields beyond seconds (fractional, offsets) are ignored — telemetry
/// writer emits whole-second precision.
fn parse_rfc3339_secs(s: &str) -> Option<u64> {
    // Expected: 2026-05-23T07:30:00Z  (20 chars minimum)
    if s.len() < 19 {
        return None;
    }
    let year: u64 = s[0..4].parse().ok()?;
    let month: u64 = s[5..7].parse().ok()?;
    let day: u64 = s[8..10].parse().ok()?;
    let hh: u64 = s[11..13].parse().ok()?;
    let mm: u64 = s[14..16].parse().ok()?;
    let ss: u64 = s[17..19].parse().ok()?;

    // Days since epoch via Gregorian formula (same as telemetry.rs).
    let days = ymd_to_days(year, month, day)?;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

fn ymd_to_days(y: u64, m: u64, d: u64) -> Option<u64> {
    if m == 0 || m > 12 || d == 0 || d > 31 {
        return None;
    }
    // Shift year so Mar 1 is the start (simplifies leap-year logic).
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y / 400;
    let yoe = y % 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe;
    // Subtract days between 0000-03-01 and 1970-01-01 = 719468
    days.checked_sub(719468)
}

fn unix_secs_to_rfc3339(secs: u64) -> String {
    let days = secs / 86400;
    let time = secs % 86400;
    let hh = time / 3600;
    let mm = (time % 3600) / 60;
    let ss = time % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}
