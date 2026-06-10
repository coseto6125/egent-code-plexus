//! Regression: `cargo test` must not append rows to the developer's live
//! `~/.ecp/telemetry/` store. Tests spawn the ecp binary, which calls
//! `telemetry_cli::record()` on every invocation (including failures);
//! without `ECP_NO_TELEMETRY=1` in the test environment those failures
//! inflate `ecp usage` error stats indefinitely.
//!
//! The fix lives in `.cargo/config.toml` `[env]` — this test verifies the
//! suppression end-to-end: even an invocation against a bogus `--graph` path
//! must write zero rows to the controlled telemetry store.

use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Spawn ecp with a nonexistent `--graph` (guaranteed failure path that
/// reaches the telemetry `record()` call), with `HOME` and `ECP_HOME`
/// redirected to a controlled temp dir, and assert no telemetry row is
/// written. The test does NOT clear `ECP_NO_TELEMETRY` — the inherited value
/// from the cargo test runner is what we want to verify is effective.
#[test]
fn failed_invocation_writes_no_telemetry_under_test() {
    let tmp = std::env::temp_dir().join(format!("ecp-tel-isolation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let bogus_graph = tmp.join("definitely-not-a-graph.bin");
    // guarantee the file does not exist
    let _ = std::fs::remove_file(&bogus_graph);

    let out = Command::new(ecp_bin())
        .args(["find", "anything", "--graph", bogus_graph.to_str().unwrap()])
        .current_dir(&tmp)
        .env("HOME", &tmp)
        .env("ECP_HOME", tmp.join(".ecp"))
        // Do NOT call env_remove("ECP_NO_TELEMETRY") — we deliberately
        // inherit the value set by .cargo/config.toml.
        .output()
        .expect("ecp spawn failed");

    // Command itself should fail (no graph), but that is not the assertion.
    let _ = out;

    let tel_root = tmp.join(".ecp/telemetry");
    let row_count: usize = if let Ok(entries) = std::fs::read_dir(&tel_root) {
        entries
            .flatten()
            .map(|e| {
                let f = e.path().join("cli-calls.jsonl");
                if f.exists() {
                    std::fs::read_to_string(&f)
                        .map(|s| s.lines().count())
                        .unwrap_or(0)
                } else {
                    0
                }
            })
            .sum()
    } else {
        0
    };

    assert_eq!(
        row_count, 0,
        "ECP_NO_TELEMETRY=1 must suppress all telemetry rows; \
         found {row_count} row(s) under {tel_root:?}. \
         Likely cause: ECP_NO_TELEMETRY is not set in .cargo/config.toml [env]."
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
