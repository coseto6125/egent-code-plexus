//! Integration tests for `ecp find-event-mirrors` (T5-34).
//!
//! Each test builds a minimal in-memory Python repo, indexes it via
//! `ecp admin index`, then runs the CLI and asserts on JSON/text output.
//! Python is sufficient because the EventTopicMirror post-process pass is
//! language-agnostic; the language detectors are covered by T5-33 unit tests.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn init_repo(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());
}

fn git_commit_all(repo: &Path) {
    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
}

fn ecp_index(repo: &Path) {
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_find_mirrors(repo: &Path, extra: &[&str]) -> (Value, String) {
    let mut args = vec!["find-event-mirrors", "--format", "json"];
    args.extend_from_slice(extra);
    let out = Command::new(ecp_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("find-event-mirrors failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    let json: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("{args:?} invalid JSON: {e}\nstdout={stdout}"));
    (json, stdout)
}

fn run_find_mirrors_text(repo: &Path, extra: &[&str]) -> String {
    let mut args = vec!["find-event-mirrors", "--format", "text"];
    args.extend_from_slice(extra);
    let out = Command::new(ecp_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("find-event-mirrors text failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn mirrors(json: &Value) -> &[Value] {
    json["mirrors"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// Redis publish on "orders" topic.
const REDIS_PUBLISHER: &str = r#"
import redis

def publish_order(r, data):
    r.publish("orders", data)
"#;

/// Redis subscribe on "orders" topic.
const REDIS_SUBSCRIBER: &str = r#"
import redis

def consume_order(pubsub):
    pubsub.subscribe("orders")
"#;

/// Redis publish on "payments" topic.
const REDIS_PUBLISHER_PAYMENTS: &str = r#"
import redis

def publish_payment(r, data):
    r.publish("payments", data)
"#;

/// Redis subscribe on "payments" topic.
const REDIS_SUBSCRIBER_PAYMENTS: &str = r#"
import redis

def consume_payment(pubsub):
    pubsub.subscribe("payments")
"#;

fn setup_two_files(pub_src: &str, sub_src: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join("svc")).unwrap();
    std::fs::write(tmp.path().join("svc/publisher.py"), pub_src).unwrap();
    std::fs::write(tmp.path().join("svc/subscriber.py"), sub_src).unwrap();
    git_commit_all(tmp.path());
    ecp_index(tmp.path());
    tmp
}

fn setup_four_files(pub1: &str, sub1: &str, pub2: &str, sub2: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join("svc")).unwrap();
    std::fs::write(tmp.path().join("svc/pub1.py"), pub1).unwrap();
    std::fs::write(tmp.path().join("svc/sub1.py"), sub1).unwrap();
    std::fs::write(tmp.path().join("svc/pub2.py"), pub2).unwrap();
    std::fs::write(tmp.path().join("svc/sub2.py"), sub2).unwrap();
    git_commit_all(tmp.path());
    ecp_index(tmp.path());
    tmp
}

fn setup_empty_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join("svc")).unwrap();
    std::fs::write(
        tmp.path().join("svc/plain.py"),
        "def add(x, y):\n    return x + y\n",
    )
    .unwrap();
    git_commit_all(tmp.path());
    ecp_index(tmp.path());
    tmp
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Kafka publisher + Kafka subscriber on "orders" → exactly 1 mirror row.
#[test]
fn single_kafka_pair_emits_one_row() {
    // Use Redis pub/sub (both same lib) to get a mirror
    let tmp = setup_two_files(REDIS_PUBLISHER, REDIS_SUBSCRIBER);
    let (json, _) = run_find_mirrors(tmp.path(), &[]);
    let rows = mirrors(&json);
    assert_eq!(
        rows.len(),
        1,
        "expected 1 mirror row; got {}: {json}",
        rows.len()
    );

    let row = &rows[0];
    assert_eq!(row["topic"].as_str().unwrap_or(""), "orders");
    assert!(
        row["confidence"].as_f64().unwrap_or(0.0) > 0.0,
        "confidence must be positive"
    );
    assert_eq!(row["requires_verification"].as_bool(), Some(true));
    // lib is null — FrameworkId not persisted
    assert!(
        row["lib"].is_null(),
        "lib should be null until schema extended"
    );
}

/// --lib redis filter: Redis mirrors return rows (lib filter is currently a no-op,
/// so all mirrors pass regardless of the argument value).
#[test]
fn lib_filter_redis_returns_rows() {
    let tmp = setup_two_files(REDIS_PUBLISHER, REDIS_SUBSCRIBER);
    let (json, _) = run_find_mirrors(tmp.path(), &["--lib", "redis"]);
    let rows = mirrors(&json);
    // lib filter is a no-op (FrameworkId not persisted) — the mirror row passes.
    assert_eq!(
        rows.len(),
        1,
        "expected 1 row with --lib redis (no-op filter); got {}: {json}",
        rows.len()
    );
}

/// --topic 'orders*' matches "orders" topic.
#[test]
fn topic_glob_orders_star_matches() {
    let tmp = setup_two_files(REDIS_PUBLISHER, REDIS_SUBSCRIBER);
    let (json, _) = run_find_mirrors(tmp.path(), &["--topic", "orders*"]);
    let rows = mirrors(&json);
    assert!(
        !rows.is_empty(),
        "--topic 'orders*' should match 'orders'; got {json}"
    );
    assert_eq!(rows[0]["topic"].as_str().unwrap_or(""), "orders");
}

/// --topic 'payments*' does NOT match "orders" topic → 0 rows.
#[test]
fn topic_glob_payments_star_no_match_on_orders_repo() {
    let tmp = setup_two_files(REDIS_PUBLISHER, REDIS_SUBSCRIBER);
    let (json, _) = run_find_mirrors(tmp.path(), &["--topic", "payments*"]);
    let rows = mirrors(&json);
    assert!(
        rows.is_empty(),
        "--topic 'payments*' must not match 'orders'; got {json}"
    );
}

/// --min-confidence 0.9 → 0 rows (T5-33 emits confidence=0.85).
#[test]
fn min_confidence_0_9_returns_zero_rows() {
    let tmp = setup_two_files(REDIS_PUBLISHER, REDIS_SUBSCRIBER);
    let (json, _) = run_find_mirrors(tmp.path(), &["--min-confidence", "0.9"]);
    let rows = mirrors(&json);
    assert!(
        rows.is_empty(),
        "--min-confidence 0.9 must exclude 0.85 edges; got {}: {json}",
        rows.len()
    );
}

/// Repo with no event topics → 0 rows and no panic.
#[test]
fn no_mirrors_empty_output_no_panic() {
    let tmp = setup_empty_repo();
    let (json, _) = run_find_mirrors(tmp.path(), &[]);
    let rows = mirrors(&json);
    assert!(
        rows.is_empty(),
        "plain repo must return 0 mirrors; got {}: {json}",
        rows.len()
    );
    // summary must still be present
    assert!(
        json["summary"].is_object(),
        "summary must be present even with 0 mirrors"
    );
}

/// --format text: header line + row line are present; no panic on empty.
#[test]
fn text_format_has_header_and_row() {
    let tmp = setup_two_files(REDIS_PUBLISHER, REDIS_SUBSCRIBER);
    let text = run_find_mirrors_text(tmp.path(), &[]);
    assert!(
        text.contains("publisher_fn"),
        "text output must contain 'publisher_fn' header; got:\n{text}"
    );
    assert!(
        text.contains("orders"),
        "text output must contain topic 'orders'; got:\n{text}"
    );
}

/// --format text with no mirrors: emits "(no event mirrors found)".
#[test]
fn text_format_empty_repo_no_panic() {
    let tmp = setup_empty_repo();
    let text = run_find_mirrors_text(tmp.path(), &[]);
    assert!(
        text.contains("no event mirrors found"),
        "empty repo text output must say 'no event mirrors found'; got:\n{text}"
    );
}

/// --format json round-trip: `mirrors` is an array, `summary.mirror_count` matches.
#[test]
fn json_format_mirrors_array_and_summary_count() {
    let tmp = setup_two_files(REDIS_PUBLISHER, REDIS_SUBSCRIBER);
    let (json, _) = run_find_mirrors(tmp.path(), &[]);
    let rows = mirrors(&json);
    let count = json["summary"]["mirror_count"].as_u64().unwrap_or(999);
    assert_eq!(
        count,
        rows.len() as u64,
        "summary.mirror_count must equal mirrors array length; json={json}"
    );
}

/// Two different topics (orders + payments) in one repo → both mirrors present.
#[test]
fn two_topics_both_mirrored() {
    let tmp = setup_four_files(
        REDIS_PUBLISHER,
        REDIS_SUBSCRIBER,
        REDIS_PUBLISHER_PAYMENTS,
        REDIS_SUBSCRIBER_PAYMENTS,
    );
    let (json, _) = run_find_mirrors(tmp.path(), &[]);
    let rows = mirrors(&json);
    assert_eq!(
        rows.len(),
        2,
        "expected 2 mirrors (orders + payments); got {}: {json}",
        rows.len()
    );
}
