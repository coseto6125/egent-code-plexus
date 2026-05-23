//! Pin the contract that `ecp routes <path>` exposes the size of its fuzzy
//! fallback when no route matches the wanted path.
//!
//! When no exact match is found, the command scores all routes by similarity
//! and returns at most 5 candidates with score > 0.0. The LLM consumer must
//! be able to tell how many routes were considered vs shown vs silently dropped.
//!
//! Contract: payload always carries `total_fuzzy_candidates`, `shown`, and
//! `zero_score_omitted`, with `shown <= total_fuzzy_candidates`.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use serde_json::Value;
use std::process::Command;

// 6 distinct routes so take(5) triggers (> 5 candidates) and the fuzzy
// fallback has enough material to expose total_fuzzy_candidates vs shown.
const FIXTURE_SRC: &str = r#"
import express from "express";
const app = express();

function h1(req, res) { res.json({}); }
function h2(req, res) { res.json({}); }
function h3(req, res) { res.json({}); }
function h4(req, res) { res.json({}); }
function h5(req, res) { res.json({}); }
function h6(req, res) { res.json({}); }

app.get("/api/users", h1);
app.get("/api/users/profile", h2);
app.get("/api/users/settings", h3);
app.post("/api/users", h4);
app.get("/api/products", h5);
app.get("/api/orders", h6);
"#;

#[test]
fn routes_fallback_exposes_fuzzy_candidate_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    write(repo, "src/main.js", FIXTURE_SRC);
    init_and_analyze(repo);

    // /not-a-real-path matches nothing exactly → triggers fuzzy fallback.
    let out = Command::new(ecp_bin())
        .args([
            "routes",
            "/not-a-real-path",
            "--format",
            "json",
            "--repo",
            ".",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp routes failed to spawn");
    assert!(
        out.status.success(),
        "ecp routes failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("ecp routes returned non-JSON: {stdout}"));
    let result: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"));

    assert_eq!(
        result["status"], "not_found",
        "expected not_found status: {result}"
    );

    let total = result["total_fuzzy_candidates"]
        .as_u64()
        .expect("payload must carry `total_fuzzy_candidates`");
    let shown = result["shown"]
        .as_u64()
        .expect("payload must carry `shown`");
    let zero_omitted = result["zero_score_omitted"]
        .as_u64()
        .expect("payload must carry `zero_score_omitted`");

    assert!(
        total >= 6,
        "expected at least 6 fuzzy candidates (one per route), got total={total}: {result}"
    );
    assert!(
        shown <= 5,
        "take(5) cap must limit shown to ≤5, got shown={shown}: {result}"
    );
    assert!(
        shown <= total,
        "shown ({shown}) must not exceed total_fuzzy_candidates ({total})"
    );
    assert!(
        zero_omitted <= total,
        "zero_score_omitted ({zero_omitted}) must not exceed total ({total})"
    );

    let candidates_len = result["candidates"]
        .as_array()
        .map(|a| a.len() as u64)
        .expect("payload must carry `candidates` array");
    assert_eq!(
        candidates_len, shown,
        "`shown` must equal candidates.len(): result={result}"
    );
}
