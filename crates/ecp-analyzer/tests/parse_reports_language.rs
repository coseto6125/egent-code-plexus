//! C5 `FileRoute::after_parse` through the real parse: the pipeline stores the
//! language the parse reported in `LocalGraph::parsed_language`, and
//! `graph.bin` keeps it per file (open point 7).
//!
//! The contract under test: the file language is the provider that accepted
//! the content, and the resolution language is the embedded script the SFC
//! parser read (open point 9: TypeScript if any block is TypeScript, else
//! JavaScript, else the component itself). A file no parse decided stores
//! `ParsedLanguage::ABSENT`.
//!
//! Exemptions, named with their source line:
//! - Astro has no JavaScript script and no mixed-language case: the
//!   frontmatter is always parsed as TypeScript (`astro/parser.rs:27-29`),
//!   and a client `<script>` only gets a `Section` (`astro/parser.rs:126-139`),
//!   so the js and mixed variants of the four SFC tests do not exist for it.

use ecp_analyzer::astro::parser::AstroProvider;
use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::kubernetes::parser::KubernetesProvider;
use ecp_analyzer::openapi::OpenApiProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::svelte::parser::SvelteProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_analyzer::vue::parser::VueProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::routing::{Language, ParsedLanguage, SizeCap};
use ecp_core::graph::{ArchivedZeroCopyGraph, ZeroCopyGraph};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Every provider the tests below route to, registered once: building the
/// tree-sitter queries is the cost of a test run.
fn pipeline() -> &'static AnalyzerPipeline {
    static PIPELINE: OnceLock<AnalyzerPipeline> = OnceLock::new();
    PIPELINE.get_or_init(|| {
        let mut pipeline = AnalyzerPipeline::new();
        macro_rules! register {
            ($($provider:expr),+ $(,)?) => {
                $(pipeline.register_provider(Box::new($provider.expect("provider"))));+
            };
        }
        register!(
            TypeScriptProvider::new(),
            JavaScriptProvider::new(),
            PythonProvider::new(),
            JavaProvider::new(),
            KotlinProvider::new(),
            CSharpProvider::new(),
            GoProvider::new(),
            RustProvider::new(),
            PhpProvider::new(),
            RubyProvider::new(),
            SwiftProvider::new(),
            CProvider::new(),
            CppProvider::new(),
            DartProvider::new(),
            VueProvider::new(),
            SvelteProvider::new(),
            AstroProvider::new(),
            KubernetesProvider::new(),
            OpenApiProvider::new(),
        );
        pipeline
    })
}

fn parse(path: &str, source: &str) -> LocalGraph {
    pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

/// `(file, resolution)` the parse stored, `None` for `ABSENT`.
fn reported(graph: &LocalGraph) -> Option<(Language, Language)> {
    ParsedLanguage::from_bytes(graph.parsed_language).map(|p| (p.file(), p.resolution()))
}

fn language_of(path: &str, source: &str) -> Option<(Language, Language)> {
    reported(&parse(path, source))
}

fn mismatch_spots(graph: &LocalGraph) -> usize {
    graph
        .blind_spots
        .iter()
        .filter(|spot| spot.kind == "sfc-script-mismatch")
        .count()
}

const VUE_TS: &str = "<script lang=\"ts\">\nexport const a: number = 1;\n</script>\n";
const VUE_JS: &str = "<script lang=\"js\">\nexport const a = 1;\n</script>\n";
const VUE: (Language, Language) = (Language::Vue, Language::Vue);
const VUE_AS_TS: (Language, Language) = (Language::Vue, Language::TypeScript);
const VUE_AS_JS: (Language, Language) = (Language::Vue, Language::JavaScript);

// -- Vue ----------------------------------------------------------------

/// Fails on HEAD: nothing sets the field, so it is `ABSENT`.
#[test]
fn test_parse_vue_ts_script_reports_typescript() {
    assert_eq!(language_of("src/App.vue", VUE_TS), Some(VUE_AS_TS));
}

/// Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_vue_js_script_reports_javascript() {
    assert_eq!(language_of("src/App.vue", VUE_JS), Some(VUE_AS_JS));
}

/// Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_vue_no_script_reports_vue() {
    assert_eq!(
        language_of("src/App.vue", "<template><div/></template>\n"),
        Some(VUE)
    );
}

/// Open point 9: TypeScript if any block is TypeScript, in either order, and
/// every block that differs from the language so far names the file.
/// Fails on HEAD: `ABSENT` and no blind spot.
#[test]
fn test_parse_vue_ts_and_js_blocks_reports_typescript() {
    let ts_then_js = format!("{VUE_TS}<script setup lang=\"js\">\nconst b = 2;\n</script>\n");
    let js_then_ts = format!("<script setup lang=\"js\">\nconst b = 2;\n</script>\n{VUE_TS}");
    for source in [ts_then_js, js_then_ts] {
        let graph = parse("src/App.vue", &source);
        assert_eq!(reported(&graph), Some(VUE_AS_TS), "{source}");
        assert_eq!(mismatch_spots(&graph), 1, "{source}");
    }
}

/// Two blocks of one language are no mismatch. Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_vue_two_ts_blocks_reports_typescript_without_blind_spot() {
    let source = format!("{VUE_TS}<script setup lang=\"ts\">\nconst b = 2;\n</script>\n");
    let graph = parse("src/App.vue", &source);
    assert_eq!(reported(&graph), Some(VUE_AS_TS));
    assert_eq!(mismatch_spots(&graph), 0);
}

/// A block with no `lang` is parsed as TypeScript (`vue/parser.rs:174-178`),
/// so it reports TypeScript. Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_vue_script_without_lang_reports_typescript() {
    assert_eq!(
        language_of("src/App.vue", "<script>\nexport const a = 1;\n</script>\n"),
        Some(VUE_AS_TS)
    );
}

/// `lang='ts'` with single quotes. Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_vue_single_quoted_lang_reports_typescript() {
    assert_eq!(
        language_of(
            "src/App.vue",
            "<script lang='ts'>\nexport const a = 1;\n</script>\n"
        ),
        Some(VUE_AS_TS)
    );
}

/// An empty block still declares its language. Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_vue_empty_ts_script_reports_typescript() {
    assert_eq!(
        language_of("src/App.vue", "<script lang=\"ts\"></script>\n"),
        Some(VUE_AS_TS)
    );
}

/// The stored resolution language is the grammar that parsed the block:
/// `detect_lang_ts` accepts `ts` and `typescript` only (`vue/parser.rs:278`),
/// so `tsx` is parsed by the JavaScript grammar and reports JavaScript. When
/// `tsx` moves to a TypeScript grammar, this test moves with it. Fails on
/// HEAD: `ABSENT`.
#[test]
fn test_parse_vue_tsx_script_reports_the_grammar_that_parsed_it() {
    assert_eq!(
        language_of(
            "src/App.vue",
            "<script lang=\"tsx\">\nexport const a = 1;\n</script>\n"
        ),
        Some(VUE_AS_JS)
    );
}

/// A `<script>` inside a template comment is no block. Fails on HEAD:
/// `ABSENT`.
#[test]
fn test_parse_vue_script_in_template_comment_reports_vue() {
    let source = "<template><!-- <script lang=\"ts\">const a = 1;</script> --></template>\n";
    assert_eq!(language_of("src/App.vue", source), Some(VUE));
}

/// Empty and whitespace-only files have no script. Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_vue_empty_or_blank_file_reports_vue() {
    for source in ["", "   \n\t\n"] {
        assert_eq!(language_of("src/App.vue", source), Some(VUE), "{source:?}");
    }
}

// -- Svelte -------------------------------------------------------------

#[test]
fn test_parse_svelte_ts_script_reports_typescript() {
    assert_eq!(
        language_of(
            "web/Card.svelte",
            "<script lang=\"ts\">\n  export let n: number = 1;\n</script>\n<p>{n}</p>\n"
        ),
        Some((Language::Svelte, Language::TypeScript))
    );
}

#[test]
fn test_parse_svelte_js_script_reports_javascript() {
    assert_eq!(
        language_of(
            "web/Card.svelte",
            "<script lang=\"js\">\n  export let n = 1;\n</script>\n<p>{n}</p>\n"
        ),
        Some((Language::Svelte, Language::JavaScript))
    );
}

#[test]
fn test_parse_svelte_no_script_reports_svelte() {
    assert_eq!(
        language_of("web/Card.svelte", "<p>static</p>\n"),
        Some((Language::Svelte, Language::Svelte))
    );
}

/// The module-scope block is a block too: a TS instance block beside a JS
/// `context="module"` block reports TypeScript and one blind spot.
#[test]
fn test_parse_svelte_ts_and_js_blocks_reports_typescript() {
    let source = "<script context=\"module\" lang=\"js\">\n  export const k = 1;\n</script>\n\
                  <script lang=\"ts\">\n  export let n: number = 1;\n</script>\n";
    let graph = parse("web/Card.svelte", source);
    assert_eq!(
        reported(&graph),
        Some((Language::Svelte, Language::TypeScript))
    );
    assert_eq!(mismatch_spots(&graph), 1);
}

// -- Astro --------------------------------------------------------------

#[test]
fn test_parse_astro_frontmatter_reports_typescript() {
    assert_eq!(
        language_of(
            "pages/index.astro",
            "---\nconst title: string = \"x\";\n---\n<h1>{title}</h1>\n"
        ),
        Some((Language::Astro, Language::TypeScript))
    );
}

#[test]
fn test_parse_astro_no_frontmatter_reports_astro() {
    assert_eq!(
        language_of("pages/index.astro", "<h1>static</h1>\n"),
        Some((Language::Astro, Language::Astro))
    );
}

/// A client `<script>` gets a Section and no parse, so it reports no
/// language (`astro/parser.rs:126-139`).
#[test]
fn test_parse_astro_client_script_only_reports_astro() {
    assert_eq!(
        language_of(
            "pages/index.astro",
            "<h1>static</h1>\n<script>\n  console.log(1);\n</script>\n"
        ),
        Some((Language::Astro, Language::Astro))
    );
}

// -- Sniffed routes -----------------------------------------------------

const OPENAPI: &str = "openapi: 3.0.0\ninfo:\n  title: T\n  version: 1.0.0\npaths: {}\ncomponents:\n  schemas:\n    User:\n      type: object\n      properties:\n        email:\n          type: string\n";

/// The gate that accepted is the stored language: Kubernetes rejects, then
/// OpenAPI accepts. Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_openapi_yaml_reports_sniffed_language() {
    assert_eq!(
        language_of("api/openapi.yaml", OPENAPI),
        Some((Language::OpenApi, Language::OpenApi))
    );
}

/// A YAML no gate accepted is `Yaml`. Fails on HEAD: `ABSENT`.
#[test]
fn test_parse_generic_yaml_reports_yaml() {
    assert_eq!(
        language_of("config/app.yaml", "name: app\nport: 80\n"),
        Some((Language::Yaml, Language::Yaml))
    );
}

/// A JSON file no gate accepted has no language.
#[test]
fn test_parse_json_without_openapi_marker_reports_absent() {
    let graph = parse("api/data.json", "{\"a\": 1}\n");
    assert_eq!(graph.parsed_language, ParsedLanguage::ABSENT);
}

// -- Omitted files ------------------------------------------------------

/// An omission carries no language: nothing was parsed.
#[test]
fn test_parse_omitted_file_reports_absent() {
    let mut capped = AnalyzerPipeline::new().with_size_cap(SizeCap::resolve(Some("10"), None));
    capped.register_provider(Box::new(VueProvider::new().unwrap()));
    let big = capped
        .parse_file_raw(Path::new("src/App.vue"), VUE_TS.as_bytes())
        .expect("an oversized routed file is an omission, not an error");
    assert_eq!(big.blind_spots[0].kind, "file-too-large");
    assert_eq!(big.parsed_language, ParsedLanguage::ABSENT);

    let not_utf8 = LocalGraph::omitted(Path::new("src/odd.vue"), "path-not-utf8", String::new());
    assert_eq!(not_utf8.parsed_language, ParsedLanguage::ABSENT);
    assert_eq!(
        LocalGraph::default().parsed_language,
        ParsedLanguage::ABSENT
    );
}

// -- 14 mainstream languages ---------------------------------------------

/// Repo `CLAUDE.md`: a parse change ships with the 14 mainstream languages.
/// For each, the parse reports `file == resolution == the path language`.
/// `.h` is C++ (`routing.rs` `Language::Cpp`).
#[test]
fn test_parse_mainstream_languages_report_the_path_language() {
    let table: [(&str, &str, Language); 15] = [
        ("src/a.ts", "export function f() {}\n", Language::TypeScript),
        ("src/a.js", "function f() {}\n", Language::JavaScript),
        ("src/a.py", "def f():\n    pass\n", Language::Python),
        ("src/A.java", "class A { void f() {} }\n", Language::Java),
        ("src/a.kt", "fun f() {}\n", Language::Kotlin),
        ("src/A.cs", "class A { void F() {} }\n", Language::CSharp),
        ("src/a.go", "package a\nfunc f() {}\n", Language::Go),
        ("src/a.rs", "fn f() {}\n", Language::Rust),
        ("src/a.php", "<?php\nfunction f() {}\n", Language::Php),
        ("src/a.rb", "def f; end\n", Language::Ruby),
        ("src/a.swift", "func f() {}\n", Language::Swift),
        ("src/a.c", "void f(void) {}\n", Language::C),
        ("src/a.cpp", "void f() {}\n", Language::Cpp),
        ("src/a.h", "void f();\n", Language::Cpp),
        ("src/a.dart", "void f() {}\n", Language::Dart),
    ];
    for (path, source, language) in table {
        assert_eq!(
            language_of(path, source),
            Some((language, language)),
            "{path}"
        );
    }
    for language in Language::MAINSTREAM {
        assert!(
            table.iter().any(|(_, _, covered)| *covered == language),
            "{language:?} is in the table"
        );
    }
}

// -- The cold entry -----------------------------------------------------

/// `AnalyzerPipeline::analyze` (the cold walk) sets the field as
/// `parse_file_raw` does; a path listed twice (a concurrent duplicate) reads
/// the same language on both graphs; a file that cannot be read is an
/// omission and stores `ABSENT`. Fails on HEAD: `ABSENT` for the Vue files.
#[test]
fn test_analyze_cold_path_reports_language_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let write = |name: &str, source: &str| {
        let abs = tmp.path().join(name);
        std::fs::write(&abs, source).unwrap();
        (abs, PathBuf::from(name))
    };
    let vue = write("App.vue", VUE_TS);
    let files = vec![
        vue.clone(),
        vue,
        write("a.py", "def f():\n    pass\n"),
        (tmp.path().join("gone.vue"), PathBuf::from("gone.vue")),
    ];
    let graphs = pipeline().analyze(files);

    let of = |name: &str| -> Vec<[u8; 2]> {
        graphs
            .iter()
            .filter(|graph| graph.file_path == Path::new(name))
            .map(|graph| graph.parsed_language)
            .collect()
    };
    let as_ts = language_bytes(VUE_AS_TS);
    assert_eq!(of("App.vue"), [as_ts, as_ts]);
    assert_eq!(
        of("a.py"),
        [language_bytes((Language::Python, Language::Python))]
    );
    assert_eq!(of("gone.vue"), [ParsedLanguage::ABSENT]);
}

fn language_bytes((file, resolution): (Language, Language)) -> [u8; 2] {
    [file as u8, resolution as u8]
}

// -- Stored in graph.bin ------------------------------------------------

fn stored_in(archived: &ArchivedZeroCopyGraph, path: &str) -> Option<(Language, Language)> {
    let idx = archived
        .file_idx_of(path)
        .unwrap_or_else(|| panic!("{path} has a File record"));
    archived
        .parsed_language(idx)
        .map(|parsed| (parsed.file(), parsed.resolution()))
}

/// The pipeline plus `GraphBuilder::add_graph`: the `File` records of an SFC
/// file and a sniffed file store what the parse reported, and `graph.bin`
/// reads it back. Fails on HEAD: every one reads `None`.
#[test]
fn test_graph_bin_vue_ts_script_stored_end_to_end() {
    let files = [
        ("src/App.vue", VUE_TS),
        ("src/Js.vue", VUE_JS),
        (
            "web/Card.svelte",
            "<script lang=\"js\">\n  export let n = 1;\n</script>\n",
        ),
        ("pages/index.astro", "---\nconst t = 1;\n---\n<p>{t}</p>\n"),
        ("src/Plain.vue", "<template><p/></template>\n"),
        ("api/openapi.yaml", OPENAPI),
        ("src/util.ts", "export function helper() {}\n"),
    ];
    let mut builder = GraphBuilder::new();
    for (path, source) in files {
        builder.add_graph(parse(path, source));
    }
    let graph: ZeroCopyGraph = builder.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).expect("archive graph");
    let archived =
        rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).expect("access graph");

    assert_eq!(stored_in(archived, "src/App.vue"), Some(VUE_AS_TS));
    assert_eq!(stored_in(archived, "src/Js.vue"), Some(VUE_AS_JS));
    assert_eq!(
        stored_in(archived, "web/Card.svelte"),
        Some((Language::Svelte, Language::JavaScript))
    );
    assert_eq!(
        stored_in(archived, "pages/index.astro"),
        Some((Language::Astro, Language::TypeScript))
    );
    assert_eq!(stored_in(archived, "src/Plain.vue"), Some(VUE));
    assert_eq!(
        stored_in(archived, "api/openapi.yaml"),
        Some((Language::OpenApi, Language::OpenApi))
    );
    assert_eq!(
        stored_in(archived, "src/util.ts"),
        Some((Language::TypeScript, Language::TypeScript))
    );
}
