//! FU-2026-10-03-cab07766ef07: `ecp impact --ambiguous-callers` lists the call
//! sites the ambiguity caveat hides. Synthetic graphs (no `admin index`) over a
//! real git work tree, so each test controls exactly which `Calls` edges the
//! graph holds and which text the `git grep` sees.

mod common;

use common::{run_git, write};
use ecp_cli::commands::impact::{
    build_payload, Direction, ImpactArgs, DEFAULT_CONFIDENCE_THRESHOLD,
};
use ecp_cli::engine::Engine;
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use serde_json::Value;
use std::path::Path;

fn engine_from(fx: GraphFixture) -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    std::fs::write(&graph_path, fx.into_bytes()).unwrap();
    let engine = Engine::load(&graph_path).expect("engine load");
    (tmp, engine)
}

fn git_track_all(repo: &Path) {
    run_git(repo, &["init", "-q"]);
    run_git(repo, &["add", "-A"]);
}

fn args(name: &str, file: &str, repo: &Path) -> ImpactArgs {
    ImpactArgs {
        name: Some(name.to_string()),
        target: None,
        baseline: None,
        file: Some(file.to_string()),
        kind: None,
        direction: Direction::Up,
        depth: 5,
        high_trust_only: false,
        min_confidence: None,
        include_tests: false,
        exclude_tests: false,
        relation_types: None,
        repo: Some(repo.to_string_lossy().into_owned()),
        test_coverage: false,
        no_heuristic: false,
        confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
        explain_confidence: false,
        format: None,
        literal: None,
        literal_coherence: false,
        batch: false,
        ambiguous_callers: true,
        max_results: None,
    }
}

/// Two `get` definitions (beta's span starts on its decorator line), and a
/// caller file whose functions are, in order: one with a resolved `Calls`
/// edge to `get`, one without.
fn python_collision(repo: &Path) -> GraphFixture {
    write(repo, "src/alpha.py", "def get():\n    return 1\n");
    write(repo, "src/beta.py", "@cache\ndef get():\n    return 2\n");
    write(
        repo,
        "src/caller.py",
        "import x\n\
         get()\n\
         def resolved():\n\
         \x20   get()\n\
         def unresolved():\n\
         \x20   obj.get(1)\n\
         \x20   getter()\n",
    );
    git_track_all(repo);

    let mut fx = GraphFixture::new();
    let alpha = fx.func("src/alpha.py", "get");
    fx.span(alpha, (0, 0, 1, 12));
    let beta = fx.func("src/beta.py", "get");
    fx.span(beta, (0, 0, 2, 12));
    let resolved = fx.func("src/caller.py", "resolved");
    fx.span(resolved, (2, 0, 3, 9));
    let unresolved = fx.func("src/caller.py", "unresolved");
    fx.span(unresolved, (4, 0, 6, 12));
    fx.edge_with(resolved, alpha, RelType::Calls, 1.0, "call");
    fx
}

fn sites(payload: &Value) -> Vec<(String, u64, Value, String)> {
    payload["ambiguous_callers"]["sites"]
        .as_array()
        .unwrap_or_else(|| panic!("ambiguous_callers.sites must be an array: {payload}"))
        .iter()
        .map(|s| {
            (
                s["file"].as_str().unwrap().to_string(),
                s["line"].as_u64().unwrap(),
                s["enclosing"].clone(),
                s["form"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn python_payload(extra: impl FnOnce(&mut ImpactArgs)) -> Value {
    let repo = tempfile::tempdir().unwrap();
    let (_g, engine) = engine_from(python_collision(repo.path()));
    let mut a = args("get", "alpha.py", repo.path());
    extra(&mut a);
    build_payload(&a, &engine).expect("impact on get --file alpha.py")
}

#[test]
fn test_ambiguous_callers_python_collision_lists_only_unattributed_sites() {
    let payload = python_payload(|_| {});
    assert_eq!(
        sites(&payload),
        vec![
            ("src/caller.py".into(), 2, Value::Null, "bare".into()),
            (
                "src/caller.py".into(),
                6,
                Value::from("unresolved"),
                "member".into()
            ),
        ],
        "payload: {payload}"
    );
    assert_eq!(payload["ambiguous_callers"]["total"], 2);
    assert_eq!(payload["ambiguous_callers"]["shown"], 2);
}

#[test]
fn test_ambiguous_callers_hit_in_function_with_calls_edge_dropped() {
    let payload = python_payload(|_| {});
    assert!(
        !sites(&payload)
            .iter()
            .any(|s| s.0 == "src/caller.py" && s.1 == 4),
        "`resolved` already has a Calls edge to get; its site must not repeat: {payload}"
    );
}

#[test]
fn test_ambiguous_callers_definition_lines_dropped() {
    let payload = python_payload(|_| {});
    let files: Vec<String> = sites(&payload).into_iter().map(|s| s.0).collect();
    assert!(
        !files
            .iter()
            .any(|f| f == "src/alpha.py" || f == "src/beta.py"),
        "definition lines (incl. a span starting on a decorator) must be dropped: {payload}"
    );
}

#[test]
fn test_ambiguous_callers_top_level_hit_kept_with_null_enclosing() {
    let payload = python_payload(|_| {});
    let top = sites(&payload)
        .into_iter()
        .find(|s| s.1 == 2)
        .unwrap_or_else(|| panic!("top-level get() must be kept: {payload}"));
    assert_eq!(top.2, Value::Null);
}

#[test]
fn test_ambiguous_callers_identifier_substring_not_matched() {
    let payload = python_payload(|_| {});
    assert!(
        !sites(&payload).iter().any(|s| s.1 == 7),
        "`getter()` must not match `get`: {payload}"
    );
}

#[test]
fn test_ambiguous_callers_flag_unset_no_field() {
    let payload = python_payload(|a| a.ambiguous_callers = false);
    assert!(
        payload.get("ambiguous_callers").is_none(),
        "flag off must not add the field: {payload}"
    );
}

#[test]
fn test_ambiguous_callers_direction_down_no_field() {
    let payload = python_payload(|a| a.direction = Direction::Down);
    assert!(
        payload.get("ambiguous_callers").is_none(),
        "downstream walk ignores the flag: {payload}"
    );
}

#[test]
fn test_ambiguous_callers_single_definition_no_field_no_git_run() {
    // A non-git directory: had git run, the field would carry an error.
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/only.py", "def solo():\n    pass\nsolo()\n");
    let mut fx = GraphFixture::new();
    let solo = fx.func("src/only.py", "solo");
    fx.span(solo, (0, 0, 1, 8));
    let (_g, engine) = engine_from(fx);
    let payload = build_payload(&args("solo", "only.py", dir.path()), &engine).unwrap();
    assert!(
        payload.get("ambiguous_callers").is_none(),
        "one definition: no field, no git run: {payload}"
    );
}

#[test]
fn test_ambiguous_callers_not_a_git_repo_reports_error_and_empty_sites() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/alpha.py", "def get():\n    pass\n");
    write(dir.path(), "src/beta.py", "def get():\n    pass\nget()\n");
    let mut fx = GraphFixture::new();
    let alpha = fx.func("src/alpha.py", "get");
    fx.span(alpha, (0, 0, 1, 8));
    let beta = fx.func("src/beta.py", "get");
    fx.span(beta, (0, 0, 1, 8));
    let (_g, engine) = engine_from(fx);
    let payload = build_payload(&args("get", "alpha.py", dir.path()), &engine)
        .expect("a git failure must not fail the query");
    let field = &payload["ambiguous_callers"];
    assert!(
        field["error"].as_str().is_some_and(|e| !e.is_empty()),
        "error reason expected: {payload}"
    );
    assert_eq!(field["sites"], serde_json::json!([]));
    assert_eq!(field["total"], 0);
}

#[test]
fn test_ambiguous_callers_more_than_cap_shown_50_total_full() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "src/alpha.py", "def get():\n    pass\n");
    write(repo.path(), "src/beta.py", "def get():\n    pass\n");
    write(repo.path(), "src/many.py", &"get()\n".repeat(60));
    git_track_all(repo.path());
    let mut fx = GraphFixture::new();
    let alpha = fx.func("src/alpha.py", "get");
    fx.span(alpha, (0, 0, 1, 8));
    let beta = fx.func("src/beta.py", "get");
    fx.span(beta, (0, 0, 1, 8));
    let (_g, engine) = engine_from(fx);
    let payload = build_payload(&args("get", "alpha.py", repo.path()), &engine).unwrap();
    let field = &payload["ambiguous_callers"];
    assert_eq!(field["total"], 60, "{payload}");
    assert_eq!(field["shown"], 50, "{payload}");
    let lines: Vec<u64> = sites(&payload).iter().map(|s| s.1).collect();
    assert_eq!(lines, (1..=50).collect::<Vec<u64>>(), "sorted by line");
}

#[test]
fn test_ambiguous_callers_call_syntaxes_across_languages_matched() {
    // (extension, call-file body, expected (line, form) hits)
    let langs: &[(&str, &str, &[(u64, &str)])] = &[
        ("py", "x = 1\nfetch(a)\n", &[(2, "bare")]),
        ("ts", "const a = 1;\nobj.fetch(a);\n", &[(2, "member")]),
        ("js", "obj.fetch (a);\n", &[(1, "member")]),
        ("go", "package m\npkg.fetch(a)\n", &[(2, "member")]),
        (
            "rs",
            "Type::fetch(a);\nx.fetch(a);\n",
            &[(1, "member"), (2, "member")],
        ),
        ("java", "x.fetch(a);\n", &[(1, "member")]),
        ("kt", "x.fetch(a)\n", &[(1, "member")]),
        ("cs", "x.fetch(a);\n", &[(1, "member")]),
        ("dart", "x.fetch(a);\n", &[(1, "member")]),
        (
            "php",
            "$x->fetch($a);\nfetch($a);\n",
            &[(1, "member"), (2, "bare")],
        ),
        ("c", "fetch(a);\n", &[(1, "bare")]),
        ("cpp", "p->fetch(a);\n", &[(1, "member")]),
        ("swift", "x.fetch(a)\n", &[(1, "member")]),
        // Known miss, documented in the flag help: no parentheses, no match.
        ("rb", "obj.fetch a\n", &[]),
    ];
    let repo = tempfile::tempdir().unwrap();
    let mut fx = GraphFixture::new();
    for (ext, body, _) in langs {
        let def = format!("def/d.{ext}");
        write(repo.path(), &def, "fetch() {}\n");
        let node = fx.func(&def, "fetch");
        fx.span(node, (0, 0, 0, 10));
        write(repo.path(), &format!("calls/c.{ext}"), body);
    }
    git_track_all(repo.path());
    let (_g, engine) = engine_from(fx);
    let payload = build_payload(&args("fetch", "d.py", repo.path()), &engine).unwrap();
    let got = sites(&payload);
    for (ext, _, want) in langs {
        let file = format!("calls/c.{ext}");
        let have: Vec<(u64, &str)> = got
            .iter()
            .filter(|s| s.0 == file)
            .map(|s| (s.1, s.3.as_str()))
            .collect();
        assert_eq!(have, want.to_vec(), "{file}: {payload}");
    }
    assert!(
        !got.iter().any(|s| s.0.starts_with("def/")),
        "definition lines dropped: {payload}"
    );
}
