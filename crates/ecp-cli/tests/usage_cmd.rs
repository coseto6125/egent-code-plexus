use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// A recent RFC3339 timestamp (now − 1h) so aggregation-fixture lines stay inside
/// the default 7-day telemetry retention window. Using a fixed past date here is a
/// time-bomb: `usage` prunes lines older than retention before aggregating, so a
/// hard-coded date silently starts failing once it ages past the window.
fn recent_ts() -> String {
    use chrono::{SecondsFormat, Utc};
    (Utc::now() - chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[test]
fn invocation_appends_one_cli_telemetry_line() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out = Command::new(ecp_bin())
        .args(["find", "definitely_no_such_symbol_xyz"])
        .current_dir(&tmp)
        .env("HOME", &tmp)
        .env_remove("ECP_NO_TELEMETRY")
        .output()
        .unwrap();
    let _ = out; // command may fail (no graph) — we only assert telemetry wrote
    let tel_root = tmp.join(".ecp/telemetry");
    let mut found = false;
    if let Ok(entries) = std::fs::read_dir(&tel_root) {
        for e in entries.flatten() {
            let f = e.path().join("cli-calls.jsonl");
            if f.exists() {
                let body = std::fs::read_to_string(&f).unwrap();
                assert!(body.lines().count() >= 1, "expected >=1 telemetry line");
                assert!(body.contains(r#""source":"cli""#));
                found = true;
            }
        }
    }
    assert!(found, "no cli-calls.jsonl written under {tel_root:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn usage_json_aggregates_a_fixture() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-json-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/myrepo__deadbeef");
    std::fs::create_dir_all(&tel).unwrap();
    let ts = recent_ts();
    let lines = [
        format!(r#"{{"ts":"{ts}","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}}"#),
        format!(r#"{{"ts":"{ts}","tool":"inspect","duration_ms":48,"ok":true,"source":"cli","error_kind":null}}"#),
        format!(r#"{{"ts":"{ts}","tool":"cypher","duration_ms":9,"ok":false,"source":"cli","error_kind":"cypher-parse"}}"#),
    ].join("\n");
    std::fs::write(tel.join("cli-calls.jsonl"), lines).unwrap();
    let out = Command::new(ecp_bin())
        .args([
            "usage",
            "--format",
            "json",
            "--telemetry-dir",
            tel.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["total"], 3);
    let by = v["by_command"].as_array().unwrap();
    let inspect = by.iter().find(|c| c["cmd"] == "inspect").unwrap();
    assert_eq!(inspect["count"], 2);
    assert_eq!(v["errors_by_kind"]["cypher-parse"], 1);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn usage_text_dashboard_is_plain_when_piped() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-txt-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__1");
    std::fs::create_dir_all(&tel).unwrap();
    let ts = recent_ts();
    std::fs::write(
        tel.join("cli-calls.jsonl"),
        format!(r#"{{"ts":"{ts}","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}}"#),
    ).unwrap();
    let out = Command::new(ecp_bin())
        .args(["usage", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(!s.contains('\x1b'), "piped output must be color-free");
    assert!(s.contains("Usage"));
    assert!(s.contains("inspect"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn usage_failures_lists_only_errors() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-fail-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__2");
    std::fs::create_dir_all(&tel).unwrap();
    let ts = recent_ts();
    let lines = [
        format!(r#"{{"ts":"{ts}","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}}"#),
        format!(r#"{{"ts":"{ts}","tool":"cypher","duration_ms":9,"ok":false,"source":"cli","error_kind":"cypher-parse"}}"#),
    ].join("\n");
    std::fs::write(tel.join("cli-calls.jsonl"), lines).unwrap();
    let out = Command::new(ecp_bin())
        .args([
            "usage",
            "--failures",
            "--telemetry-dir",
            tel.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("cypher-parse"));
    assert!(
        !s.contains("inspect"),
        "failures view must omit successful commands"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// Literal dates fed into a now-relative retention window must be
// monotone-safe: ancient (always pruned) or far-future (always survives).
// A recent past date asserted as "fresh" expires once the wall clock passes
// ts + retention and turns every PR's CI red (PR #534, FU-2026-06-03-001).
#[test]
fn usage_prunes_lines_older_than_retention() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-prune-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__3");
    std::fs::create_dir_all(&tel).unwrap();
    let lines = [
        r#"{"ts":"2020-01-01T00:00:00Z","tool":"find","duration_ms":4,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2099-01-01T00:00:00Z","tool":"find","duration_ms":4,"ok":true,"source":"cli","error_kind":null}"#,
    ].join("\n");
    let f = tel.join("cli-calls.jsonl");
    std::fs::write(&f, lines).unwrap();
    let _ = Command::new(ecp_bin())
        .args([
            "usage",
            "--format",
            "json",
            "--telemetry-dir",
            tel.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let body = std::fs::read_to_string(&f).unwrap();
    assert!(!body.contains("2020-01-01"), "ancient line must be pruned");
    assert!(body.contains("2099-01-01"), "fresh line must survive");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn usage_clear_removes_cli_log_but_keeps_mcp() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-clear-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__clear");
    std::fs::create_dir_all(&tel).unwrap();
    let cli = tel.join("cli-calls.jsonl");
    let mcp = tel.join("calls.jsonl");
    std::fs::write(
        &cli,
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}"#,
    )
    .unwrap();
    std::fs::write(
        &mcp,
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"ecp_inspect","duration_ms":6,"ok":true}"#,
    )
    .unwrap();
    let out = Command::new(ecp_bin())
        .args(["usage", "--clear", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "clear should exit 0");
    assert!(!cli.exists(), "cli-calls.jsonl must be removed by --clear");
    assert!(mcp.exists(), "MCP calls.jsonl must be preserved by --clear");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn usage_clear_reports_what_it_cleared() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-clearrep-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__rep");
    std::fs::create_dir_all(&tel).unwrap();
    std::fs::write(
        tel.join("cli-calls.jsonl"),
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"find","duration_ms":4,"ok":true,"source":"cli","error_kind":null}"#,
    )
    .unwrap();
    let out = Command::new(ecp_bin())
        .args(["usage", "--clear", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(
        s.to_lowercase().contains("clear"),
        "should report what was cleared, got: {s}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
