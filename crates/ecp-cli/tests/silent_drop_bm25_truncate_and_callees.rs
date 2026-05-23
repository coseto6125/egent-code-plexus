//! Pin the BM25-mode payload contracts for two silent-drop sites:
//!
//! * H1 — substring fallback caps at `MULTI_CAP` (100). The payload must
//!   always carry `bm25_pre_truncate_total: u64` so the LLM can tell when
//!   the result list is partial. Default safe value is the real count
//!   when no cap fired.
//! * H2 — every BM25 Hit must carry `callee_count: u64` alongside
//!   `caller_count`. The `callees` list is truncated at
//!   `HOP_EXPANSION_LIMIT` for token budget; without `callee_count` an
//!   LLM reading the list cannot tell whether the symbol has 3 callees
//!   or 300.
//!
//! One small repo proves both contracts at once.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use serde_json::Value;
use std::process::Command;

#[test]
fn bm25_payload_carries_truncate_metadata_and_callee_count() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // A small chain: `entry` calls `helper`, `helper` calls `inner`. The
    // BM25 hit for `helper` must report callee_count == 1 (calls `inner`)
    // even though the `callees` list is also truncated at HOP_EXPANSION_LIMIT.
    write(
        repo,
        "src/lib.rs",
        r#"pub fn entry() { helper(); }
pub fn helper() { inner(); }
pub fn inner() {}
"#,
    );
    init_and_analyze(repo);

    let out = Command::new(ecp_bin())
        .args([
            "find", "helper", "--mode", "bm25", "--format", "json", "--repo", ".",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp find failed to spawn");
    assert!(
        out.status.success(),
        "ecp find failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result: Value =
        serde_json::from_slice(&out.stdout).expect("ecp find produced non-JSON output");

    // H1: payload always carries bm25_pre_truncate_total. Type-check it
    // exists and is a number; the actual cap doesn't fire on this tiny
    // repo so the value is just the raw scan count.
    let pre_total = result["bm25_pre_truncate_total"]
        .as_u64()
        .expect("payload must carry `bm25_pre_truncate_total` (u64)");
    assert!(
        pre_total < 1_000_000,
        "bm25_pre_truncate_total looks corrupt: {pre_total}"
    );

    // H2: every hit (across all buckets) must expose callee_count. Walk
    // the source bucket — that's where `helper` lives.
    let source_hits = result["source"]
        .as_array()
        .expect("payload must carry `source` array");
    let helper_hit = source_hits
        .iter()
        .find(|h| h["name"].as_str() == Some("helper"))
        .unwrap_or_else(|| panic!("expected `helper` hit in source bucket: {result}"));

    let callee_count = helper_hit["callee_count"]
        .as_u64()
        .expect("hit must carry `callee_count`");
    let caller_count = helper_hit["caller_count"]
        .as_u64()
        .expect("hit must carry `caller_count`");

    assert!(
        callee_count >= 1,
        "helper calls inner — expected callee_count >= 1, got {callee_count}: {helper_hit}"
    );
    assert!(
        caller_count >= 1,
        "helper is called by entry — expected caller_count >= 1, got {caller_count}: {helper_hit}"
    );
}
