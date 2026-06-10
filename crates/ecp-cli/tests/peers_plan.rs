//! `ecp peers plan` — pre-spawn disjointness query for agent-team leads.
//!
//! Fixture call graph (TS):
//!   alpha --calls--> shared <--calls-- beta     (alpha/beta collide via shared)
//!   delta --calls--> gamma                      (own cluster)
//!   omega                                       (isolated)

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

const SOURCE: &str = r#"
export function shared(): number { return 1; }
export function alpha(): number { return shared(); }
export function beta(): number { return shared(); }
export function gamma(): number { return 2; }
export function delta(): number { return gamma(); }
export function omega(): number { return 3; }
"#;

fn init_repo_and_index(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.ts"), SOURCE).unwrap();
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

fn run_plan_json(repo: &Path, targets: &str) -> Value {
    let out = Command::new(ecp_bin())
        .args(["peers", "plan", "--targets", targets, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("peers plan failed to spawn");
    assert!(
        out.status.success(),
        "peers plan failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("bad JSON ({e}): {stdout}"))
}

#[test]
fn plan_clusters_overlapping_targets_and_lists_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_and_index(dir.path());

    let v = run_plan_json(dir.path(), "alpha,beta,delta,omega,ghost");

    // unresolved listed honestly, never guessed into a cluster
    let unresolved: Vec<&str> = v["unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert_eq!(unresolved, vec!["ghost"], "payload: {v}");

    // alpha/beta collide via `shared`
    let overlaps = v["overlaps"].as_array().unwrap();
    let ab = overlaps
        .iter()
        .find(|o| {
            let (a, b) = (o["a"].as_str().unwrap(), o["b"].as_str().unwrap());
            (a == "alpha" && b == "beta") || (a == "beta" && b == "alpha")
        })
        .unwrap_or_else(|| panic!("alpha/beta overlap missing: {v}"));
    assert!(ab["shared_count"].as_u64().unwrap() >= 1);
    let sample: Vec<&str> = ab["sample"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert!(
        sample.iter().any(|s| s.contains("shared")),
        "sample should name the shared symbol: {v}"
    );

    // clusters: {alpha,beta} merged; delta and omega disjoint singletons
    let clusters: Vec<Vec<String>> = v["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let mut names: Vec<String> = c
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect();
            names.sort();
            names
        })
        .collect();
    assert_eq!(clusters.len(), 3, "expected 3 work packages: {v}");
    assert!(
        clusters.iter().any(|c| c == &["alpha", "beta"]),
        "alpha+beta must land in one package: {v}"
    );
    assert!(clusters.iter().any(|c| c == &["delta"]), "{v}");
    assert!(clusters.iter().any(|c| c == &["omega"]), "{v}");

    // impact suppression lower-bound caveat must be present
    assert!(
        v["caveat"].as_str().unwrap_or("").contains("lower bound"),
        "caveat missing: {v}"
    );
}

#[test]
fn plan_all_disjoint_yields_singleton_clusters_text() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_and_index(dir.path());

    let out = Command::new(ecp_bin())
        .args(["peers", "plan", "--targets", "delta,omega"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("peers plan failed to spawn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no overlaps"),
        "disjoint targets should say so plainly: {stdout}"
    );
}
