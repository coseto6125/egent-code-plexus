//! `ecp contracts` reads the graph's route record (architecture direction,
//! area 8): Route nodes are producers and Fetches edges are consumers.
//! Before this change `extract_contracts_for_repo` returned empty lists as a
//! success, so `--repo @all` printed `pairs: []` with no caveat.

mod common;

use common::{commit_all, ecp_bin, run_git};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Several indexed git repos that share one registry.
struct World {
    tmp: tempfile::TempDir,
    home: PathBuf,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        Self { tmp, home }
    }

    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = self.tmp.path().join(name);
        fs::create_dir_all(&root).unwrap();
        for (rel, text) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        run_git(&root, &["init", "-q", "-b", "main"]);
        commit_all(&root, "base");
        let out = self.ecp(&root, &["admin", "index", "--repo", "."]);
        assert!(out.status.success(), "admin index {name}: {}", stderr(&out));
        root
    }

    fn ecp(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(ecp_bin())
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "contracts-record")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }

    /// `ecp contracts --format json` over `repo_arg`, run from `cwd`.
    fn contracts(&self, cwd: &Path, repo_arg: &str, extra: &[&str]) -> Output {
        let mut args = vec!["contracts", "--repo", repo_arg, "--format", "json"];
        args.extend_from_slice(extra);
        self.ecp(cwd, &args)
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json_of(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let start = text
        .find('{')
        .unwrap_or_else(|| panic!("no JSON on stdout: {text}\nstderr: {}", stderr(out)));
    serde_json::Deserializer::from_str(&text[start..])
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap()
}

const SERVER_JS: &str = "const express = require('express');\nconst app = express();\nfunction listUsers(req, res) { res.json([{ id: 1 }]); }\napp.get('/api/users', listUsers);\napp.listen(3000);\n";
const CLIENT_JS: &str = "async function load() {\n  const res = await fetch('/api/users');\n  return res.json();\n}\nmodule.exports = { load };\n";
const LIB_PY: &str = "def parse():\n    return 1\n";

/// Stands for: before the fix this command printed `pairs: []` as a success
/// for repos it never read. Two repos with no route now fail with the cause.
#[test]
fn test_contracts_all_repos_unimplemented_fails_not_empty() {
    let w = World::new();
    let a = w.repo("alpha", &[("lib.py", LIB_PY)]);
    let b = w.repo("beta", &[("lib.py", LIB_PY)]);
    let out = w.contracts(&a, &format!("{},{}", a.display(), b.display()), &[]);
    assert!(
        !out.status.success(),
        "empty extraction must not succeed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr(&out).contains("no route contract extracted"),
        "{}",
        stderr(&out)
    );
}

/// Stands for: `queue` and `rpc` have no extractor, so asking for them alone
/// is an error that says so, and never an empty list.
#[test]
fn test_contracts_kind_queue_reports_not_implemented() {
    let w = World::new();
    let a = w.repo(
        "alpha",
        &[("server.js", SERVER_JS), ("client.js", CLIENT_JS)],
    );
    let out = w.contracts(&a, ".", &["--kind", "queue"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("not implemented"), "{}", stderr(&out));
}

/// Stands for: one Express route `GET /api/users` and one `fetch("/api/users")`
/// in the same repo are one contract pair, read from the Route node and the
/// Fetches edge.
#[test]
fn test_contracts_express_route_and_fetch_pair() {
    let w = World::new();
    let a = w.repo(
        "alpha",
        &[("server.js", SERVER_JS), ("client.js", CLIENT_JS)],
    );
    let out = w.contracts(&a, ".", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_of(&out);
    let pairs = v["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 1, "{v}");
    let pair = &pairs[0];
    assert_eq!(pair["kind"], "route");
    assert_eq!(pair["method"], "GET");
    assert_eq!(pair["path"], "/api/users");
    assert_eq!(pair["scope"], "same_repo");
    assert_eq!(pair["producer"]["file"], "server.js");
    assert_eq!(pair["consumer"]["file"], "client.js");
    assert_eq!(v["unmatched_producer_count"], 0);
    assert_eq!(v["unmatched_consumer_count"], 0);
    let caveats = v["caveats"].as_array().unwrap();
    assert!(
        caveats
            .iter()
            .any(|c| c.as_str().unwrap().contains("Fetches")),
        "the consumer limit must be stated: {v}"
    );
}

/// Stands for: a client in repo `web` and a route in repo `api` meet on the
/// same verb and path. `web` also declares the route, so its `Fetches` edge
/// exists; the pair with `api` is `cross_repo`.
#[test]
fn test_contracts_two_repos_pair_across_repos() {
    let w = World::new();
    let api = w.repo("api", &[("server.js", SERVER_JS)]);
    let web = w.repo(
        "web",
        &[
            ("client.js", CLIENT_JS),
            (
                "mock.js",
                "const app = require('express')();\napp.get('/api/users', () => {});\n",
            ),
        ],
    );
    let out = w.contracts(&web, &format!("{},{}", api.display(), web.display()), &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_of(&out);
    assert_eq!(v["repos_scanned"], 2);
    let cross: Vec<&Value> = v["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["scope"] == "cross_repo")
        .collect();
    assert_eq!(cross.len(), 1, "{v}");
    assert!(cross[0]["producer"]["repo"]
        .as_str()
        .unwrap()
        .starts_with("api"));
    assert!(cross[0]["consumer"]["repo"]
        .as_str()
        .unwrap()
        .starts_with("web"));
}

/// Stands for: a route declared with the `/` path (an empty prefix) is a
/// producer with path `/`, and without a client it lands in the unmatched list.
#[test]
fn test_contracts_root_route_listed_as_unmatched_producer() {
    let w = World::new();
    let a = w.repo(
        "alpha",
        &[(
            "server.js",
            "const app = require('express')();\napp.get('/', home);\n",
        )],
    );
    let out = w.contracts(&a, ".", &["--unmatched-only"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_of(&out);
    let producers = v["unmatched_producers"].as_array().unwrap();
    assert!(
        producers
            .iter()
            .any(|p| p["path"] == "/" && p["method"] == "GET"),
        "{v}"
    );
    assert!(v["unmatched_consumers"].as_array().unwrap().is_empty());
}

/// Stands for: a repo whose graph cannot be read is a per-repo error, and the
/// other repo's contracts still print.
#[test]
fn test_contracts_unreadable_graph_is_per_repo_error() {
    let w = World::new();
    let good = w.repo(
        "good",
        &[("server.js", SERVER_JS), ("client.js", CLIENT_JS)],
    );
    let bad = w.repo("bad", &[("server.js", SERVER_JS), ("client.js", CLIENT_JS)]);
    let ecp_dir = w.home.join(".ecp");
    let mut broken = 0;
    for entry in fs::read_dir(&ecp_dir).unwrap().flatten() {
        if !entry.file_name().to_string_lossy().starts_with("bad") {
            continue;
        }
        let commits = entry.path().join("commits");
        for commit in fs::read_dir(&commits).unwrap().flatten() {
            let graph = commit.path().join("graph.bin");
            if graph.exists() {
                fs::write(&graph, b"not a graph").unwrap();
                broken += 1;
            }
        }
    }
    assert!(broken > 0, "no graph.bin found under {}", ecp_dir.display());
    let out = w.contracts(&good, &format!("{},{}", good.display(), bad.display()), &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_of(&out);
    let errors = v["repo_errors"].as_array().expect("repo_errors");
    assert!(
        errors
            .iter()
            .any(|e| e["repo"].as_str().unwrap().starts_with("bad")),
        "{v}"
    );
    assert_eq!(v["pairs"].as_array().unwrap().len(), 1, "{v}");
}

/// Stands for: a repo name that is not in the registry fails the run with the
/// name in the message, instead of a silent partial result.
#[test]
fn test_contracts_absent_repo_in_list_fails_naming_it() {
    let w = World::new();
    let a = w.repo(
        "alpha",
        &[("server.js", SERVER_JS), ("client.js", CLIENT_JS)],
    );
    let out = w.contracts(&a, &format!("{},no-such-repo", a.display()), &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no-such-repo"), "{}", stderr(&out));
}

/// Stands for: a whitespace-only `--repo` never yields an empty success.
#[test]
fn test_contracts_whitespace_repo_never_empty_success() {
    let w = World::new();
    let a = w.repo("alpha", &[("lib.py", LIB_PY)]);
    let out = w.contracts(&a, "   ", &[]);
    assert!(
        !out.status.success(),
        "a repo with no routes must fail, not print pairs: []: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Stands for: the `Fetches` edge starts at the file, so N identical calls in
/// one file are one consumer, one pair, and the consumer carries no `line`.
#[test]
fn test_contracts_same_fetch_twice_in_one_file_is_one_pair_without_line() {
    let w = World::new();
    let twice = "async function a() { return fetch('/api/users'); }\nasync function b() { return fetch('/api/users'); }\n";
    let a = w.repo("alpha", &[("server.js", SERVER_JS), ("client.js", twice)]);
    let out = w.contracts(&a, ".", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_of(&out);
    let pairs = v["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 1, "{v}");
    assert!(pairs[0]["consumer"].get("line").is_none(), "{v}");
    assert!(pairs[0]["producer"]["line"].as_u64().unwrap() > 0, "{v}");
}

/// Stands for: a consumer pairs inside its own repo only with the route its
/// `Fetches` edge names; a sibling route of another verb on the same path stays
/// unmatched, and a repo with no client has no consumer to pair.
#[test]
fn test_contracts_own_repo_consumer_pairs_only_with_its_target_route() {
    let w = World::new();
    let server = "const app = require('express')();\napp.get('/api/users', list);\napp.post('/api/users', make);\napp.all('/api/users', any);\n";
    let a = w.repo("alpha", &[("server.js", server), ("client.js", CLIENT_JS)]);
    let out = w.contracts(&a, ".", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_of(&out);
    let pairs = v["pairs"].as_array().unwrap();
    let from_client: Vec<&Value> = pairs
        .iter()
        .filter(|p| p["consumer"]["file"] == "client.js")
        .collect();
    let lines: std::collections::HashSet<u64> = from_client
        .iter()
        .map(|p| p["producer"]["line"].as_u64().unwrap())
        .collect();
    assert_eq!(
        lines.len(),
        from_client.len(),
        "one consumer, one pair per target route: {v}"
    );
    assert!(
        !from_client.iter().any(|p| p["method"] == "POST"),
        "a GET fetch never pairs with the POST route: {v}"
    );
}
