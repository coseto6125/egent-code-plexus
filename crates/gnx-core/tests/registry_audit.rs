//! Tests for ~/.gnx/audit.log (spec §9).

use gnx_core::registry::{AuditEvent, AuditLog};

#[test]
fn event_serializes_to_jsonl() {
    let e = AuditEvent::AnalyzeComplete {
        repo: "gitnexus-rs".into(),
        branch: "main".into(),
        files: 234,
        nodes: 12453,
        duration_ms: 4521,
    };
    let line = e.to_json_line().unwrap();
    assert!(line.starts_with('{'));
    assert!(line.contains("\"event\":\"analyze.complete\""));
    assert!(line.contains("\"files\":234"));
    assert!(line.ends_with('\n'));
}

#[test]
fn append_event_to_fresh_file() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("audit.log");
    let log = AuditLog::open(&log_path).unwrap();

    log.append(&AuditEvent::HookFired {
        kind: "rename".into(),
        from: Some("old".into()),
        to: Some("new".into()),
        repo: "gitnexus-rs".into(),
    })
    .unwrap();

    let content = std::fs::read_to_string(&log_path).unwrap();
    assert!(content.contains("\"event\":\"hook.fired\""));
    assert!(content.contains("\"type\":\"rename\""));
}

#[test]
fn rotates_at_5mb_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("audit.log");
    let log = AuditLog::open(&log_path).unwrap();

    let big_event = AuditEvent::AnalyzeComplete {
        repo: "x".repeat(1000),
        branch: "x".repeat(1000),
        files: 1,
        nodes: 1,
        duration_ms: 1,
    };

    for _ in 0..3000 {
        log.append(&big_event).unwrap();
    }

    let rotated_1 = tmp.path().join("audit.log.1");
    assert!(rotated_1.exists(), "expected audit.log.1 after rotation");
}

#[test]
fn keeps_only_two_rotated() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("audit.log");
    let log = AuditLog::open(&log_path).unwrap();

    let big = AuditEvent::AnalyzeComplete {
        repo: "x".repeat(2000),
        branch: "x".repeat(2000),
        files: 1,
        nodes: 1,
        duration_ms: 1,
    };

    for _ in 0..6000 {
        log.append(&big).unwrap();
    }

    let r2 = tmp.path().join("audit.log.2");
    assert!(
        r2.exists(),
        "expected audit.log.2 to exist after multiple rotations"
    );
    let r3 = tmp.path().join("audit.log.3");
    assert!(!r3.exists(), "audit.log.3 must not exist (cap = 2)");
}

use std::time::{Duration, SystemTime};

#[test]
fn cleanup_old_keeps_recent_rotated() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("audit.log");

    // Create a recent (80-day-old) rotated file
    let recent_rotated = tmp.path().join("audit.log.1");
    std::fs::write(&recent_rotated, "recent data").unwrap();
    let recent_time = SystemTime::now() - Duration::from_secs(80 * 24 * 3600);
    filetime::set_file_mtime(
        &recent_rotated,
        filetime::FileTime::from_system_time(recent_time),
    )
    .unwrap();

    let log = AuditLog::open(&log_path).unwrap();
    log.cleanup_old(Duration::from_secs(90 * 24 * 3600)).unwrap();

    assert!(
        recent_rotated.exists(),
        "expected 80-day-old rotated to be retained under 90-day policy"
    );
}

#[test]
fn deletes_rotated_older_than_90_days() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("audit.log");

    // Manually create an old audit.log.1 with mtime 100 days ago
    let old_rotated = tmp.path().join("audit.log.1");
    std::fs::write(&old_rotated, "old data").unwrap();
    let old_time = SystemTime::now() - Duration::from_secs(100 * 24 * 3600);
    filetime::set_file_mtime(
        &old_rotated,
        filetime::FileTime::from_system_time(old_time),
    )
    .unwrap();

    let log = AuditLog::open(&log_path).unwrap();
    log.cleanup_old(Duration::from_secs(90 * 24 * 3600)).unwrap();

    assert!(!old_rotated.exists(), "expected old rotated to be deleted");
}
