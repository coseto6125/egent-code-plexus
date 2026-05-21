//! Integration tests for `ecp find-transaction-patterns` (Saga half).
//!
//! Each test builds a minimal in-memory repo, indexes it via `ecp admin index`,
//! then runs the CLI and asserts on the JSON output.  All fixtures are pure
//! Python so we only need one language to exercise the compensator detection
//! logic (the algorithm is language-agnostic).

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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn saga_pairs(json: &Value) -> &[Value] {
    json["saga_pairs"]
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

#[test]
fn outbox_status_blocked_field_present() {
    let tmp = setup_single_file(SAGA_NO_COMPENSATOR);
    let json = run_find_tx(tmp.path(), &[]);
    // outbox_patterns must always be present (empty array).
    let outbox = json["outbox_patterns"].as_array();
    assert!(
        outbox.is_some(),
        "outbox_patterns field must always be present: {json}"
    );
    assert!(
        outbox.unwrap().is_empty(),
        "outbox_patterns must be empty (blocked on T5-33): {json}"
    );
    // summary.outbox_status must carry the blocker token.
    let status = json["summary"]["outbox_status"].as_str().unwrap_or("");
    assert_eq!(
        status, "blocked_on_t5_33",
        "outbox_status should be 'blocked_on_t5_33': {json}"
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
