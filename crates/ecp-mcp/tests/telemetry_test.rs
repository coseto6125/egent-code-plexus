//! Tests for the best-effort jsonl telemetry write path.

use ecp_mcp::telemetry::{append_to, rfc3339_now, CallRecord};
use tempfile::TempDir;

// ─── write-side: happy path ───────────────────────────────────────────────────

#[test]
fn append_to_produces_valid_jsonl_line() {
    let dir = TempDir::new().unwrap();
    let ts = rfc3339_now();
    let record = CallRecord {
        ts: &ts,
        tool: "ecp_inspect",
        duration_ms: 42,
        ok: true,
    };
    append_to(&record, dir.path());

    let path = dir.path().join("calls.jsonl");
    assert!(path.exists(), "calls.jsonl must be created");

    let content = std::fs::read_to_string(&path).unwrap();
    let line = content.trim();
    assert!(!line.is_empty(), "file must not be empty");

    let v: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("expected valid JSON: {e}\ngot: {line}"));

    assert_eq!(v["tool"], "ecp_inspect");
    assert_eq!(v["duration_ms"], 42);
    assert_eq!(v["ok"], true);
    // ts must be a non-empty string
    assert!(v["ts"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
}

#[test]
fn append_to_accumulates_multiple_lines() {
    let dir = TempDir::new().unwrap();
    let ts = rfc3339_now();
    for i in 0..3u64 {
        let tool = format!("ecp_tool_{i}");
        let record = CallRecord {
            ts: &ts,
            tool: &tool,
            duration_ms: i * 10,
            ok: i % 2 == 0,
        };
        append_to(&record, dir.path());
    }

    let content = std::fs::read_to_string(dir.path().join("calls.jsonl")).unwrap();
    let lines: Vec<_> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "expected 3 jsonl lines");

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v.get("tool").is_some());
        assert!(v.get("duration_ms").is_some());
        assert!(v.get("ok").is_some());
    }
}

// ─── write-side: failure does not panic ───────────────────────────────────────

#[test]
fn append_to_read_only_dir_does_not_panic() {
    // Point at a path that cannot be created (nested under a non-existent root).
    // On most systems `/proc/readonly_fake_ecp_test/sub` will fail to create.
    // If that somehow succeeds (unusual sandbox), the test is a no-op but still passes.
    let unreachable = std::path::Path::new("/proc/readonly_fake_ecp_test_path_6174/telemetry");
    let ts = rfc3339_now();
    let record = CallRecord {
        ts: &ts,
        tool: "ecp_find",
        duration_ms: 1,
        ok: true,
    };
    // Must not panic.
    append_to(&record, unreachable);
}

// ─── rfc3339_now sanity ───────────────────────────────────────────────────────

#[test]
fn rfc3339_now_has_correct_shape() {
    let ts = rfc3339_now();
    // Expected: YYYY-MM-DDTHH:MM:SSZ  (20 chars)
    assert_eq!(ts.len(), 20, "unexpected length: {ts}");
    assert!(ts.ends_with('Z'), "must end with Z: {ts}");
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[7..8], "-");
    assert_eq!(&ts[10..11], "T");
}
