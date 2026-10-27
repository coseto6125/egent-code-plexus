//! C5 open point 7, overlay side: the overlay reads the C5 language after
//! parse where the index reads the byte `graph.bin` stores, so a Vue,
//! Svelte or Astro file resolves as its script language on both sides.
//!
//! | Side | Source of the language |
//! |---|---|
//! | clean base file | the `File.parsed_language` record |
//! | dirty file | the fragment (`FragmentFile.parsed_language`) |
//! | new file | the fragment |
//!
//! Each parity test feeds one tree to a fresh index and to the overlay of
//! the dirty copy over the base index, and compares the `Calls` edges (callee
//! file and name, line, confidence). An overlay `Calls` edge carries the
//! uncommitted-edit reason instead of the index's, so the reason is not part
//! of the comparison.
//!
//! The SFC providers emit no calls (`sfc_common.rs`, C5 open point 13), so a
//! Vue or Svelte *caller* is built here the way the analyzer's
//! `parsed_language_stored.rs` builds it: the TypeScript provider parses the
//! script text at the SFC path, and the language is the one the real SFC
//! parse reports for the same source.

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_cli::reanalyze::pipeline_for_path;
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_cli::session::overlay_writer::{
    fragment_file_from_local_graph, write_dirty_fragments_from_graphs,
};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::routing::{FileRouter, Language, ParsedLanguage, SizeCap};
use ecp_core::file_category::Language as LegacyLanguage;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::OverlayView;
use ecp_core::session::{layout, DirtyEvents, SessionMeta};
use parity::{calls_after_edit, named, CallEdge};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const UTIL_TS: (&str, &str) = ("src/util.ts", "export function helper() { return 1; }\n");

fn pipeline_parse(path: &str, source: &str) -> LocalGraph {
    pipeline_for_path(Path::new(path), SizeCap::BUILT_IN)
        .unwrap_or_else(|| panic!("no pipeline for {path}"))
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn is_sfc(path: &str) -> bool {
    [".vue", ".svelte", ".astro"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

/// The text of every `<script>` block of `source`, in order.
fn script_text(source: &str) -> String {
    let mut text = String::new();
    let mut rest = source;
    while let Some(open) = rest.find("<script") {
        let tag = &rest[open..];
        let Some(end_of_tag) = tag.find('>') else {
            break;
        };
        let body = &tag[end_of_tag + 1..];
        let Some(close) = body.find("</script>") else {
            break;
        };
        text.push_str(&body[..close]);
        text.push('\n');
        rest = &body[close + "</script>".len()..];
    }
    text
}

/// The nodes and calls of an SFC's script, at the SFC path.
fn sfc_script_graph(path: &str, source: &str) -> LocalGraph {
    TypeScriptProvider::new()
        .unwrap()
        .parse_file(Path::new(path), script_text(source).as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

/// A file as the real pipeline parses it. An SFC keeps the real parse's
/// reported language and gains the script's calls.
fn parse(path: &str, source: &str) -> LocalGraph {
    if !is_sfc(path) {
        return pipeline_parse(path, source);
    }
    let mut graph = sfc_script_graph(path, source);
    graph.parsed_language = pipeline_parse(path, source).parsed_language;
    graph
}

/// [`parse`] without the stamp: a graph that reports no language, as a
/// provider's output before the pipeline finishes it.
fn parse_unstamped(path: &str, source: &str) -> LocalGraph {
    if is_sfc(path) {
        sfc_script_graph(path, source)
    } else {
        pipeline_parse(path, source)
    }
}

fn targets(edges: &[CallEdge]) -> Vec<(&str, &str)> {
    edges
        .iter()
        .map(|edge| (edge.callee_file.as_str(), edge.callee.as_str()))
        .collect()
}

/// The util module beside one version of `App.vue`.
fn tree(app: &str) -> [(&str, &str); 2] {
    [UTIL_TS, ("src/App.vue", app)]
}

/// `(file, name)` of each edge of [`call_targets`].
fn callees(edges: &[(String, String, u32)]) -> Vec<(&str, &str)> {
    edges
        .iter()
        .map(|(file, name, _)| (file.as_str(), name.as_str()))
        .collect()
}

fn vue_ts(call: &str) -> String {
    format!("<script lang=\"ts\">\nexport function setup() {{ return {call}; }}\n</script>\n")
}

// -- the dirty file is an SFC caller ------------------------------------

/// Stands for: a dirty `.vue` with a TypeScript script binds a function only
/// a clean `.ts` file declares, as the index binds it. Red on HEAD: the
/// overlay reads `Unknown` from the `.vue` path, so no clean `.ts`
/// candidate passes the global tier and the edge is missing.
#[test]
fn test_overlay_vue_ts_script_global_tier_matches_index() {
    let before = vue_ts("1");
    let after = vue_ts("helper()");
    let parity = calls_after_edit(
        &parse,
        &[UTIL_TS, ("src/App.vue", before.as_str())],
        &[("src/App.vue", after.as_str())],
        &named("setup"),
    );
    assert_eq!(
        targets(&parity.reindex),
        [("src/util.ts", "helper")],
        "the index binds the .ts helper for the Vue caller"
    );
    assert_eq!(parity.overlay, parity.reindex);
}

/// Stands for: the same edge through the fragment bin, not through the
/// direct `LocalGraph` conversion: the writer stores the language, the
/// reader returns it, and the overlay built from the decoded input agrees
/// with the index. Red on HEAD for the same reason as the direct test.
#[test]
fn test_overlay_vue_ts_fragment_round_trip_global_tier_matches_index() {
    let before = vue_ts("1");
    let after = vue_ts("helper()");
    let reindex = call_targets(&index_bytes(&tree(&after)), None, "setup");
    assert_eq!(
        callees(&reindex),
        [("src/util.ts", "helper")],
        "the index binds the .ts helper for the Vue caller"
    );

    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), &worktree, "vue-ts-sid");
    let mtime = write_worktree_file(&worktree, "src/App.vue", &after);
    write_dirty_fragments_from_graphs(&session_dir, &[parse("src/App.vue", &after)], &[mtime])
        .unwrap();
    let (accepted, _) = accepted_fragments(&session_dir, &worktree).unwrap();
    let inputs = decode_view_inputs(&accepted);
    assert_eq!(inputs.len(), 1);

    let base = index_bytes(&tree(&before));
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&base).unwrap();
    let view = OverlayView::build(archived, &inputs).unwrap();
    let overlay = call_targets(&base, Some(&view), "setup");
    assert_eq!(overlay, reindex);
}

/// Stands for: the edit changes `lang="ts"` to `lang="js"`. The overlay
/// follows the new language: the JS caller binds the clean `.js` helper and
/// no longer the `.ts` one, as the index does. The base record of `App.vue`
/// still says TypeScript; the fragment wins. Red on HEAD: no candidate
/// passes for a `.vue` path.
#[test]
fn test_overlay_vue_script_lang_changed_ts_to_js() {
    let ts_only =
        "<script lang=\"ts\">\nexport function setup() { return tsHelper(); }\n</script>\n";
    let js_only =
        "<script lang=\"js\">\nexport function setup() { return tsHelper() + jsHelper(); }\n</script>\n";
    let parity = calls_after_edit(
        &parse,
        &[
            ("src/util.ts", "export function tsHelper() { return 1; }\n"),
            ("src/lib.js", "export function jsHelper() { return 2; }\n"),
            ("src/App.vue", ts_only),
        ],
        &[("src/App.vue", js_only)],
        &named("setup"),
    );
    assert_eq!(
        targets(&parity.reindex),
        [("src/lib.js", "jsHelper")],
        "a JavaScript script reaches the .js helper and not the .ts one"
    );
    assert_eq!(parity.overlay, parity.reindex);
}

/// Stands for: a new `.svelte` file with a JavaScript script has no base
/// record at all; the fragment's language decides, as the index's parse does.
/// Red on HEAD: the path gives the Svelte file no script language.
#[test]
fn test_overlay_new_svelte_js_script_matches_index() {
    let component =
        "<script lang=\"js\">\nexport function setup() { return helper(); }\n</script>\n";
    let parity = calls_after_edit(
        &parse,
        &[("src/util.js", "export function helper() { return 1; }\n")],
        &[("src/Comp.svelte", component)],
        &named("setup"),
    );
    assert_eq!(targets(&parity.reindex), [("src/util.js", "helper")]);
    assert_eq!(parity.overlay, parity.reindex);
}

/// Stands for: a component with two script blocks of different languages
/// resolves as TypeScript (C5 open point 9) on both sides.
#[test]
fn test_overlay_vue_two_script_blocks_mixed_languages_matches_index() {
    let before = "<script lang=\"js\">\nexport const base = 1;\n</script>\n<script setup lang=\"ts\">\nexport function setup() { return 1; }\n</script>\n";
    let after = "<script lang=\"js\">\nexport const base = 1;\n</script>\n<script setup lang=\"ts\">\nexport function setup() { return helper(); }\n</script>\n";
    let parity = calls_after_edit(
        &parse,
        &[UTIL_TS, ("src/Mixed.vue", before)],
        &[("src/Mixed.vue", after)],
        &named("setup"),
    );
    assert_eq!(targets(&parity.reindex), [("src/util.ts", "helper")]);
    assert_eq!(parity.overlay, parity.reindex);
}

/// Stands for: a graph that reports no language reads from its path on both
/// sides, as it did before the language was stored. A `.ts` caller finds no
/// candidate in an unstamped `.vue` file in the index (a path-less language
/// pairs with nothing), and none in the overlay. Green on HEAD and after: the
/// existing path is unchanged.
#[test]
fn test_overlay_unstamped_vue_candidate_reads_path_like_index() {
    let component = "<script lang=\"ts\">\nexport function helper() { return 1; }\n</script>\n";
    let parity = calls_after_edit(
        &parse_unstamped,
        &[
            ("src/App.vue", component),
            ("src/app.ts", "export function run() { return 1; }\n"),
        ],
        &[("src/app.ts", "export function run() { return helper(); }\n")],
        &named("run"),
    );
    assert_eq!(parity.overlay, parity.reindex);
    assert!(
        targets(&parity.reindex).is_empty(),
        "an unstamped Vue candidate pairs with no .ts caller: {:?}",
        parity.reindex
    );
}

// -- the candidate is an SFC ----------------------------------------------

/// Stands for: a dirty `.ts` caller binds a function only a clean `.vue`
/// file (TypeScript script) declares. The clean candidate's language is the
/// stored record, not its path. Red on HEAD: the overlay reads `Unknown`
/// for the `.vue` candidate and the global tier rejects it.
#[test]
fn test_overlay_ts_caller_clean_vue_candidate_matches_index() {
    let component = "<script lang=\"ts\">\nexport function helper() { return 1; }\n</script>\n";
    let parity = calls_after_edit(
        &parse,
        &[
            ("src/App.vue", component),
            ("src/app.ts", "export function run() { return 1; }\n"),
        ],
        &[("src/app.ts", "export function run() { return helper(); }\n")],
        &named("run"),
    );
    assert_eq!(
        targets(&parity.reindex),
        [("src/App.vue", "helper")],
        "the index binds the Vue component's function for the .ts caller"
    );
    assert_eq!(parity.overlay, parity.reindex);
}

/// Stands for: a clean Vue caller keeps its edge when the `.ts` candidate it
/// calls is dirty. The overlay does not re-resolve a clean caller; the base
/// edge follows the candidate to its fresh node. Green on HEAD: this guards
/// that reading the stored language does not disturb a clean caller.
#[test]
fn test_overlay_clean_vue_caller_dirty_ts_candidate_matches_index() {
    let component = vue_ts("helper()");
    let parity = calls_after_edit(
        &parse,
        &[UTIL_TS, ("src/App.vue", component.as_str())],
        &[(
            "src/util.ts",
            "export function extra() { return 0; }\nexport function helper() { return 2; }\n",
        )],
        &named("setup"),
    );
    assert_eq!(targets(&parity.reindex), [("src/util.ts", "helper")]);
    assert_eq!(parity.overlay, parity.reindex);
}

// -- a removed dirty file ---------------------------------------------------

/// Stands for: a deleted Vue file leaves no edge in the overlay, and the
/// view builds although the removed path's base record stores a language.
/// Green on HEAD: removal never read the language.
#[test]
fn test_overlay_removed_vue_file_leaves_no_calls_like_index() {
    let component = vue_ts("helper()");
    let with_app = index_bytes(&[UTIL_TS, ("src/App.vue", component.as_str())]);
    let without_app = index_bytes(&[UTIL_TS]);
    assert_eq!(
        callees(&call_targets(&with_app, None, "setup")),
        [("src/util.ts", "helper")],
        "the base index holds the Vue caller's edge"
    );
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&with_app).unwrap();
    let view = OverlayView::build_with_removed(archived, &[], &["src/App.vue".to_string()])
        .expect("a removed path makes a view");
    assert_eq!(
        call_targets(&with_app, Some(&view), "setup"),
        call_targets(&without_app, None, "setup"),
        "the overlay of the removal equals the index without the file"
    );
    assert!(call_targets(&without_app, None, "setup").is_empty());
}

// -- the fragment payload -----------------------------------------------

/// Stands for: the V7 fragment carries `ParsedLanguage` and the reader
/// returns it unchanged: a Vue file with a TypeScript script (file Vue,
/// resolution TypeScript), a Vue file with no script, and a `.rs` file.
/// Red on HEAD: the fragment holds no language, so the decoded input has
/// none (it does not compile there; the assertion is on `parsed_language`).
#[test]
fn test_fragment_v7_parsed_language_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    let session_dir = make_session_dir(tmp.path(), &worktree, "lang-sid");
    let sources = [
        ("src/Ts.vue", vue_ts("1")),
        ("src/Bare.vue", "<template><div/></template>\n".to_string()),
        ("src/lib.rs", "pub fn a() -> i32 { 1 }\n".to_string()),
    ];
    let graphs: Vec<LocalGraph> = sources
        .iter()
        .map(|(path, source)| pipeline_parse(path, source))
        .collect();
    let mtimes: Vec<u64> = sources
        .iter()
        .map(|(path, source)| write_worktree_file(&worktree, path, source))
        .collect();
    write_dirty_fragments_from_graphs(&session_dir, &graphs, &mtimes).unwrap();
    let (accepted, _) = accepted_fragments(&session_dir, &worktree).unwrap();
    let inputs = decode_view_inputs(&accepted);
    assert_eq!(inputs.len(), 3, "every fragment reads back");
    let read = |path: &str| {
        inputs
            .iter()
            .find(|input| input.rel_path == path)
            .unwrap_or_else(|| panic!("{path} reads back"))
    };

    let ts_vue = read("src/Ts.vue").parsed_language.expect("a language");
    assert_eq!(ts_vue.file(), Language::Vue);
    assert_eq!(ts_vue.resolution(), Language::TypeScript);
    assert_eq!(
        read("src/Ts.vue").file_meta().language,
        LegacyLanguage::TypeScript,
        "the overlay compares a Vue file's script language"
    );

    for (graph, (path, _)) in graphs.iter().zip(&sources) {
        assert_eq!(
            read(path).parsed_language,
            ParsedLanguage::from_bytes(graph.parsed_language),
            "{path}: the fragment returns what the parse reported"
        );
    }

    let rust = read("src/lib.rs").parsed_language.expect("a language");
    let path_language = FileRouter::PATH_ONLY
        .route("src/lib.rs")
        .route()
        .and_then(|route| route.fixed_language());
    assert_eq!(Some(rust.file()), path_language);
    assert_eq!(rust.resolution(), rust.file());
}

/// 14 mainstream languages (repo `CLAUDE.md`): a dirty file's fragment
/// carries `file == resolution ==` the path language, and the overlay's
/// `Calls` edges equal the index's. Rows give the caller's name and the
/// cross-file call; `min_edges` is the floor of the index's edges, set only
/// where the bare cross-file call is known to bind.
#[test]
fn test_overlay_14_languages_fragment_language_and_calls_match_index() {
    // (extension, caller, library source, app source, min_edges)
    let table: [(&str, &str, &str, &str, usize); 14] = [
        (
            "ts",
            "run",
            "export function helper() { return 1; }\n",
            "export function run() { return helper(); }\n",
            1,
        ),
        (
            "js",
            "run",
            "export function helper() { return 1; }\n",
            "export function run() { return helper(); }\n",
            1,
        ),
        (
            "py",
            "run",
            "def helper():\n    return 1\n",
            "def run():\n    return helper()\n",
            0,
        ),
        (
            "java",
            "run",
            "class Lib { static int helper() { return 1; } }\n",
            "class App { int run() { return helper(); } }\n",
            0,
        ),
        (
            "kt",
            "run",
            "fun helper(): Int { return 1 }\n",
            "fun run(): Int { return helper() }\n",
            0,
        ),
        (
            "cs",
            "Run",
            "class Lib { public static int Helper() { return 1; } }\n",
            "class App { int Run() { return Helper(); } }\n",
            0,
        ),
        (
            "go",
            "Run",
            "package lib\n\nfunc Helper() int { return 1 }\n",
            "package app\n\nfunc Run() int { return Helper() }\n",
            0,
        ),
        (
            "rs",
            "run",
            "pub fn helper() -> i32 { 1 }\n",
            "pub fn run() -> i32 { helper() }\n",
            0,
        ),
        (
            "php",
            "run",
            "<?php\nfunction helper() { return 1; }\n",
            "<?php\nfunction run() { return helper(); }\n",
            0,
        ),
        (
            "rb",
            "run",
            "def helper\n  1\nend\n",
            "def run\n  helper()\nend\n",
            0,
        ),
        (
            "swift",
            "run",
            "func helper() -> Int { return 1 }\n",
            "func run() -> Int { return helper() }\n",
            0,
        ),
        (
            "c",
            "run",
            "int helper(void) { return 1; }\n",
            "int run(void) { return helper(); }\n",
            0,
        ),
        (
            "cpp",
            "run",
            "int helper() { return 1; }\n",
            "int run() { return helper(); }\n",
            0,
        ),
        (
            "dart",
            "run",
            "int helper() { return 1; }\n",
            "int run() { return helper(); }\n",
            0,
        ),
    ];
    for (extension, caller, library, app, min_edges) in table {
        let (lib_path, app_path) = (
            format!("src/lib.{extension}"),
            format!("src/app.{extension}"),
        );

        let language = FileRouter::PATH_ONLY
            .route(&app_path)
            .route()
            .and_then(|route| route.fixed_language())
            .unwrap_or_else(|| panic!("{app_path} routes to a fixed language"));
        assert!(Language::MAINSTREAM.contains(&language), "{app_path}");
        let fragment = fragment_file_from_local_graph(&parse(&app_path, app));
        let parsed = ParsedLanguage::from_bytes(fragment.parsed_language)
            .unwrap_or_else(|| panic!("{app_path}: the fragment carries a language"));
        assert_eq!(parsed.file(), language, "{app_path}: file language");
        assert_eq!(parsed.resolution(), language, "{app_path}: resolution");

        let dirty = format!("\n{app}");
        let parity = calls_after_edit(
            &parse,
            &[(lib_path.as_str(), library), (app_path.as_str(), app)],
            &[(app_path.as_str(), dirty.as_str())],
            &named(caller),
        );
        assert_eq!(
            parity.overlay, parity.reindex,
            "{extension}: the overlay's Calls equal the index's"
        );
        assert!(
            parity.reindex.len() >= min_edges,
            "{extension}: the fixture binds its cross-file call: {:?}",
            parity.reindex
        );
    }
}

// -- helpers ------------------------------------------------------------

fn index_bytes(tree: &[(&str, &str)]) -> rkyv::util::AlignedVec {
    let mut builder = GraphBuilder::new();
    for (path, source) in tree {
        builder.add_graph(parse(path, source));
    }
    let graph: ZeroCopyGraph = builder.build();
    rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap()
}

/// `(callee file, callee name, confidence %)` of each `Calls` edge whose
/// caller is `caller`, from the index alone or merged with `view`.
fn call_targets(
    bytes: &[u8],
    view: Option<&OverlayView>,
    caller: &str,
) -> Vec<(String, String, u32)> {
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap();
    let merged = MergedGraph::new(graph, view);
    let mut edges: Vec<(String, String, u32)> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Calls)
        .filter_map(|edge| {
            let source = merged.node(edge.source)?;
            let target = merged.node(edge.target)?;
            (source.name(&merged) == caller).then(|| {
                (
                    target.file_path(&merged).unwrap_or_default().to_string(),
                    target.name(&merged).to_string(),
                    (edge.confidence() * 100.0).round() as u32,
                )
            })
        })
        .collect();
    edges.sort();
    edges.dedup();
    edges
}

/// A session dir whose manifest is empty and current.
/// `worktree` is the session's source worktree: the writer hashes each
/// file there, and the fragment id keys on that hash.
fn make_session_dir(tmp: &Path, worktree: &Path, sid: &str) -> PathBuf {
    let session_dir = tmp.join("sessions").join(sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-06-10T10:00:00Z".into(),
        last_touched: "2026-06-10T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: worktree.to_string_lossy().into_owned(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &meta).unwrap();
    DirtyEvents::write_atomic(
        &layout::dirty_events_path(&session_dir),
        &DirtyEvents::empty(ecp_core::config::resolve_size_cap(worktree)),
    )
    .unwrap();
    session_dir
}

/// Write a real worktree file and return its mtime in nanoseconds: the value
/// the manifest records and the reader checks.
fn write_worktree_file(worktree: &Path, rel: &str, source: &str) -> u64 {
    let path = worktree.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, source).unwrap();
    std::fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
