//! `ecp path` — the contract, and its behaviour across the 14 mainstream
//! languages.
//!
//! The BFS walks the merged CSR and cannot differ per language. Name
//! resolution and Calls-edge emission can, so a path that works on Python and
//! not on Go is exactly the mixed-stack failure this suite exists to catch.
//! One repo holds every fixture and is indexed once: 14 separate indexes would
//! buy nothing and cost 14x the build.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// `(file name, language label, source)`. Every fixture defines the same
/// three-link chain `<pfx>_entry -> <pfx>_mid -> <pfx>_sink`, so one assertion
/// shape covers all of them.
const FIXTURES: &[(&str, &str, &str)] = &[
    (
        "chain.ts",
        "ts",
        "export function ts_entry(): number { return ts_mid(); }\n\
         function ts_mid(): number { return ts_sink(); }\n\
         function ts_sink(): number { return 1; }\n",
    ),
    (
        "chain.js",
        "js",
        "function js_entry() { return js_mid(); }\n\
         function js_mid() { return js_sink(); }\n\
         function js_sink() { return 1; }\n",
    ),
    (
        "chain.py",
        "py",
        "def py_entry():\n    return py_mid()\n\n\
         def py_mid():\n    return py_sink()\n\n\
         def py_sink():\n    return 1\n",
    ),
    (
        "Chain.java",
        "java",
        "class Chain {\n\
         \x20   static int java_entry() { return java_mid(); }\n\
         \x20   static int java_mid() { return java_sink(); }\n\
         \x20   static int java_sink() { return 1; }\n\
         }\n",
    ),
    (
        "Chain.kt",
        "kt",
        "fun kt_entry(): Int = kt_mid()\n\
         fun kt_mid(): Int = kt_sink()\n\
         fun kt_sink(): Int = 1\n",
    ),
    (
        "Chain.cs",
        "cs",
        "class ChainCs {\n\
         \x20   static int cs_entry() { return cs_mid(); }\n\
         \x20   static int cs_mid() { return cs_sink(); }\n\
         \x20   static int cs_sink() { return 1; }\n\
         }\n",
    ),
    (
        "chain.go",
        "go",
        "package chain\n\n\
         func go_entry() int { return go_mid() }\n\
         func go_mid() int { return go_sink() }\n\
         func go_sink() int { return 1 }\n",
    ),
    (
        "chain.rs",
        "rs",
        "pub fn rs_entry() -> i32 { rs_mid() }\n\
         fn rs_mid() -> i32 { rs_sink() }\n\
         fn rs_sink() -> i32 { 1 }\n",
    ),
    (
        "chain.php",
        "php",
        "<?php\n\
         function php_entry() { return php_mid(); }\n\
         function php_mid() { return php_sink(); }\n\
         function php_sink() { return 1; }\n",
    ),
    (
        "chain.rb",
        "rb",
        "def rb_entry\n  rb_mid()\nend\n\n\
         def rb_mid\n  rb_sink()\nend\n\n\
         def rb_sink\n  1\nend\n",
    ),
    (
        "chain.swift",
        "sw",
        "func sw_entry() -> Int { return sw_mid() }\n\
         func sw_mid() -> Int { return sw_sink() }\n\
         func sw_sink() -> Int { return 1 }\n",
    ),
    (
        "chain.c",
        "c",
        "int c_sink(void) { return 1; }\n\
         int c_mid(void) { return c_sink(); }\n\
         int c_entry(void) { return c_mid(); }\n",
    ),
    (
        "chain.cpp",
        "cpp",
        "int cpp_sink() { return 1; }\n\
         int cpp_mid() { return cpp_sink(); }\n\
         int cpp_entry() { return cpp_mid(); }\n",
    ),
    (
        "chain.dart",
        "dart",
        "int dart_entry() => dart_mid();\n\
         int dart_mid() => dart_sink();\n\
         int dart_sink() => 1;\n",
    ),
];

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo holding every fixture, committed and indexed once.
fn indexed_polyglot_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir(repo.join("src")).unwrap();
    for (file, _, source) in FIXTURES {
        std::fs::write(repo.join("src").join(file), source).unwrap();
    }
    // A path whose hops are NOT all the same relation: `makeChild` reaches
    // `TBase` only through `TChild`'s inheritance edge. Both hops carry a
    // reason string (`type_annotation`, `heritage`) and neither of those says
    // "this one is inheritance", so the relation type has to be its own field.
    std::fs::write(
        repo.join("src/mixed.ts"),
        "export class TBase { work(): number { return 1; } }\n\
         export class TChild extends TBase {}\n\
         export function makeChild(): TChild { return new TChild(); }\n",
    )
    .unwrap();
    // A production chain whose only link runs through a test file. Both the
    // path walk and the impact walk have to exclude it by default and include
    // it under --include-tests, which is what pins their two copies of the
    // node guard to the same behaviour.
    std::fs::create_dir(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("src/bridge.py"),
        "def prod_start():\n    return t_bridge()\n\ndef prod_end():\n    return 1\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("tests/test_bridge.py"),
        "def t_bridge():\n    return prod_end()\n",
    )
    .unwrap();
    // Two definitions of one name, plus a bare call to it. At index time the
    // resolver suppresses the ambiguous call, so the graph is missing an edge
    // that the source plainly has — the case where a miss is a lower bound.
    std::fs::write(repo.join("src/amb_a.py"), "def dupe():\n    return 1\n").unwrap();
    std::fs::write(repo.join("src/amb_b.py"), "def dupe():\n    return 2\n").unwrap();
    std::fs::write(
        repo.join("src/amb_caller.py"),
        "def run_all():\n    return dupe()\n",
    )
    .unwrap();
    // A chain long enough that the default depth reaches it but --depth 2 does
    // not: the depth cap has to be observable to be worth having.
    std::fs::write(
        repo.join("src/deep.py"),
        "def d0():\n    return d1()\n\n\
         def d1():\n    return d2()\n\n\
         def d2():\n    return d3()\n\n\
         def d3():\n    return d4()\n\n\
         def d4():\n    return 1\n\n\
         def island():\n    return 0\n",
    )
    .unwrap();
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
            "init",
        ],
    );
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_path(repo: &Path, args: &[&str]) -> Value {
    let mut argv = vec!["path"];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--format", "json"]);
    let out = Command::new(ecp_bin())
        .args(&argv)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp path failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "ecp path {args:?} did not emit JSON ({e}): stdout={stdout} stderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Names on the path, in order.
fn step_names(out: &Value) -> Vec<String> {
    out["path"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a path array, got {out}"))
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect()
}

/// The load-bearing contract, across the polyglot matrix: `entry -> sink`
/// resolves, and the answer NAMES THE INTERMEDIATE. Reachability alone was
/// already available through cypher's `-[:Calls*1..N]->`; the middle element
/// is the whole reason this command exists, so asserting on it is asserting on
/// the feature rather than on a rendering.
///
/// Every language is checked in one pass and the failures are reported
/// together: knowing that 3 of 14 languages regressed is a different repair
/// job from knowing that 1 did.
#[test]
fn path_names_the_intermediate_in_every_mainstream_language() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let mut failures: Vec<String> = Vec::new();
    for (_, pfx, _) in FIXTURES {
        let entry = format!("{pfx}_entry");
        let sink = format!("{pfx}_sink");
        let out = run_path(tmp.path(), &[&entry, &sink]);
        if out["found"].as_bool() != Some(true) {
            failures.push(format!("{pfx}: no path {entry} -> {sink} ({out})"));
            continue;
        }
        let names = step_names(&out);
        let want = vec![entry.clone(), format!("{pfx}_mid"), sink.clone()];
        if names != want {
            failures.push(format!("{pfx}: got {names:?}, want {want:?}"));
        }
        if out["hops"].as_u64() != Some(2) {
            failures.push(format!("{pfx}: hops = {}, want 2", out["hops"]));
        }
        // Each hop has to carry its edge, not just its endpoint. Without this
        // the suite would still pass if the payload degraded to bare names.
        let steps = out["path"].as_array().unwrap();
        for step in &steps[1..] {
            if step["viaRelType"].as_str() != Some("calls") {
                failures.push(format!(
                    "{pfx}: hop into {} has viaRelType {}, want calls",
                    step["name"], step["viaRelType"]
                ));
            }
            if !step["viaConfidence"].is_number() {
                failures.push(format!(
                    "{pfx}: hop into {} lost its confidence",
                    step["name"]
                ));
            }
        }
        if steps[0]["viaRelType"].as_str() != Some("") {
            failures.push(format!(
                "{pfx}: start node claims an incoming edge: {}",
                steps[0]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} languages failed:\n{}",
        failures.len(),
        FIXTURES.len(),
        failures.join("\n")
    );
}

/// An unreachable pair is a real answer. The contract is `found: false` with
/// no fabricated chain — never an error, and never a plausible-looking route.
#[test]
fn path_reports_no_route_between_unconnected_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let out = run_path(tmp.path(), &["d0", "island"]);
    assert_eq!(out["found"].as_bool(), Some(false), "{out}");
    assert!(out["path"].is_null(), "a miss must carry no path: {out}");
    // Contract: a miss explains how to widen the search rather than leaving
    // the caller to guess. The exact wording is free to change.
    let caveat = out["result"].as_str().unwrap_or_default();
    assert!(
        caveat.contains("--depth"),
        "miss caveat lost its hint: {out}"
    );
}

/// Swapping the two arguments is the mistake this command invites, so a miss
/// in one direction reports the direction that works. Contract: the caveat
/// names the other direction; without it the caller burns a round trip
/// rediscovering it.
#[test]
fn path_miss_names_the_working_direction_when_arguments_are_swapped() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let out = run_path(tmp.path(), &["d4", "d0"]);
    assert_eq!(out["found"].as_bool(), Some(false), "{out}");
    let caveat = out["result"].as_str().unwrap_or_default();
    assert!(
        caveat.contains("upstream"),
        "swapped-argument miss must name the upstream direction: {out}"
    );

    // And that direction has to actually work when taken.
    let reversed = run_path(tmp.path(), &["d4", "d0", "--direction", "up"]);
    assert_eq!(reversed["found"].as_bool(), Some(true), "{reversed}");
    assert_eq!(
        step_names(&reversed),
        vec!["d4", "d3", "d2", "d1", "d0"],
        "upstream walk must climb the callers in order: {reversed}"
    );
}

/// `--depth` bounds the search. Contract: a chain longer than the cap is a
/// miss, and the same chain inside the cap is a hit — so the flag is doing
/// work rather than being ignored.
#[test]
fn path_depth_cap_bounds_the_search() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let capped = run_path(tmp.path(), &["d0", "d4", "--depth", "2"]);
    assert_eq!(capped["found"].as_bool(), Some(false), "{capped}");

    let full = run_path(tmp.path(), &["d0", "d4", "--depth", "4"]);
    assert_eq!(full["found"].as_bool(), Some(true), "{full}");
    assert_eq!(full["hops"].as_u64(), Some(4), "{full}");
}

/// A name the graph does not hold is an error, not an empty path — the same
/// contract `ecp impact` keeps, so an LLM reads one failure mode, not two.
#[test]
fn path_rejects_a_symbol_the_graph_does_not_hold() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let out = Command::new(ecp_bin())
        .args(["path", "d0", "no_such_symbol_anywhere", "--format", "json"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("ecp path failed to spawn");
    assert!(!out.status.success(), "an unknown symbol must fail the run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no_such_symbol_anywhere"),
        "the error must name the symbol it could not find: {stderr}"
    );
}

/// Every hop carries its relation type. The default walk follows every
/// non-containment relation, so without this an inheritance hop and a call hop
/// are indistinguishable in the output — and they are not the same fact.
///
/// Contract: the two hops of `make_child -> PChild -> PBase` report DIFFERENT
/// `viaRelType` values, and the inheritance one says so. The exact spelling of
/// the first hop's relation is the resolver's business, not this test's.
#[test]
fn path_steps_carry_their_relation_type() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let out = run_path(tmp.path(), &["makeChild", "TBase"]);
    assert_eq!(out["found"].as_bool(), Some(true), "{out}");

    let rels: Vec<&str> = out["path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["viaRelType"].as_str().unwrap())
        .collect();
    assert_eq!(rels[0], "", "the start node has no incoming edge: {out}");
    let hops = &rels[1..];
    assert!(
        hops.contains(&"extends"),
        "the inheritance hop must name itself: {out}"
    );
    assert!(
        hops.iter().collect::<std::collections::HashSet<_>>().len() > 1,
        "a mixed-relation path must not render every hop alike: {out}"
    );
}

/// With two same-named definitions the resolver suppresses the bare call at
/// index time, so the graph is missing an edge the source plainly has. The
/// payload otherwise tells the caller that an unreachable pair is a real
/// answer, which here would be a lie.
///
/// Contract: the miss carries a caveat naming the collision. `ecp impact`
/// keeps the same contract for the same reason.
#[test]
fn path_miss_admits_when_a_same_name_collision_suppressed_edges() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let out = run_path(tmp.path(), &["run_all", "dupe"]);
    assert_eq!(out["toCandidates"].as_u64(), Some(2), "{out}");
    let caveat = out["result"].as_str().unwrap_or_default();
    assert!(
        caveat.contains("same-named definitions"),
        "a collision-driven miss must say so: {out}"
    );
}

/// `--to-file` narrows an overloaded endpoint to one definition. Contract: the
/// candidate count drops, so the caller can ask about a specific definition
/// instead of accepting whichever one the walk happened to reach.
#[test]
fn path_endpoint_file_filter_narrows_the_candidate_set() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let wide = run_path(tmp.path(), &["run_all", "dupe"]);
    assert_eq!(wide["toCandidates"].as_u64(), Some(2), "{wide}");

    let narrow = run_path(tmp.path(), &["run_all", "dupe", "--to-file", "amb_a"]);
    assert_eq!(narrow["toCandidates"].as_u64(), Some(1), "{narrow}");
}

/// A confidence outside 0.0–1.0 filters out every edge, and the resulting
/// `found: false` is indistinguishable from a real miss. Contract: the flag is
/// rejected up front rather than producing a plausible wrong answer.
#[test]
fn path_rejects_an_out_of_range_confidence() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    for bad in ["2", "-1", "NaN"] {
        let out = Command::new(ecp_bin())
            .args(["path", "d0", "d4", "--min-confidence", bad])
            .current_dir(tmp.path())
            .env("HOME", tmp.path())
            .output()
            .expect("ecp path failed to spawn");
        assert!(
            !out.status.success(),
            "--min-confidence {bad} must be rejected, not silently applied"
        );
    }
}

/// `ecp path` and `ecp impact` keep separate copies of the node guard and the
/// edge filter — `run_bfs` inlines them because routing it through the shared
/// helpers measured +2% on the vscode corpus, and per-query latency outranks
/// the tidier arrangement. This is what stops the copies drifting.
///
/// Contract: the two walks agree about the same graph. `prod_start` reaches
/// `prod_end` only through a function in a test file, so both must miss it by
/// default and both must find it under `--include-tests`. A divergence in
/// either copy of the guard flips exactly one of the four assertions.
#[test]
fn path_walks_the_same_graph_as_impact() {
    let tmp = tempfile::tempdir().unwrap();
    indexed_polyglot_repo(tmp.path());

    let reaches_via_impact = |extra: &[&str]| -> bool {
        let mut argv = vec![
            "impact",
            "prod_start",
            "--direction",
            "down",
            "--depth",
            "4",
            "--format",
            "json",
        ];
        argv.extend_from_slice(extra);
        let out = Command::new(ecp_bin())
            .args(&argv)
            .current_dir(tmp.path())
            .env("HOME", tmp.path())
            .output()
            .expect("ecp impact failed to spawn");
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "impact did not emit JSON ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
        v["impact"]
            .as_array()
            .map(|rows| rows.iter().any(|r| r["name"].as_str() == Some("prod_end")))
            .unwrap_or(false)
    };

    let path_default = run_path(tmp.path(), &["prod_start", "prod_end", "--depth", "4"]);
    assert_eq!(
        path_default["found"].as_bool(),
        Some(false),
        "a route through a test file must not be a production path: {path_default}"
    );
    assert!(
        !reaches_via_impact(&[]),
        "impact must exclude the same test-file hop"
    );

    let path_tests = run_path(
        tmp.path(),
        &["prod_start", "prod_end", "--depth", "4", "--include-tests"],
    );
    assert_eq!(
        path_tests["found"].as_bool(),
        Some(true),
        "--include-tests must open the same hop for path: {path_tests}"
    );
    assert_eq!(
        step_names(&path_tests),
        vec!["prod_start", "t_bridge", "prod_end"],
        "{path_tests}"
    );
    assert!(
        reaches_via_impact(&["--include-tests"]),
        "--include-tests must open the same hop for impact"
    );
}
