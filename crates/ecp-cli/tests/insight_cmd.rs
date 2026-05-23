//! Integration tests for `ecp insight`.
//!
//! Uses `--telemetry-path <fixture>` (hidden flag) to avoid touching
//! the real `~/.ecp/` directory and to keep tests reproducible.

use ecp_cli::commands::insight::{build_payload, InsightArgs};
use std::path::PathBuf;
use tempfile::TempDir;

// ─── fixture helpers ──────────────────────────────────────────────────────────

/// RFC3339 UTC timestamp that is definitely within the last 24h window.
fn recent_ts() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Use a fixed offset from now: 30 minutes ago.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 1800;
    unix_to_rfc3339(secs)
}

/// RFC3339 UTC from Unix seconds (same algorithm as insight.rs).
fn unix_to_rfc3339(secs: u64) -> String {
    let days = secs / 86400;
    let time = secs % 86400;
    let hh = time / 3600;
    let mm = (time % 3600) / 60;
    let ss = time % 60;
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
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

/// Write a fixture jsonl file with `n` calls for `tool`, all within the
/// last 24h window.
fn write_fixture(dir: &std::path::Path, lines: &[(&str, u64, bool)]) -> PathBuf {
    let path = dir.join("calls.jsonl");
    let ts = recent_ts();
    let content: String = lines
        .iter()
        .map(|(tool, dur, ok)| {
            format!("{{\"ts\":\"{ts}\",\"tool\":\"{tool}\",\"duration_ms\":{dur},\"ok\":{ok}}}\n")
        })
        .collect();
    std::fs::write(&path, content).unwrap();
    path
}

fn insight_args(telemetry_path: PathBuf) -> InsightArgs {
    InsightArgs {
        repo: None,
        format: Some("json".into()),
        hours: 24,
        telemetry_path: Some(telemetry_path),
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[test]
fn insight_missing_telemetry_emits_no_telemetry_status() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("nonexistent.jsonl");
    let args = insight_args(missing);
    let payload = build_payload(&args).unwrap();
    assert_eq!(payload["status"], "no_telemetry");
    assert!(payload["hint"].as_str().is_some());
}

#[test]
fn insight_10_lines_two_tools_aggregates_correctly() {
    let dir = TempDir::new().unwrap();
    // 6 calls for ecp_inspect with durations [10,20,30,40,50,60]
    // 4 calls for ecp_find   with durations [5,15,25,35], 1 error
    let lines: &[(&str, u64, bool)] = &[
        ("ecp_inspect", 10, true),
        ("ecp_inspect", 20, true),
        ("ecp_inspect", 30, true),
        ("ecp_inspect", 40, true),
        ("ecp_inspect", 50, true),
        ("ecp_inspect", 60, true),
        ("ecp_find", 5, true),
        ("ecp_find", 15, false), // error
        ("ecp_find", 25, true),
        ("ecp_find", 35, true),
    ];
    let path = write_fixture(dir.path(), lines);
    let args = insight_args(path);
    let payload = build_payload(&args).unwrap();

    assert_eq!(payload["status"], "success");
    assert_eq!(payload["total_calls"], 10);

    let by_tool = payload["by_tool"].as_array().unwrap();
    assert_eq!(by_tool.len(), 2);

    let inspect = by_tool
        .iter()
        .find(|t| t["tool"] == "ecp_inspect")
        .expect("ecp_inspect entry missing");
    assert_eq!(inspect["calls"], 6);
    // sorted durations: [10,20,30,40,50,60]; idx = (n-1)*pct/100
    // p50: (5*50)/100 = 2 → 30; p99: (5*99)/100 = 4 → 50
    assert_eq!(inspect["p50_ms"], 30, "p50 for ecp_inspect");
    assert_eq!(inspect["p99_ms"], 50, "p99 for ecp_inspect");
    assert_eq!(inspect["error_rate"], 0.0, "no errors for ecp_inspect");

    let find = by_tool
        .iter()
        .find(|t| t["tool"] == "ecp_find")
        .expect("ecp_find entry missing");
    assert_eq!(find["calls"], 4);
    // sorted durations: [5,15,25,35]; idx = (n-1)*pct/100
    // p50: (3*50)/100 = 1 → 15; p99: (3*99)/100 = 2 → 25
    assert_eq!(find["p50_ms"], 15, "p50 for ecp_find");
    assert_eq!(find["p99_ms"], 25, "p99 for ecp_find");
    // 1 error out of 4 = 0.25
    let er = find["error_rate"].as_f64().unwrap();
    assert!(
        (er - 0.25).abs() < 1e-6,
        "error_rate should be 0.25, got {er}"
    );
}

#[test]
fn insight_hourly_buckets_present() {
    let dir = TempDir::new().unwrap();
    let path = write_fixture(dir.path(), &[("ecp_inspect", 20, true)]);
    let args = insight_args(path);
    let payload = build_payload(&args).unwrap();
    let buckets = payload["hourly_buckets"].as_array().unwrap();
    // 24h window → 24 buckets pre-seeded
    assert_eq!(buckets.len(), 24);
    // At least one bucket has calls = 1
    let with_calls: Vec<_> = buckets
        .iter()
        .filter(|b| b["calls"].as_u64().unwrap_or(0) > 0)
        .collect();
    assert!(
        !with_calls.is_empty(),
        "at least one bucket must have calls > 0"
    );
}

#[test]
fn insight_old_records_outside_window_excluded() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("calls.jsonl");
    // Timestamp from 48h ago (outside default 24h window)
    let old_secs = {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(48 * 3600)
    };
    let old_ts = unix_to_rfc3339(old_secs);
    std::fs::write(
        &path,
        format!("{{\"ts\":\"{old_ts}\",\"tool\":\"ecp_old\",\"duration_ms\":5,\"ok\":true}}\n"),
    )
    .unwrap();

    let args = insight_args(path);
    let payload = build_payload(&args).unwrap();
    // Old record filtered out → no_telemetry status
    assert_eq!(payload["status"], "no_telemetry");
}
