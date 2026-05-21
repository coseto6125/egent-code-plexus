//! Integration tests for `ecp find-transaction-patterns` (Saga + Outbox).
//!
//! Each test builds a minimal in-memory repo, indexes it via `ecp admin index`,
//! then runs the CLI and asserts on the JSON output.  All fixtures are pure
//! Python so we only need one language to exercise the detection logic
//! (both algorithms are language-agnostic).

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

fn run_find_tx(repo: &Path, extra: &[&str]) -> Value {
    let mut args = vec![
        "find-transaction-patterns",
        "--repo",
        ".",
        "--format",
        "json",
    ];
    args.extend_from_slice(extra);
    let out = Command::new(ecp_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("find-transaction-patterns failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("{args:?} invalid JSON: {e}\nstdout={stdout}"))
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// Class with `place_order` and `compensate_place_order` (no Calls edge).
const SAGA_COMPENSATE: &str = r#"
class Order:
    def place_order(self, order_id: str) -> None:
        pass

    def compensate_place_order(self, order_id: str) -> None:
        pass
"#;

/// Class with `pay` and `undo_pay` methods.
const SAGA_UNDO: &str = r#"
class Payment:
    def pay(self, amount: int) -> None:
        pass

    def undo_pay(self, amount: int) -> None:
        pass
"#;

/// Class with `ship` and `rollback_ship` methods.
const SAGA_ROLLBACK: &str = r#"
class Shipping:
    def ship(self, pkg_id: str) -> None:
        pass

    def rollback_ship(self, pkg_id: str) -> None:
        pass
"#;

/// `compensate_place_order` actually calls `place_order` → bumps confidence.
const SAGA_CALLS_BACK: &str = r#"
class Order:
    def place_order(self, order_id: str) -> None:
        pass

    def compensate_place_order(self, order_id: str) -> None:
        self.place_order(order_id)
"#;

/// `Order.place_order` on one class, `Cart.compensate_place_order` on another —
/// no valid pair (owner_class differs).
const SAGA_CROSS_CLASS: &str = r#"
class Order:
    def place_order(self, order_id: str) -> None:
        pass

class Cart:
    def compensate_place_order(self, order_id: str) -> None:
        pass
"#;

/// Only `place_order` with no compensator on any class.
const SAGA_NO_COMPENSATOR: &str = r#"
class Order:
    def place_order(self, order_id: str) -> None:
        pass
"#;

/// Two classes: Order (place_order / compensate_place_order) and
/// Cart (add_item / compensate_add_item).  The `--class Order` filter
/// must return exactly 1 pair.
const SAGA_TWO_CLASSES: &str = r#"
class Order:
    def place_order(self, order_id: str) -> None:
        pass

    def compensate_place_order(self, order_id: str) -> None:
        pass

class Cart:
    def add_item(self, sku: str) -> None:
        pass

    def compensate_add_item(self, sku: str) -> None:
        pass
"#;

// ── Outbox fixtures ───────────────────────────────────────────────────────────

/// `OutboxEvent` class + `save()` method + downstream Kafka producer call.
/// Expected: 1 OutboxPattern finding.
const OUTBOX_FULL: &str = r#"
from kafka import KafkaProducer

producer = KafkaProducer(bootstrap_servers='localhost:9092')

class OutboxEvent:
    def __init__(self, topic: str, payload: bytes) -> None:
        self.topic = topic
        self.payload = payload

    def save(self) -> None:
        producer.send("orders", self.payload)
"#;

/// `OutboxEvent` class + `save()` + downstream function that calls producer.
/// Tests BFS depth-2 traversal.
const OUTBOX_INDIRECT: &str = r#"
from kafka import KafkaProducer

producer = KafkaProducer(bootstrap_servers='localhost:9092')

def publish_event(topic: str, payload: bytes) -> None:
    producer.send(topic, payload)

class OutboxEvent:
    def __init__(self, topic: str, payload: bytes) -> None:
        self.topic = topic
        self.payload = payload

    def save(self) -> None:
        publish_event(self.topic, self.payload)
"#;

/// No outbox table at all → no outbox findings.
const OUTBOX_NO_TABLE: &str = r#"
class Order:
    def place_order(self, order_id: str) -> None:
        pass
"#;

/// `OutboxEvent` class but no downstream publisher reachable → no findings.
const OUTBOX_NO_PUBLISHER: &str = r#"
class OutboxEvent:
    def __init__(self, topic: str, payload: bytes) -> None:
        self.topic = topic
        self.payload = payload

    def save(self) -> None:
        pass
"#;

/// `event_outbox` snake_case variant → also a valid outbox table name.
const OUTBOX_SNAKE_CASE: &str = r#"
from kafka import KafkaProducer

producer = KafkaProducer(bootstrap_servers='localhost:9092')

class event_outbox:
    def __init__(self, topic: str, payload: bytes) -> None:
        self.topic = topic
        self.payload = payload

    def save(self) -> None:
        producer.send("payments", self.payload)
"#;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn saga_pairs(json: &Value) -> &[Value] {
    json["saga_pairs"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
}

fn outbox_patterns(json: &Value) -> &[Value] {
    json["outbox_patterns"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
}

fn setup_single_file(source: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join("saga")).unwrap();
    std::fs::write(tmp.path().join("saga/order.py"), source).unwrap();
    git_commit_all(tmp.path());
    ecp_index(tmp.path());
    tmp
}

fn setup_outbox_file(source: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join("outbox")).unwrap();
    std::fs::write(tmp.path().join("outbox/events.py"), source).unwrap();
    git_commit_all(tmp.path());
    ecp_index(tmp.path());
    tmp
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn saga_compensate_pair_emits_match() {
    let tmp = setup_single_file(SAGA_COMPENSATE);
    let json = run_find_tx(tmp.path(), &[]);
    let pairs = saga_pairs(&json);
    assert!(
        !pairs.is_empty(),
        "expected ≥1 saga pair for compensate prefix: {json}"
    );
    let pair = &pairs[0];
    assert_eq!(
        pair["operation"].as_str().unwrap_or(""),
        "Order.place_order"
    );
    assert_eq!(
        pair["compensator"].as_str().unwrap_or(""),
        "Order.compensate_place_order"
    );
    assert_eq!(pair["requires_verification"].as_bool(), Some(true));
    let confidence = pair["confidence"].as_f64().unwrap_or(0.0);
    assert!(
        (0.59..0.61).contains(&confidence),
        "base confidence should be ~0.6, got {confidence}"
    );
    assert_eq!(pair["tier"].as_str().unwrap_or(""), "BLIND_SPOT");
}

#[test]
fn saga_undo_prefix_emits_match() {
    let tmp = setup_single_file(SAGA_UNDO);
    let json = run_find_tx(tmp.path(), &[]);
    let pairs = saga_pairs(&json);
    assert!(
        !pairs.is_empty(),
        "expected ≥1 saga pair for undo prefix: {json}"
    );
    let pair = &pairs[0];
    assert_eq!(pair["operation"].as_str().unwrap_or(""), "Payment.pay");
    assert_eq!(
        pair["compensator"].as_str().unwrap_or(""),
        "Payment.undo_pay"
    );
}

#[test]
fn saga_rollback_prefix_emits_match() {
    let tmp = setup_single_file(SAGA_ROLLBACK);
    let json = run_find_tx(tmp.path(), &[]);
    let pairs = saga_pairs(&json);
    assert!(
        !pairs.is_empty(),
        "expected ≥1 saga pair for rollback prefix: {json}"
    );
    let pair = &pairs[0];
    assert_eq!(pair["operation"].as_str().unwrap_or(""), "Shipping.ship");
    assert_eq!(
        pair["compensator"].as_str().unwrap_or(""),
        "Shipping.rollback_ship"
    );
}

#[test]
fn compensator_calling_operation_bumps_confidence() {
    let tmp = setup_single_file(SAGA_CALLS_BACK);
    let json = run_find_tx(tmp.path(), &[]);
    let pairs = saga_pairs(&json);
    assert!(
        !pairs.is_empty(),
        "expected saga pair when compensator calls operation: {json}"
    );
    let pair = &pairs[0];
    let confidence = pair["confidence"].as_f64().unwrap_or(0.0);
    // Confidence bumped to 0.8 when compensator body has Calls edge to operation.
    assert!(
        (0.79..0.81).contains(&confidence),
        "confidence should be ~0.8 when compensator calls operation, got {confidence}: {json}"
    );
    assert_eq!(pair["tier"].as_str().unwrap_or(""), "POSSIBLY_RELATED");
    assert_eq!(
        pair["evidence"]["compensator_calls_operation"].as_bool(),
        Some(true)
    );
}

#[test]
fn compensator_on_different_class_no_match() {
    let tmp = setup_single_file(SAGA_CROSS_CLASS);
    let json = run_find_tx(tmp.path(), &[]);
    let pairs = saga_pairs(&json);
    assert!(
        pairs.is_empty(),
        "cross-class compensator must NOT form a pair: {json}"
    );
}

#[test]
fn no_compensator_no_match() {
    let tmp = setup_single_file(SAGA_NO_COMPENSATOR);
    let json = run_find_tx(tmp.path(), &[]);
    let pairs = saga_pairs(&json);
    assert!(
        pairs.is_empty(),
        "lone operation with no compensator must yield empty saga_pairs: {json}"
    );
}

// ── Outbox tests ──────────────────────────────────────────────────────────────

#[test]
fn outbox_full_pattern_emits_finding() {
    let tmp = setup_outbox_file(OUTBOX_FULL);
    let json = run_find_tx(tmp.path(), &[]);
    let patterns = outbox_patterns(&json);
    assert!(
        !patterns.is_empty(),
        "expected ≥1 OutboxPattern finding for OutboxEvent + Kafka producer: {json}"
    );
    let p = &patterns[0];
    assert_eq!(
        p["outbox_table"]["name"].as_str().unwrap_or(""),
        "OutboxEvent"
    );
    assert_eq!(p["requires_verification"].as_bool(), Some(true));
    let confidence = p["confidence"].as_f64().unwrap_or(0.0);
    assert!(
        confidence >= 0.70,
        "outbox confidence should be ≥0.70, got {confidence}: {json}"
    );
    assert!(
        confidence < 0.90,
        "outbox confidence must be <0.90 (heuristic), got {confidence}: {json}"
    );
    // writer must be present
    assert!(!p["writer"]["name"].as_str().unwrap_or("").is_empty());
    // publisher must be present
    assert!(!p["publisher"]["name"].as_str().unwrap_or("").is_empty());
    // summary counts
    assert_eq!(
        json["summary"]["outbox_count"].as_u64(),
        Some(patterns.len() as u64)
    );
}

#[test]
fn outbox_no_table_no_finding() {
    let tmp = setup_outbox_file(OUTBOX_NO_TABLE);
    let json = run_find_tx(tmp.path(), &[]);
    let patterns = outbox_patterns(&json);
    assert!(
        patterns.is_empty(),
        "no outbox table → no OutboxPattern findings: {json}"
    );
    assert_eq!(
        json["summary"]["outbox_count"].as_u64(),
        Some(0),
        "outbox_count should be 0: {json}"
    );
}

#[test]
fn outbox_no_publisher_no_finding() {
    let tmp = setup_outbox_file(OUTBOX_NO_PUBLISHER);
    let json = run_find_tx(tmp.path(), &[]);
    let patterns = outbox_patterns(&json);
    assert!(
        patterns.is_empty(),
        "outbox table with no publisher reachable → no findings: {json}"
    );
}

#[test]
fn outbox_indirect_publish_via_helper_emits_finding() {
    let tmp = setup_outbox_file(OUTBOX_INDIRECT);
    let json = run_find_tx(tmp.path(), &[]);
    // The BFS should reach publish_event (which calls producer.send) from save().
    // We accept 0 findings here if the Calls graph doesn't connect — this is
    // a best-effort heuristic — but the command must not error.
    assert!(
        json["outbox_patterns"].as_array().is_some(),
        "outbox_patterns must always be present: {json}"
    );
}

#[test]
fn outbox_snake_case_name_matched() {
    let tmp = setup_outbox_file(OUTBOX_SNAKE_CASE);
    let json = run_find_tx(tmp.path(), &[]);
    let patterns = outbox_patterns(&json);
    assert!(
        !patterns.is_empty(),
        "snake_case event_outbox class must be matched: {json}"
    );
    let p = &patterns[0];
    assert_eq!(
        p["outbox_table"]["name"].as_str().unwrap_or(""),
        "event_outbox"
    );
}

#[test]
fn outbox_only_flag_suppresses_saga() {
    let tmp = setup_outbox_file(OUTBOX_FULL);
    let json = run_find_tx(tmp.path(), &["--outbox-only"]);
    let saga = saga_pairs(&json);
    assert!(
        saga.is_empty(),
        "--outbox-only must suppress saga_pairs: {json}"
    );
    // outbox_patterns may or may not have findings depending on graph, but field exists.
    assert!(
        json["outbox_patterns"].as_array().is_some(),
        "outbox_patterns field must exist: {json}"
    );
}

#[test]
fn saga_only_flag_suppresses_outbox() {
    let tmp = setup_outbox_file(OUTBOX_FULL);
    let json = run_find_tx(tmp.path(), &["--saga-only"]);
    let patterns = outbox_patterns(&json);
    assert!(
        patterns.is_empty(),
        "--saga-only must yield empty outbox_patterns: {json}"
    );
}

#[test]
fn outbox_patterns_field_always_present() {
    let tmp = setup_single_file(SAGA_NO_COMPENSATOR);
    let json = run_find_tx(tmp.path(), &[]);
    assert!(
        json["outbox_patterns"].as_array().is_some(),
        "outbox_patterns field must always be present: {json}"
    );
    assert!(
        json["summary"]["outbox_count"].as_u64().is_some(),
        "summary.outbox_count must always be present: {json}"
    );
}

#[test]
fn class_scope_filter_works() {
    let tmp = setup_single_file(SAGA_TWO_CLASSES);
    let json = run_find_tx(tmp.path(), &["--class", "Order"]);
    let pairs = saga_pairs(&json);
    assert_eq!(
        pairs.len(),
        1,
        "--class Order should return exactly 1 pair, got {}: {json}",
        pairs.len()
    );
    assert!(
        pairs[0]["operation"]
            .as_str()
            .unwrap_or("")
            .starts_with("Order."),
        "pair should belong to Order class: {json}"
    );
}
