//! `unexplained_refs`: the whole-word text occurrences of a target's name
//! that no graph edge explains, scanned by default on a short caller list or
//! a colliding name, and forced by `--ambiguous-callers`. Synthetic graphs
//! (no `admin index`) over a real git work tree, so each test controls
//! exactly which edges the graph holds and which text `git grep` sees. The
//! per-language matrix lives in `impact_unexplained_refs.rs`.

use crate::common;

use common::{run_git, write};
use ecp_cli::commands::impact::{
    build_payload, Direction, ImpactArgs, DEFAULT_CONFIDENCE_THRESHOLD,
};
use ecp_cli::engine::Engine;
use ecp_core::graph::{NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use serde_json::{json, Value};
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
        ambiguous_callers: false,
        max_results: None,
    }
}

/// (path, line, enclosing, form) of every listed site.
fn sites(payload: &Value) -> Vec<(String, u64, Value, String)> {
    payload["unexplained_refs"]["sites"]
        .as_array()
        .unwrap_or_else(|| panic!("unexplained_refs.sites must be an array: {payload}"))
        .iter()
        .map(|s| {
            (
                s["path"].as_str().unwrap().to_string(),
                s["line"].as_u64().unwrap(),
                s["enclosing"].clone(),
                s["form"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn site_keys(payload: &Value) -> Vec<(String, u64)> {
    sites(payload).into_iter().map(|s| (s.0, s.1)).collect()
}

/// One `get` definition in `src/lib.py` (line 1-2) plus `extra` files.
fn one_def(repo: &Path, extra: &[(&str, &str)]) -> (GraphFixture, u32) {
    write(repo, "src/lib.py", "def get():\n    return 1\n");
    for (path, body) in extra {
        write(repo, path, body);
    }
    git_track_all(repo);
    let mut fx = GraphFixture::new();
    let def = fx.func("src/lib.py", "get");
    fx.span(def, (0, 0, 1, 12));
    (fx, def)
}

fn impact(fx: GraphFixture, repo: &Path, extra: impl FnOnce(&mut ImpactArgs)) -> Value {
    let (_g, engine) = engine_from(fx);
    let mut a = args("get", "lib.py", repo);
    extra(&mut a);
    build_payload(&a, &engine).expect("impact on get")
}

// ── I1: when the scan runs ──────────────────────────────────────────────────

/// Fails before the change: the scan ran only with the flag and a collision.
#[test]
fn test_unexplained_refs_zero_callers_function_passed_as_value_listed() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, _) = one_def(
        repo.path(),
        &[("src/routes.py", "def register():\n    return [get]\n")],
    );
    let register = fx.func("src/routes.py", "register");
    fx.span(register, (0, 0, 1, 24));
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        sites(&payload),
        vec![(
            "src/routes.py".into(),
            2,
            Value::from("register"),
            "reference".into()
        )],
        "{payload}"
    );
    assert_eq!(payload["unexplained_refs"]["total"], 1);
    assert_eq!(payload["unexplained_refs"]["files"], 1);
}

/// Fails before the change: only the definitions' language family was
/// searched, so a template of another language was never read.
#[test]
fn test_unexplained_refs_template_of_other_language_listed() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(
        repo.path(),
        &[
            ("templates/page.html", "<p>{{ get }}</p>\n"),
            (
                "web/App.vue",
                "<template>\n  <b @click=\"get()\" />\n</template>\n",
            ),
        ],
    );
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        sites(&payload),
        vec![
            (
                "templates/page.html".into(),
                1,
                Value::Null,
                "reference".into()
            ),
            ("web/App.vue".into(), 2, Value::Null, "bare".into()),
        ],
        "{payload}"
    );
}

#[test]
fn test_unexplained_refs_two_callers_and_missed_site_lists_only_missed() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, def) = one_def(
        repo.path(),
        &[(
            "src/use.py",
            "def a():\n    get()\ndef b():\n    get()\ndef c():\n    f = get\n",
        )],
    );
    for (name, start) in [("a", 0), ("b", 2), ("c", 4)] {
        let f = fx.func("src/use.py", name);
        fx.span(f, (start, 0, start + 1, 12));
        if name != "c" {
            fx.edge_with(f, def, RelType::Calls, 1.0, "call");
        }
    }
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        site_keys(&payload),
        vec![("src/use.py".to_string(), 6)],
        "{payload}"
    );
}

fn three_callers(repo: &Path) -> (GraphFixture, u32) {
    let (mut fx, def) = one_def(
        repo,
        &[(
            "src/use.py",
            "def a():\n    get()\ndef b():\n    get()\ndef c():\n    get()\nx = get\n",
        )],
    );
    for (name, start) in [("a", 0), ("b", 2), ("c", 4)] {
        let f = fx.func("src/use.py", name);
        fx.span(f, (start, 0, start + 1, 9));
        fx.edge_with(f, def, RelType::Calls, 1.0, "call");
    }
    (fx, def)
}

#[test]
fn test_unexplained_refs_three_callers_scan_skipped_field_absent() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = three_callers(repo.path());
    let payload = impact(fx, repo.path(), |_| {});
    assert!(
        payload.get("unexplained_refs").is_none(),
        "3 callers > UNEXPLAINED_SCAN_MAX_CALLERS: no scan, no field: {payload}"
    );
}

#[test]
fn test_unexplained_refs_flag_forces_scan_at_five_callers() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, def) = three_callers(repo.path());
    for (name, start) in [("d", 10), ("e", 12)] {
        let f = fx.func("src/more.py", name);
        fx.span(f, (start, 0, start + 1, 9));
        fx.edge_with(f, def, RelType::Calls, 1.0, "call");
    }
    let payload = impact(fx, repo.path(), |a| a.ambiguous_callers = true);
    assert_eq!(
        site_keys(&payload),
        vec![("src/use.py".to_string(), 7)],
        "the flag scans past the caller cap: {payload}"
    );
}

#[test]
fn test_unexplained_refs_direction_down_no_field() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(repo.path(), &[("src/use.py", "x = get\n")]);
    let payload = impact(fx, repo.path(), |a| a.direction = Direction::Down);
    assert!(payload.get("unexplained_refs").is_none(), "{payload}");
}

#[test]
fn test_unexplained_refs_direction_both_scans() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(repo.path(), &[("src/use.py", "x = get\n")]);
    let payload = impact(fx, repo.path(), |a| a.direction = Direction::Both);
    assert_eq!(site_keys(&payload), vec![("src/use.py".to_string(), 1)]);
}

/// `batch` is what `--batch` and the library fan-outs set: the old opt-in
/// rule holds, so a single-definition name with 0 callers is not scanned.
#[test]
fn test_unexplained_refs_batch_keeps_opt_in_rule() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(repo.path(), &[("src/use.py", "x = get\n")]);
    let payload = impact(fx, repo.path(), |a| {
        a.batch = true;
        a.ambiguous_callers = true;
    });
    assert!(
        payload.get("unexplained_refs").is_none(),
        "batch scans only a colliding name with the flag: {payload}"
    );
}

// ── I1: what the scan reports ───────────────────────────────────────────────

#[test]
fn test_unexplained_refs_scan_found_nothing_total_zero_field_present() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(repo.path(), &[("src/other.py", "def other():\n    pass\n")]);
    let payload = impact(fx, repo.path(), |_| {});
    let field = &payload["unexplained_refs"];
    assert_eq!(field["total"], 0, "{payload}");
    assert_eq!(field["files"], 0, "{payload}");
    assert_eq!(field["shown"], 0, "{payload}");
    assert_eq!(field["sites"], json!([]), "{payload}");
    assert!(field.get("error").is_none(), "{payload}");
    assert!(field["note"]
        .as_str()
        .is_some_and(|n| n.contains("does not explain")));
}

/// Two definitions, one with its span starting on a decorator line: every
/// occurrence is a declaration, so nothing is unexplained.
#[test]
fn test_unexplained_refs_hits_only_in_definition_lines_total_zero() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "src/alpha.py", "def get():\n    return 1\n");
    write(
        repo.path(),
        "src/beta.py",
        "@cache\ndef get():\n    return 2\n",
    );
    git_track_all(repo.path());
    let mut fx = GraphFixture::new();
    let alpha = fx.func("src/alpha.py", "get");
    fx.span(alpha, (0, 0, 1, 12));
    let beta = fx.func("src/beta.py", "get");
    fx.span(beta, (0, 0, 2, 12));
    let payload = impact(fx, repo.path(), |a| a.file = Some("alpha.py".into()));
    assert_eq!(payload["unexplained_refs"]["total"], 0, "{payload}");
}

#[test]
fn test_unexplained_refs_vendored_build_and_document_files_excluded() {
    let repo = tempfile::tempdir().unwrap();
    let body = "x = get\n";
    let (fx, _) = one_def(
        repo.path(),
        &[
            ("node_modules/pkg/index.js", body),
            ("web/node_modules/pkg/index.js", body),
            ("vendor/lib/a.php", body),
            (".venv/lib/python3.12/site-packages/m.py", body),
            ("dist/bundle.js", body),
            ("packages/ui/build/out.js", body),
            ("static/app.min.js", body),
            ("README.md", body),
            ("docs/notes.rst", body),
            ("requirements.txt", body),
            ("src/kept.py", body),
        ],
    );
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        site_keys(&payload),
        vec![("src/kept.py".to_string(), 1)],
        "{payload}"
    );
}

/// A value declaration caller (schema 56 makes it the caller of the calls
/// outside functions) explains only its own span: the same file's top-level
/// line and function stay listed.
#[test]
fn test_unexplained_refs_value_declaration_caller_explains_only_own_span() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, def) = one_def(
        repo.path(),
        &[(
            "src/table.py",
            "HANDLERS = [\n    get,\n]\nOTHER = get\ndef later():\n    return get\n",
        )],
    );
    let handlers = fx.node(NodeKind::Variable, "src/table.py", "HANDLERS");
    fx.span(handlers, (0, 0, 2, 1));
    fx.edge_with(handlers, def, RelType::Calls, 1.0, "call");
    let later = fx.func("src/table.py", "later");
    fx.span(later, (4, 0, 5, 14));
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        sites(&payload)
            .into_iter()
            .map(|s| (s.1, s.2))
            .collect::<Vec<_>>(),
        vec![(4, Value::Null), (6, Value::from("later"))],
        "{payload}"
    );
}

/// A type caller explains its own span, but not the inside of a named
/// method declared within it: that method is a caller of its own.
#[test]
fn test_unexplained_refs_type_caller_explains_span_outside_named_methods() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, def) = one_def(
        repo.path(),
        &[(
            "src/svc.py",
            "class Svc:\n    hook = get\n    def run(self):\n        return get()\n",
        )],
    );
    let svc = fx.node(NodeKind::Class, "src/svc.py", "Svc");
    fx.span(svc, (0, 0, 3, 20));
    fx.edge_with(svc, def, RelType::Calls, 1.0, "call");
    let run = fx.method("src/svc.py", "Svc", "run");
    fx.span(run, (2, 4, 3, 20));
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        sites(&payload)
            .into_iter()
            .map(|s| (s.1, s.2, s.3))
            .collect::<Vec<_>>(),
        vec![(4, Value::from("run"), "bare".to_string())],
        "{payload}"
    );
}

/// Fails before the change in the other direction: a non-function caller
/// used to explain its whole file; now only a File / Module does.
#[test]
fn test_unexplained_refs_file_caller_explains_whole_file() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, def) = one_def(
        repo.path(),
        &[
            ("src/main.py", "get()\ndef inner():\n    return get\n"),
            ("src/other.py", "y = get\n"),
        ],
    );
    let file = fx.node(NodeKind::File, "src/main.py", "main.py");
    fx.span(file, (0, 0, 2, 14));
    fx.edge_with(file, def, RelType::Calls, 1.0, "call");
    let inner = fx.func("src/main.py", "inner");
    fx.span(inner, (1, 0, 2, 14));
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        site_keys(&payload),
        vec![("src/other.py".to_string(), 1)],
        "{payload}"
    );
}

/// The graph holds an import edge, not its line: the first top-level
/// occurrence of an importing file is the import; a later one is not.
#[test]
fn test_unexplained_refs_import_line_explained_later_value_use_listed() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, def) = one_def(
        repo.path(),
        &[("src/app.py", "from lib import get\n\nROUTES = [get]\n")],
    );
    let file = fx.node(NodeKind::File, "src/app.py", "app.py");
    fx.edge(file, def, RelType::Imports);
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        site_keys(&payload),
        vec![("src/app.py".to_string(), 3)],
        "{payload}"
    );
}

#[test]
fn test_unexplained_refs_regex_metacharacter_name_matched_literally() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "src/lib.js", "function $get() {}\n");
    write(
        repo.path(),
        "src/use.js",
        "const a = $get;\nconst b = Xget;\nconst c = get;\n",
    );
    git_track_all(repo.path());
    let mut fx = GraphFixture::new();
    let def = fx.func("src/lib.js", "$get");
    fx.span(def, (0, 0, 0, 18));
    let (_g, engine) = engine_from(fx);
    let payload = build_payload(&args("$get", "lib.js", repo.path()), &engine).unwrap();
    assert_eq!(
        site_keys(&payload),
        vec![("src/use.js".to_string(), 1)],
        "`$` is literal, and `get` alone is another name: {payload}"
    );
}

#[test]
fn test_unexplained_refs_git_grep_failure_error_field_impact_succeeds() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(repo.path(), &[("src/use.py", "x = get\n")]);
    std::fs::write(repo.path().join(".git/index"), b"not an index").unwrap();
    let payload = impact(fx, repo.path(), |_| {});
    let field = &payload["unexplained_refs"];
    assert!(
        field["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("git grep failed")),
        "{payload}"
    );
    assert_eq!(field["total"], 0);
    assert_eq!(field["sites"], json!([]));
    assert_eq!(payload["status"], "success", "{payload}");
}

#[test]
fn test_unexplained_refs_not_a_git_work_tree_error_field_impact_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/lib.py", "def get():\n    pass\nx = get\n");
    let mut fx = GraphFixture::new();
    let def = fx.func("src/lib.py", "get");
    fx.span(def, (0, 0, 1, 8));
    let payload = impact(fx, dir.path(), |_| {});
    let field = &payload["unexplained_refs"];
    assert!(
        field["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{payload}"
    );
    assert_eq!(field["sites"], json!([]));
    assert_eq!(field["total"], 0);
}

#[test]
fn test_unexplained_refs_more_than_cap_shown_20_total_exact() {
    let repo = tempfile::tempdir().unwrap();
    let many = "f(get)\n".repeat(25);
    let (fx, _) = one_def(
        repo.path(),
        &[("src/many.py", many.as_str()), ("src/two.py", "get; get\n")],
    );
    let payload = impact(fx, repo.path(), |_| {});
    let field = &payload["unexplained_refs"];
    assert_eq!(field["total"], 27, "{payload}");
    assert_eq!(field["shown"], 20, "{payload}");
    assert_eq!(field["files"], 2, "files counts past the cap: {payload}");
    let lines: Vec<u64> = sites(&payload).iter().map(|s| s.1).collect();
    assert_eq!(
        lines,
        (1..=20).collect::<Vec<u64>>(),
        "sorted by path, line"
    );
}

#[test]
fn test_unexplained_refs_two_occurrences_on_one_line_counted_twice() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(repo.path(), &[("src/use.py", "get(); get()\nget()\n")]);
    let payload = impact(fx, repo.path(), |_| {});
    let field = &payload["unexplained_refs"];
    assert_eq!(field["total"], 3, "{payload}");
    assert_eq!(field["shown"], 2, "{payload}");
    let counts: Vec<Option<u64>> = field["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.get("count").and_then(Value::as_u64))
        .collect();
    assert_eq!(counts, vec![Some(2), None], "count is omitted when 1");
}

#[test]
fn test_unexplained_refs_partial_words_and_non_ascii_neighbours_not_listed() {
    let repo = tempfile::tempdir().unwrap();
    let (fx, _) = one_def(
        repo.path(),
        &[(
            "src/use.py",
            "getter()\nforget = 1\néget()\nget_x = 2\nok = get\n",
        )],
    );
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(site_keys(&payload), vec![("src/use.py".to_string(), 5)]);
}

// ── I1: attribution kept from `--ambiguous-callers` ─────────────────────────

/// Fails before the fix: the construction `new Config(1)` lands on the
/// constructor, whose `Calls` in-edge impact already counts as a caller, but
/// the filter read only the class's own in-edges.
#[test]
fn test_unexplained_refs_instantiator_via_constructor_dropped() {
    let repo = tempfile::tempdir().unwrap();
    for f in ["src/a.ts", "src/b.ts"] {
        write(
            repo.path(),
            f,
            "export class Config {\n  constructor(n: number) {}\n}\n",
        );
    }
    write(
        repo.path(),
        "src/use.ts",
        "export function load() {\n  return new Config(1);\n}\n\
         export function other() {\n  return new Config(2);\n}\n",
    );
    git_track_all(repo.path());
    let mut fx = GraphFixture::new();
    let mut ctors = Vec::new();
    for f in ["src/a.ts", "src/b.ts"] {
        let class = fx.node(NodeKind::Class, f, "Config");
        fx.span(class, (0, 0, 2, 1));
        let ctor = fx.node_owned(NodeKind::Constructor, f, "Config", "constructor");
        fx.span(ctor, (1, 2, 1, 25));
        fx.edge(class, ctor, RelType::HasMethod);
        ctors.push(ctor);
    }
    let load = fx.func("src/use.ts", "load");
    fx.span(load, (0, 0, 2, 1));
    let other = fx.func("src/use.ts", "other");
    fx.span(other, (3, 0, 5, 1));
    fx.edge_with(load, ctors[0], RelType::Calls, 1.0, "call");
    let (_g, engine) = engine_from(fx);
    let payload = build_payload(&args("Config", "a.ts", repo.path()), &engine).unwrap();
    assert_eq!(
        sites(&payload)
            .iter()
            .map(|s| (s.1, s.2.clone()))
            .collect::<Vec<_>>(),
        vec![(5, Value::from("other"))],
        "`load` is already a counted instantiator; `other` is not: {payload}"
    );
}

const NESTED_CALLER: &str =
    "def outer():\n    get()\n    def inner():\n        get()\n    return inner\n\
                             def known():\n    f = lambda: get()\n";

/// Fails before the fix on line 4: `outer` holds a `Calls` edge to `get`, and
/// the check looked at every enclosing scope, hiding the call in `inner`.
#[test]
fn test_unexplained_refs_nested_function_not_hidden_by_outer_caller() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, def) = one_def(repo.path(), &[("src/nested.py", NESTED_CALLER)]);
    let outer = fx.func("src/nested.py", "outer");
    fx.span(outer, (0, 0, 4, 16));
    let inner = fx.func("src/nested.py", "inner");
    fx.span(inner, (2, 4, 3, 14));
    let known = fx.func("src/nested.py", "known");
    fx.span(known, (5, 0, 6, 24));
    let lambda = fx.func("src/nested.py", "<anonymous>");
    fx.span(lambda, (6, 8, 6, 24));
    fx.edge_with(outer, def, RelType::Calls, 1.0, "call");
    fx.edge_with(known, def, RelType::Calls, 1.0, "call");
    let payload = impact(fx, repo.path(), |_| {});
    assert_eq!(
        sites(&payload)
            .iter()
            .map(|s| (s.1, s.2.clone()))
            .collect::<Vec<_>>(),
        vec![(4, Value::from("inner"))],
        "line 2 (outer) and line 7 (lambda inside known) are explained: {payload}"
    );
}

// ── I3: blind spots scoped to evidence ──────────────────────────────────────

fn spotted(repo: &Path) -> GraphFixture {
    let (mut fx, _) = one_def(
        repo,
        &[("src/use.py", "x = get\n"), ("src/quiet.py", "y = 1\n")],
    );
    fx.blind_spot("python-dynamic-call", "src/use.py", (0, 0, 0, 7), "getattr");
    fx.blind_spot(
        "python-dynamic-call",
        "src/quiet.py",
        (0, 0, 0, 5),
        "getattr",
    );
    fx.blind_spot("python-exec", "src/lib.py", (0, 0, 0, 3), "exec");
    fx
}

#[test]
fn test_blind_spot_warning_upstream_lists_site_files_only() {
    let repo = tempfile::tempdir().unwrap();
    let payload = impact(spotted(repo.path()), repo.path(), |_| {});
    let warning = &payload["blind_spot_warning"];
    assert_eq!(warning["files"], json!(["src/use.py"]), "{payload}");
    assert_eq!(warning["total"], 1, "{payload}");
    assert_eq!(warning["by_kind"], json!({"python-dynamic-call": 1}));
}

#[test]
fn test_blind_spot_warning_upstream_target_file_only_omitted() {
    let repo = tempfile::tempdir().unwrap();
    let (mut fx, _) = one_def(repo.path(), &[("src/quiet.py", "y = 1\n")]);
    fx.blind_spot("python-exec", "src/lib.py", (0, 0, 0, 3), "exec");
    let payload = impact(fx, repo.path(), |_| {});
    assert!(
        payload.get("blind_spot_warning").is_none(),
        "a blind spot in the target's own file hides no caller: {payload}"
    );
}

#[test]
fn test_blind_spot_warning_downstream_keeps_target_file() {
    let repo = tempfile::tempdir().unwrap();
    let payload = impact(spotted(repo.path()), repo.path(), |a| {
        a.direction = Direction::Down
    });
    let warning = &payload["blind_spot_warning"];
    assert_eq!(warning["files"], json!(["src/lib.py"]), "{payload}");
    assert_eq!(warning["by_kind"], json!({"python-exec": 1}));
}

#[test]
fn test_blind_spot_warning_both_is_the_union() {
    let repo = tempfile::tempdir().unwrap();
    let payload = impact(spotted(repo.path()), repo.path(), |a| {
        a.direction = Direction::Both
    });
    let warning = &payload["blind_spot_warning"];
    assert_eq!(
        warning["files"],
        json!(["src/lib.py", "src/use.py"]),
        "{payload}"
    );
    assert_eq!(warning["total"], 2);
}
