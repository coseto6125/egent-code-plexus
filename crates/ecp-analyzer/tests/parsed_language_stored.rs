//! C5 open point 7: `graph.bin` stores `ParsedLanguage` per file, and the
//! index-side readers read the stored value instead of re-deriving the
//! language from the path. The script language of a Vue, Svelte or Astro
//! file and the language a content gate accepted are not in the path.
//!
//! Two kinds of test. The builder-level tests set `LocalGraph::parsed_language`
//! by hand on a graph the TypeScript provider parsed at a `.vue` path: that
//! is the only way to give a Vue file a *caller*, because the SFC parsers
//! emit no calls (`sfc_common.rs` `parse_embedded_script`, C5 open point 13).
//! The real-parse tests run `AnalyzerPipeline::parse_file_raw` on a Vue file,
//! so the stored value is what the parse reports. Each SFC test compares the
//! `.vue` file with the same source at a `.ts` path: the stored language must
//! make the index read them alike.
mod instantiation_calls_support;

use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_analyzer::vue::parser::VueProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::routing::{FileRouter, Language, ParsedLanguage};
use ecp_core::file_category::{FileMeta, Language as LegacyLanguage};
use ecp_core::graph::{ArchivedZeroCopyGraph, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, imports_edges_of, Hit};
use std::path::Path;
use std::sync::OnceLock;

/// The pipeline that parses the real-parse tests' files.
fn pipeline() -> &'static AnalyzerPipeline {
    static PIPELINE: OnceLock<AnalyzerPipeline> = OnceLock::new();
    PIPELINE.get_or_init(|| {
        let mut pipeline = AnalyzerPipeline::new();
        pipeline.register_provider(Box::new(TypeScriptProvider::new().unwrap()));
        pipeline.register_provider(Box::new(PythonProvider::new().unwrap()));
        pipeline.register_provider(Box::new(VueProvider::new().unwrap()));
        pipeline
    })
}

fn parse_raw(path: &str, source: &str) -> LocalGraph {
    pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

/// A Vue component whose only block is a TypeScript script.
fn vue_component(script: &str) -> String {
    format!("<script lang=\"ts\">\n{script}</script>\n")
}

/// `graph` with the language its parse would have decided.
fn parsed_as(mut graph: LocalGraph, language: ParsedLanguage) -> LocalGraph {
    graph.parsed_language = language.to_bytes();
    graph
}

/// The language `FileRoute::after_parse` decides for `path`.
fn parsed(path: &str, accepted: Language, script: Option<Language>) -> ParsedLanguage {
    FileRouter::PATH_ONLY
        .route(path)
        .route()
        .unwrap_or_else(|| panic!("{path} routes"))
        .after_parse(accepted, script)
        .unwrap_or_else(|e| panic!("{path}: {e:?}"))
}

fn vue_ts(path: &str) -> ParsedLanguage {
    parsed(path, Language::Vue, Some(Language::TypeScript))
}

fn parse(provider: &dyn LanguageProvider, path: &str, source: &str) -> LocalGraph {
    provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn archive(graph: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(graph).expect("archive graph")
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).expect("access graph")
}

/// The stored language of the file at `path`, read back from `graph.bin`.
fn stored(archived: &ArchivedZeroCopyGraph, path: &str) -> Option<ParsedLanguage> {
    let idx = archived
        .file_idx_of(path)
        .unwrap_or_else(|| panic!("{path} has a File record"));
    archived.parsed_language(idx)
}

/// `Calls` edges from `caller` as `(file, name, confidence)`.
fn call_targets(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, String, f32)> {
    calls_from(graph, caller)
        .into_iter()
        .map(
            |Hit {
                 file,
                 name,
                 confidence,
                 ..
             }| (file, name, confidence),
        )
        .collect()
}

const CALLER: &str = "export function setup() { return helper(); }\n";
const TS_HELPER: (&str, &str) = ("src/util.ts", "export function helper() { return 1; }\n");

/// The caller source at `caller_path` beside `candidate`, the caller stored
/// as `caller_language` when given.
fn caller_graph(
    caller_path: &str,
    caller_language: Option<ParsedLanguage>,
    candidate: (&dyn LanguageProvider, &str, &str),
    caller_source: &str,
) -> ZeroCopyGraph {
    let ts = TypeScriptProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    let caller = parse(&ts, caller_path, caller_source);
    builder.add_graph(match caller_language {
        Some(language) => parsed_as(caller, language),
        None => caller,
    });
    let (provider, path, source) = candidate;
    builder.add_graph(parse(provider, path, source));
    builder.build()
}

#[test]
fn test_global_tier_vue_ts_script_admits_ts_candidate() {
    let ts = TypeScriptProvider::new().unwrap();
    let candidate = (&ts as &dyn LanguageProvider, TS_HELPER.0, TS_HELPER.1);
    let control = call_targets(
        &caller_graph("src/app.ts", None, candidate, CALLER),
        "setup",
    );
    assert!(
        control
            .iter()
            .any(|(file, name, _)| file == TS_HELPER.0 && name == "helper"),
        "a .ts caller binds the .ts helper at the global tier: {control:?}"
    );
    let vue = call_targets(
        &caller_graph(
            "src/App.vue",
            Some(vue_ts("src/App.vue")),
            candidate,
            CALLER,
        ),
        "setup",
    );
    assert_eq!(
        vue, control,
        "a Vue file with a TS script binds as a .ts file"
    );
}

#[test]
fn test_global_tier_vue_ts_script_rejects_python_candidate() {
    let py = PythonProvider::new().unwrap();
    let candidate = (
        &py as &dyn LanguageProvider,
        "src/util.py",
        "def helper():\n    return 1\n",
    );
    let vue = call_targets(
        &caller_graph(
            "src/App.vue",
            Some(vue_ts("src/App.vue")),
            candidate,
            CALLER,
        ),
        "setup",
    );
    assert!(
        vue.iter().all(|(_, name, _)| name != "helper"),
        "a TS script never binds a Python declaration: {vue:?}"
    );
}

/// C5 Consumers row 4: the `Imports` pass and the import tier read the
/// stored language, so a Vue file's TS script binds its import as a .ts file
/// does: the same `Imports` edges and the same `Calls` tier.
#[test]
fn test_imports_edges_vue_ts_script_binds_like_ts_importer() {
    let ts = TypeScriptProvider::new().unwrap();
    let candidate = (&ts as &dyn LanguageProvider, TS_HELPER.0, TS_HELPER.1);
    let importer =
        "import { helper } from './util';\nexport function setup() { return helper(); }\n";
    let control = caller_graph("src/app.ts", None, candidate, importer);
    let vue = caller_graph(
        "src/App.vue",
        Some(vue_ts("src/App.vue")),
        candidate,
        importer,
    );
    let control_imports = imports_edges_of(&control, "app.ts");
    assert!(
        !control_imports.is_empty(),
        "the .ts importer has an Imports edge"
    );
    assert_eq!(
        imports_edges_of(&vue, "App.vue"),
        control_imports,
        "Imports edges of the Vue file"
    );
    assert_eq!(
        call_targets(&vue, "setup"),
        call_targets(&control, "setup"),
        "Calls of the Vue file"
    );
}

/// C5 Consumers rows 1 and 2: Pass 1's per-file language (export recording,
/// owner rules) is the stored one. A Vue file whose TS script exports a
/// function is an ES module, as the same .ts file is.
#[test]
fn test_file_flags_vue_ts_script_is_es_module_like_ts() {
    let ts = TypeScriptProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(parsed_as(
        parse(&ts, "src/App.vue", CALLER),
        vue_ts("src/App.vue"),
    ));
    builder.add_graph(parse(&ts, "src/app.ts", CALLER));
    let bytes = archive(&builder.build());
    let archived = access(&bytes);
    let flag = |path: &str| archived.is_es_module_file(archived.file_idx_of(path).unwrap());
    assert!(flag("src/app.ts"), "the .ts file is an ES module");
    assert!(
        flag("src/App.vue"),
        "the Vue file with a TS script is an ES module"
    );
}

#[test]
fn test_graph_bin_file_parsed_language_round_trip() {
    let mut builder = GraphBuilder::new();
    let file = |path: &str| LocalGraph {
        file_path: path.into(),
        ..LocalGraph::default()
    };
    let mainstream: Vec<(String, Language)> = Language::MAINSTREAM
        .iter()
        .map(|&language| (format!("src/a.{}", language.extensions()[0]), language))
        .collect();
    for (path, _) in &mainstream {
        builder.add_graph(file(path));
    }
    let given = [
        ("web/App.vue", vue_ts("web/App.vue")),
        (
            "web/Card.svelte",
            parsed(
                "web/Card.svelte",
                Language::Svelte,
                Some(Language::JavaScript),
            ),
        ),
        (
            "web/Plain.vue",
            parsed("web/Plain.vue", Language::Vue, None),
        ),
        (
            "api/openapi.yaml",
            parsed("api/openapi.yaml", Language::OpenApi, None),
        ),
    ];
    for (path, language) in given {
        builder.add_graph(parsed_as(file(path), language));
    }
    // No parse decided these: a Vue file whose script no parser reported,
    // and a YAML file whose content gate is unknown.
    builder.add_graph(file("web/Unreported.vue"));
    builder.add_graph(file("config/app.yaml"));

    let bytes = archive(&builder.build());
    let archived = access(&bytes);
    for (path, language) in &mainstream {
        let read = stored(archived, path).unwrap_or_else(|| panic!("{path} stores a language"));
        assert_eq!(read.file(), *language, "file language of {path}");
        assert_eq!(read.resolution(), read.file(), "resolution of {path}");
    }
    for (path, language) in given {
        assert_eq!(stored(archived, path), Some(language), "{path}");
    }
    let vue = stored(archived, "web/App.vue").unwrap();
    assert_eq!(
        (vue.file(), vue.resolution()),
        (Language::Vue, Language::TypeScript)
    );
    assert_eq!(stored(archived, "web/Unreported.vue"), None);
    assert_eq!(stored(archived, "config/app.yaml"), None);
    assert_eq!(archived.parsed_language(archived.files.len() as u32), None);
}

/// Two graphs of one path (they do occur) come from one parse, so every
/// `File` record of the path reads the language that parse reported.
#[test]
fn test_add_graph_duplicate_path_every_record_reads_parsed() {
    let source = vue_component(CALLER);
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse_raw("src/App.vue", &source));
    builder.add_graph(parse_raw("src/App.vue", &source));
    let graph = builder.build();
    let records: Vec<_> = graph
        .files
        .iter()
        .map(|file| ParsedLanguage::from_bytes(file.parsed_language))
        .collect();
    assert_eq!(records, [Some(vue_ts("src/App.vue")); 2]);
}

#[test]
fn test_parsed_language_bytes_round_trip_every_routed_language() {
    for &language in Language::ALL {
        let Some(extension) = language.extensions().first() else {
            continue;
        };
        let path = format!("src/a.{extension}");
        let outcome = FileRouter::PATH_ONLY.route(&path);
        let Some(route) = outcome.route() else {
            continue;
        };
        for &accepted in route.candidates() {
            let scripts =
                std::iter::once(None).chain(accepted.embedded_scripts().iter().map(|s| Some(*s)));
            for script in scripts {
                let value = route.after_parse(accepted, script).unwrap();
                assert_eq!(
                    ParsedLanguage::from_bytes(value.to_bytes()),
                    Some(value),
                    "{path}"
                );
            }
        }
    }
}

#[test]
fn test_parsed_language_from_bytes_invalid_pairs_none() {
    let byte = |language: Language| language as u8;
    for bytes in [
        ParsedLanguage::ABSENT,
        [byte(Language::TypeScript), byte(Language::Vue)],
        [byte(Language::Rust), byte(Language::Python)],
        [byte(Language::Vue), byte(Language::Python)],
        [u8::MAX, byte(Language::Rust)],
        [byte(Language::Rust), u8::MAX],
    ] {
        assert_eq!(ParsedLanguage::from_bytes(bytes), None, "{bytes:?}");
    }
    assert_eq!(ParsedLanguage::encode(None), ParsedLanguage::ABSENT);
}

/// C5 "The compatibility rule": `FileMeta::admits` reads the language after
/// parse on both files. Before it, `Unknown == Unknown` admitted every
/// `.vue`, `.svelte`, `.astro`, `.proto` and `.json` pairing.
#[test]
fn test_file_meta_admits_reads_parsed_language_on_both_files() {
    let vue = FileMeta::of_parsed("src/App.vue", vue_ts("src/App.vue"));
    assert_eq!(vue.language, LegacyLanguage::TypeScript);
    assert!(vue.admits(FileMeta::from_path("src/util.ts")));
    assert!(!vue.admits(FileMeta::from_path("src/util.js")));
    assert!(!vue.admits(FileMeta::from_path("src/util.py")));
    assert!(
        !vue.admits(FileMeta::from_path("vendor/util.ts")),
        "vendor barrier"
    );
    // A path alone gives a component no script language and a `.proto`
    // file Protobuf: they no longer pair.
    assert!(!FileMeta::from_path("src/App.vue").admits(FileMeta::from_path("api/svc.proto")));
    assert!(!FileMeta::from_path("api/svc.proto").admits(FileMeta::from_path("api/schema.json")));
    // Two files with no language after parse keep the path rule; two files
    // of one language after parse admit each other.
    assert!(FileMeta::from_path("src/App.vue").admits(FileMeta::from_path("src/Other.vue")));
    assert!(FileMeta::from_path("api/svc.proto").admits(FileMeta::from_path("api/other.proto")));
}

const TS_HELPER_SOURCE: &str = "export function helper() { return 1; }\n";

/// The index of `files`, each parsed by `AnalyzerPipeline::parse_file_raw`.
fn index_of(files: &[(&str, String)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (path, source) in files {
        builder.add_graph(parse_raw(path, source));
    }
    builder.build()
}

/// The callee side of the global tier through the real parse: the SFC
/// parsers emit no calls, so a Vue file cannot be the caller of a real
/// parse, but a TS caller reaches a declaration of the Vue script. The
/// confidence is the `.ts` control's: the same tier.
#[test]
fn test_global_tier_ts_caller_binds_vue_ts_script_declaration() {
    let edge_to = |helper: (&str, String)| {
        let graph = index_of(&[("src/app.ts", CALLER.to_string()), helper]);
        call_targets(&graph, "setup")
            .into_iter()
            .filter(|(_, name, _)| name == "helper")
            .collect::<Vec<_>>()
    };
    let control = edge_to(("src/util.ts", TS_HELPER_SOURCE.to_string()));
    assert_eq!(
        control.len(),
        1,
        "the .ts caller binds the .ts helper: {control:?}"
    );
    let vue = edge_to(("src/Util.vue", vue_component(TS_HELPER_SOURCE)));
    assert_eq!(
        vue,
        [(
            "src/Util.vue".to_string(),
            "helper".to_string(),
            control[0].2
        )],
        "a TS script declaration binds at the .ts tier"
    );
}

/// TypeScript and JavaScript stay apart at the global tier (C5 open point 5):
/// a Vue file with a JS script is a JavaScript candidate.
#[test]
fn test_global_tier_ts_caller_skips_vue_js_script_declaration() {
    let component = format!("<script lang=\"js\">\n{TS_HELPER_SOURCE}</script>\n");
    let graph = index_of(&[
        ("src/app.ts", CALLER.to_string()),
        ("src/Util.vue", component),
    ]);
    let hits = call_targets(&graph, "setup");
    assert!(
        hits.iter().all(|(_, name, _)| name != "helper"),
        "a JS script never binds at a TS caller's global tier: {hits:?}"
    );
}

/// C5 Consumers row 4 through the real parse: the `Imports` pass reads the
/// language the Vue parse reported.
#[test]
fn test_imports_edges_real_vue_ts_script_binds_like_ts_importer() {
    let importer =
        "import { helper } from './util';\nexport function setup() { return helper(); }\n";
    let util = ("src/util.ts", TS_HELPER_SOURCE.to_string());
    let control = index_of(&[("src/app.ts", importer.to_string()), util.clone()]);
    let vue = index_of(&[("src/App.vue", vue_component(importer)), util]);
    let control_imports = imports_edges_of(&control, "app.ts");
    assert!(
        !control_imports.is_empty(),
        "the .ts importer has an Imports edge"
    );
    assert_eq!(imports_edges_of(&vue, "App.vue"), control_imports);
}

/// C5 Consumers rows 1 and 2 through the real parse: a Vue file whose TS
/// script exports is an ES module, one that exports nothing is not, as the
/// same source in a `.ts` file.
#[test]
fn test_file_flags_real_vue_ts_script_is_es_module_like_ts() {
    for (script, expected) in [
        ("export function setup() { return 1; }\n", true),
        ("function setup() { return 1; }\n", false),
    ] {
        let graph = index_of(&[
            ("src/app.ts", script.to_string()),
            ("src/App.vue", vue_component(script)),
        ]);
        let bytes = archive(&graph);
        let archived = access(&bytes);
        let flag = |path: &str| archived.is_es_module_file(archived.file_idx_of(path).unwrap());
        assert_eq!(flag("src/app.ts"), expected, "{script}");
        assert_eq!(flag("src/App.vue"), expected, "{script}");
    }
}

const DEFAULT_IMPORTER: &str =
    "import Comp from './Comp.vue';\nexport function run() { return Comp(); }\n";

type ImportEdge = (String, String, &'static str, u32);
type CallEdge = (String, String, f32);

/// `Imports` and `Calls` edges of the importer of a component whose script
/// is `script`, with the component's file name read as `.ts`, so the Vue
/// file and its `.ts` control compare equal.
fn default_import_edges(
    component: (&str, String),
    importer: &str,
) -> (Vec<ImportEdge>, Vec<CallEdge>) {
    let graph = index_of(&[("src/main.ts", importer.to_string()), component]);
    let normalize = |file: String| file.replace(".vue", ".ts");
    let imports = imports_edges_of(&graph, "main.ts")
        .into_iter()
        .map(|(file, name, kind, confidence)| (normalize(file), name, kind, confidence))
        .collect();
    let calls = call_targets(&graph, "run")
        .into_iter()
        .map(|(file, name, confidence)| (normalize(file), name, confidence))
        .collect();
    (imports, calls)
}

/// Unmasked consumer: `import Comp from './Comp.vue'`. Before the Vue file
/// stored its script language it was no ES module, so the default import
/// fell back to a by-name lookup and bound the private `function Comp`.
/// With a TS script that exports something, the file is an ES module and
/// the default import binds an exported declaration only: the SFC parser
/// emits no `default` export, so the import binds nothing. A `.ts` file of
/// the same source is the control.
#[test]
fn test_default_import_vue_with_exports_requires_exported_declaration() {
    let script = "export function helper() { return 1; }\nfunction Comp() { return 2; }\n";
    let control = default_import_edges(
        ("src/Comp.ts", script.to_string()),
        &DEFAULT_IMPORTER.replace("Comp.vue", "Comp"),
    );
    let mut vue = default_import_edges(("src/Comp.vue", vue_component(script)), DEFAULT_IMPORTER);
    // Both bind the target's File node, whose name is the file's own name.
    for (_, name, kind, _) in &mut vue.0 {
        if *kind == "File" && name == "Comp.vue" {
            *name = "Comp.ts".to_string();
        }
    }
    assert_eq!(
        vue, control,
        "the Vue file binds a default import as a .ts file"
    );
    assert!(
        vue.0.iter().all(|(_, name, _, _)| name != "Comp"),
        "the private Comp is no longer bound by the default import: {:?}",
        vue.0
    );
}

/// A script with no export keeps the by-name fallback, as before: the file
/// is no ES module, in `.vue` and in `.ts` alike.
#[test]
fn test_default_import_vue_script_without_exports_binds_by_name() {
    let script = "function Comp() { return 2; }\n";
    let control = default_import_edges(
        ("src/Comp.ts", script.to_string()),
        &DEFAULT_IMPORTER.replace("Comp.vue", "Comp"),
    );
    let vue = default_import_edges(("src/Comp.vue", vue_component(script)), DEFAULT_IMPORTER);
    assert_eq!(
        vue, control,
        "the Vue file binds a default import as a .ts file"
    );
    assert!(
        vue.0
            .iter()
            .any(|(file, name, _, _)| file == "src/Comp.ts" && name == "Comp"),
        "the by-name fallback binds Comp: {:?}",
        vue.0
    );
}
