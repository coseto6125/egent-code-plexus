//! What the `ecp impact` binary prints for the evidence of one query: an
//! ambiguous target lists its candidates ranked by callers (and exits
//! non-zero), and a 0-caller target's stderr hint cites the text scan.
//! Synthetic graphs through `--graph`, over a real git work tree as cwd.

use crate::common;

use common::{ecp_bin, run_git, write};
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use serde_json::Value;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fixture {
    repo: tempfile::TempDir,
    _graph_dir: tempfile::TempDir,
    graph: PathBuf,
}

impl Fixture {
    fn new(fx: GraphFixture, repo: tempfile::TempDir) -> Self {
        run_git(repo.path(), &["init", "-q"]);
        run_git(repo.path(), &["add", "-A"]);
        let graph_dir = tempfile::tempdir().unwrap();
        let graph = graph_dir.path().join("graph.bin");
        std::fs::write(&graph, fx.into_bytes()).unwrap();
        Self {
            repo,
            _graph_dir: graph_dir,
            graph,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(ecp_bin());
        cmd.arg("--graph")
            .arg(&self.graph)
            .arg("impact")
            .args(args)
            .current_dir(self.repo.path())
            // A HOME that equals a commitless work tree is refused as a
            // folder of projects, so HOME is the graph's own temp dir.
            .env("HOME", self._graph_dir.path())
            .env("ECP_SKIP_BG_REBUILD", "1")
            .env_remove("CLAUDE_CODE_SESSION_ID");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .output()
            .expect("ecp impact failed to spawn")
    }
}

fn json_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout must be one JSON document ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// One `get` definition per `(path, callers)`, each with that many callers
/// `c<k>_<i>` in `src/c<k>.py` at lines 1, 3, 5, ...
fn candidates(defs: &[(&str, usize)]) -> Fixture {
    let repo = tempfile::tempdir().unwrap();
    let mut fx = GraphFixture::new();
    for (k, &(path, callers)) in defs.iter().enumerate() {
        write(repo.path(), path, "def get():\n    pass\n");
        let def = fx.func(path, "get");
        fx.span(def, (0, 0, 1, 8));
        let caller_file = format!("src/c{k}.py");
        for i in 0..callers as u32 {
            let caller = fx.func(&caller_file, &format!("c{k}_{i}"));
            fx.span(caller, (i * 2, 0, i * 2 + 1, 9));
            fx.edge_with(caller, def, RelType::Calls, 1.0, "call");
        }
    }
    Fixture::new(fx, repo)
}

fn ambiguous_json(fixture: &Fixture) -> Value {
    let out = fixture.run(&["get", "--format", "json"]);
    assert!(!out.status.success(), "an ambiguous target exits non-zero");
    let json = json_of(&out);
    assert_eq!(json["status"], "ambiguous", "{json}");
    json
}

fn files_and_callers(json: &Value) -> Vec<(String, u64)> {
    json["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates array expected: {json}"))
        .iter()
        .map(|c| {
            (
                c["file"].as_str().unwrap().to_string(),
                c["callers"].as_u64().unwrap(),
            )
        })
        .collect()
}

// ── I2: ambiguous names list candidates with evidence ──────────────────────

#[test]
fn test_impact_ambiguous_two_candidates_ranked_with_evidence() {
    let fixture = candidates(&[("src/a.py", 0), ("src/b.py", 4)]);
    let json = ambiguous_json(&fixture);
    assert_eq!(
        files_and_callers(&json),
        vec![("src/b.py".to_string(), 4), ("src/a.py".to_string(), 0)],
        "most-called first: {json}"
    );
    let first = &json["candidates"][0];
    assert!(first["target"].as_str().is_some(), "{json}");
    assert_eq!(first["line"], 1);
    assert_eq!(first["kind"], "Function");
    assert_eq!(
        first["top_callers"],
        serde_json::json!(["c1_0 src/c1.py:1", "c1_1 src/c1.py:3", "c1_2 src/c1.py:5"]),
        "at most 3, `name file:line`: {json}"
    );
    assert_eq!(
        json["candidates"][1]["top_callers"],
        serde_json::json!([]),
        "an array even when empty: {json}"
    );
    assert!(json.get("more").is_none(), "{json}");
    assert!(json.get("more_command").is_none(), "{json}");
}

#[test]
fn test_impact_ambiguous_seven_candidates_capped_at_five_with_more() {
    let fixture = candidates(&[
        ("src/d0.py", 0),
        ("src/d1.py", 3),
        ("src/d2.py", 1),
        ("src/d3.py", 5),
        ("src/d4.py", 2),
        ("src/d5.py", 4),
        ("src/d6.py", 6),
    ]);
    let json = ambiguous_json(&fixture);
    let callers: Vec<u64> = files_and_callers(&json).into_iter().map(|c| c.1).collect();
    assert_eq!(callers, vec![6, 5, 4, 3, 2], "{json}");
    assert_eq!(json["more"], 2, "{json}");
    assert_eq!(json["more_command"], "ecp find get --all", "{json}");
}

#[test]
fn test_impact_ambiguous_vendored_candidates_dropped_and_counted_in_more() {
    let fixture = candidates(&[
        ("src/a.py", 1),
        (".venv/lib/site-packages/m/get.py", 9),
        ("src/b.py", 0),
    ]);
    let json = ambiguous_json(&fixture);
    assert_eq!(
        files_and_callers(&json),
        vec![("src/a.py".to_string(), 1), ("src/b.py".to_string(), 0)],
        "{json}"
    );
    assert_eq!(json["more"], 1, "{json}");
}

#[test]
fn test_impact_ambiguous_all_candidates_vendored_kept() {
    let fixture = candidates(&[("node_modules/x/get.py", 0), ("vendor/y/get.py", 2)]);
    let json = ambiguous_json(&fixture);
    assert_eq!(
        files_and_callers(&json),
        vec![
            ("vendor/y/get.py".to_string(), 2),
            ("node_modules/x/get.py".to_string(), 0)
        ],
        "{json}"
    );
}

#[test]
fn test_impact_ambiguous_llm_format_lists_candidates_and_exits_non_zero() {
    let fixture = candidates(&[("src/a.py", 0), ("src/b.py", 1)]);
    let out = fixture.run(&["get"]);
    assert!(!out.status.success(), "an ambiguous target exits non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ambiguous"), "{stdout}");
    assert!(stdout.contains("src/b.py"), "{stdout}");
    assert!(stdout.contains("c1_0 src/c1.py:1"), "{stdout}");
    assert!(stdout.contains("callers"), "{stdout}");
}

// ── I1: the stderr conclusion cites the scan ────────────────────────────────

fn zero_caller_fixture(use_body: &str) -> Fixture {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "src/lib.py", "def get():\n    pass\n");
    write(repo.path(), "src/use.py", use_body);
    let mut fx = GraphFixture::new();
    let def = fx.func("src/lib.py", "get");
    fx.span(def, (0, 0, 1, 8));
    Fixture::new(fx, repo)
}

#[test]
fn test_impact_zero_callers_hint_cites_unexplained_total() {
    let fixture = zero_caller_fixture("A = get\nB = [get]\n");
    let out = fixture.run(&["get", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = json_of(&out);
    assert_eq!(json["unexplained_refs"]["total"], 2, "{json}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("2 text references the graph does not explain"),
        "{stderr}"
    );
    assert!(!stderr.contains("dead code, or recent rename"), "{stderr}");
}

#[test]
fn test_impact_zero_callers_scan_found_nothing_hint_says_so() {
    let fixture = zero_caller_fixture("x = 1\n");
    let out = fixture.run(&["get", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_of(&out)["unexplained_refs"]["total"], 0);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no text reference outside its definition"),
        "{stderr}"
    );
}

// ── `--batch` and concurrency ───────────────────────────────────────────────

#[test]
fn test_impact_batch_zero_callers_no_scan_as_before() {
    let fixture = zero_caller_fixture("A = get\n");
    let mut child = fixture
        .command(&["--batch", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("impact --batch failed to spawn");
    child.stdin.take().unwrap().write_all(b"get\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let body = stdout
        .strip_prefix("=== target: get ===\n")
        .unwrap_or_else(|| panic!("block header: {stdout}"));
    let json: Value = serde_json::from_str(body.trim()).unwrap();
    assert!(
        json.get("unexplained_refs").is_none(),
        "--batch keeps the opt-in scan: {json}"
    );
}

#[test]
fn test_impact_two_concurrent_calls_on_one_repo_same_answer() {
    let fixture = zero_caller_fixture("A = get\n");
    let spawn = || {
        fixture
            .command(&["get", "--format", "json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("impact failed to spawn")
    };
    let (a, b) = (spawn(), spawn());
    let (a, b) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    assert!(a.status.success() && b.status.success());
    assert_eq!(json_of(&a), json_of(&b));
    assert_eq!(json_of(&a)["unexplained_refs"]["total"], 1);
}
