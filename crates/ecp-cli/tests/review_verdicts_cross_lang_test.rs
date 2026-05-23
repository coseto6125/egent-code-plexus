//! Verify cross-language ripple: when a backend Route's handler relocates
//! and a frontend file fetches that route, `ecp review --verdicts` should
//! emit a `ROUTE_CONTRACT_CHANGED` verdict at RISK severity with the
//! consumer file attached as `cross_callers`.

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Backend v1: `POST /api/orders` handler at line 4.
const BACKEND_V1: &str = r#"
import express from "express";
const app = express();
app.post('/api/orders', (req, res) => res.json({ id: 1 }));
"#;

/// Backend v2: same route, handler moved down (extra middleware above).
const BACKEND_V2: &str = r#"
import express from "express";
const app = express();
app.use((req, res, next) => next()); // new middleware shifts handler line
app.post('/api/orders', (req, res) => res.json({ id: 1 }));
"#;

/// Frontend consumer — literal-URL fetch the resolver can match to a Route.
const FRONTEND: &str = r#"
export async function createOrder(payload) {
  const res = await fetch('/api/orders', {
    method: 'POST',
    body: JSON.stringify(payload),
  });
  return res.json();
}
"#;

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(repo: &std::path::Path, msg: &str) {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

#[test]
#[ignore = "FU-2026-05-23-013: WARN→RISK escalation needs Fetches edge to handler; route handler match incomplete in fixture"]
fn cross_lang_ripple_escalates_modified_route_to_risk() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir(repo.join("server")).unwrap();
    std::fs::create_dir(repo.join("web")).unwrap();

    // v1: backend route + frontend fetch in same baseline.
    std::fs::write(repo.join("server/routes.ts"), BACKEND_V1).unwrap();
    std::fs::write(repo.join("web/orders.ts"), FRONTEND).unwrap();
    commit(repo, "v1");

    let baseline_sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // v2: backend handler shifts line; frontend unchanged.
    std::fs::write(repo.join("server/routes.ts"), BACKEND_V2).unwrap();
    commit(repo, "v2");

    let output = Command::new(ecp_bin())
        .args([
            "review",
            "--since",
            &baseline_sha,
            "--verdicts",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp review --verdicts");

    assert!(
        output.status.success(),
        "review verdicts failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}; stdout: {stdout}"));

    let verdicts = parsed["verdicts"]
        .as_array()
        .expect("verdicts must be array");

    // Find the ROUTE_CONTRACT_CHANGED verdict for POST /api/orders.
    let route_verdict = verdicts
        .iter()
        .find(|v| {
            v["kind"].as_str() == Some("ROUTE_CONTRACT_CHANGED")
                && v["symbol"].as_str() == Some("POST /api/orders")
        })
        .unwrap_or_else(|| {
            panic!("expected ROUTE_CONTRACT_CHANGED for POST /api/orders; got: {verdicts:#?}")
        });

    // The Fetches edge is statically resolvable → severity must escalate
    // from the baseline Warn (handler relocation) to Risk.
    assert_eq!(
        route_verdict["severity"].as_str(),
        Some("RISK"),
        "modified route with literal-URL consumer must be RISK; got: {}",
        route_verdict["severity"]
    );

    // cross_callers must contain the consumer file path.
    let cross = route_verdict["cross_callers"]
        .as_array()
        .expect("cross_callers must be present and non-null");
    assert!(
        cross.iter().any(|c| c["path"]
            .as_str()
            .map(|p| p.contains("web/orders.ts"))
            .unwrap_or(false)),
        "expected web/orders.ts in cross_callers; got: {cross:?}"
    );
}
