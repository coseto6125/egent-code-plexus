//! Pin the contract that `ecp find-event-mirrors` exposes when low-confidence
//! mirror edges are filtered by `--min-confidence`.
//!
//! T5-33 emits EventTopicMirror edges at confidence=0.85. Raising the threshold
//! above 0.85 must not silently drop rows — the payload must carry `filtered_out`
//! (count of mirrors dropped) and `threshold_used` (the effective threshold).

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn write(repo: &Path, rel: &str, body: &str) {
    let full = repo.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

fn init_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

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

// Redis publish/subscribe on "orders" — T5-33 emits EventTopicMirror at confidence=0.85.
const PUBLISHER: &str = r#"
import redis

def publish_order(r, data):
    r.publish("orders", data)
"#;

const SUBSCRIBER: &str = r#"
import redis

def consume_order(pubsub):
    pubsub.subscribe("orders")
"#;

#[test]
fn high_threshold_surfaces_filtered_out_and_threshold_used() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // One publisher + one subscriber on "orders" → one EventTopicMirror at 0.85.
    write(repo, "svc/publisher.py", PUBLISHER);
    write(repo, "svc/subscriber.py", SUBSCRIBER);
    init_and_analyze(repo);

    // Raise threshold above the emitted confidence (0.85) → mirror is filtered.
    let out = Command::new(ecp_bin())
        .args([
            "find-event-mirrors",
            "--min-confidence",
            "0.9",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("find-event-mirrors failed to spawn");
    assert!(
        out.status.success(),
        "find-event-mirrors failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
    let result: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("invalid JSON: {e}\nstdout={stdout}"));

    let filtered_out = result["filtered_out"]
        .as_u64()
        .expect("payload must carry `filtered_out`");
    let threshold_used = result["threshold_used"]
        .as_f64()
        .expect("payload must carry `threshold_used`");

    assert!(
        filtered_out >= 1,
        "`filtered_out` must be >= 1 when threshold (0.9) exceeds emitted confidence (0.85): result={result}"
    );
    assert!(
        (threshold_used - 0.9_f64).abs() < 1e-3,
        "`threshold_used` must be ≈0.9, got {threshold_used}: result={result}"
    );

    // Mirrors array must be empty (all filtered).
    let mirrors_len = result["mirrors"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(
        mirrors_len, 0,
        "mirrors should be empty when all are filtered: result={result}"
    );
}
