//! The cold index walk, the provider choice and the size cap read one
//! `FileRouter::route` outcome (C5 routing contract, consumer area 6).
//!
//! Each test names the contract it stands for, so a deliberate change of the
//! contract is not mistaken for a regression.

use ecp_cli::commands::admin::index::{run_analyzer_for_paths, should_analyze_path};
use ecp_cli::provider_registry::make_provider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::contracts::routing::{
    Dispatch, FileRouter, Language, PathRule, RouteOutcome, SizeCap,
};
use ecp_core::graph::{NodeKind, ZeroCopyGraph};
use std::path::{Path, PathBuf};

const ONE_MIB: usize = 1024 * 1024;

fn router() -> FileRouter {
    FileRouter::new(SizeCap::BUILT_IN)
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A Python file of `bytes` bytes that defines `name` first and pads with comments.
fn python_of_size(name: &str, bytes: usize) -> Vec<u8> {
    let mut source = format!("def {name}():\n    return 1\n").into_bytes();
    source.resize(bytes, b'#');
    if let Some(last) = source.last_mut() {
        *last = b'\n';
    }
    source
}

fn cold_index(src: &Path) -> ZeroCopyGraph {
    let out = tempfile::tempdir().unwrap();
    run_analyzer_for_paths(src, out.path(), None, None)
        .expect("cold index")
        .1
}

fn node_names(graph: &ZeroCopyGraph, kind: NodeKind) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == kind)
        .map(|node| node.name.resolve(pool).to_string())
        .collect()
}

/// `(kind, file_path)` of every blind spot in the graph.
fn blind_spots(graph: &ZeroCopyGraph) -> Vec<(String, String)> {
    let pool = graph.string_pool.as_slice();
    graph
        .blind_spots
        .iter()
        .map(|spot| {
            (
                spot.kind.resolve(pool).to_string(),
                spot.file_path.resolve(pool).to_string(),
            )
        })
        .collect()
}

fn has_blind_spot(graph: &ZeroCopyGraph, kind: &str, file: &str) -> bool {
    blind_spots(graph)
        .iter()
        .any(|(k, f)| k == kind && f == file)
}

/// Contract: the walk's "is this source" answer is the router's `Routed`
/// answer, for every extension default of every language (the 14 mainstream
/// languages and the DSL and config languages), and the provider that the
/// wrapper names is the route's first provider.
///
/// Fails on today's code: `should_analyze_path` is private to the binary
/// module, and today it accepts `md`/`txt`/`rst`, which the router leaves
/// unrouted.
#[test]
fn test_should_analyze_path_agrees_with_route_every_default_extension() {
    let mut covered_mainstream = 0;
    for &language in Language::ALL {
        for ext in language.extensions() {
            let rel = format!("src/sample.{ext}");
            let outcome = router().route(&rel);
            let RouteOutcome::Routed(route) = outcome else {
                panic!("{rel}: the router must route a default extension, got {outcome:?}");
            };
            assert_eq!(route.dispatch(), Dispatch::Fixed(language), "{rel}");
            assert!(
                should_analyze_path(Path::new(&rel)),
                "{rel}: the walk must accept what the router routes"
            );
            assert_eq!(
                AnalyzerPipeline::provider_name_for_path(Path::new(&rel)),
                Some(language.provider()),
                "{rel}: the provider name comes from the route"
            );
        }
        if Language::MAINSTREAM.contains(&language) && !language.extensions().is_empty() {
            covered_mainstream += 1;
        }
    }
    assert_eq!(
        covered_mainstream,
        Language::MAINSTREAM.len(),
        "every mainstream language must be exercised by an extension"
    );
}

/// Contract: a path the router does not route is not walked, whatever the
/// old extension list said. `md`, `txt` and `rst` have no provider today
/// (`provider_name_for_path` returns `None`), upper-case extensions are
/// unrouted, and the template rules exclude by repo-relative path.
///
/// Fails on today's code: `should_analyze_path` is private, and it accepts
/// `README.md`.
#[test]
fn test_should_analyze_path_unrouted_and_excluded_paths_rejected() {
    for rel in [
        "README.md",
        "notes.txt",
        "docs/guide.rst",
        "src/UPPER.RS",
        "src/.py",
        "src/no_extension",
        "bricks/feature/__brick__/lib/widget.dart",
        "bricks/{{name.snakeCase()}}/widget.dart",
        "",
    ] {
        assert!(
            !should_analyze_path(Path::new(rel)),
            "{rel:?} must not be walked"
        );
        assert!(
            AnalyzerPipeline::provider_name_for_path(Path::new(rel)).is_none(),
            "{rel:?} must have no provider"
        );
    }
}

/// Contract: path rules decide before extensions, and the wrapper keeps
/// today's answer for the YAML and JSON content-sniffed routes (the first
/// candidate: `kubernetes`, `openapi`).
///
/// Pins today's behaviour of `provider_name_for_path` for these paths.
#[test]
fn test_provider_name_for_path_path_rules_and_sniffed_routes_keep_first_candidate() {
    for (rel, provider) in [
        ("Dockerfile", "dockerfile"),
        ("svc/dockerfile", "dockerfile"),
        ("docker-compose.yml", "docker-compose"),
        ("deploy/compose.yaml", "docker-compose"),
        (".github/workflows/ci.yml", "github-actions"),
        ("k8s/deployment.yaml", "kubernetes"),
        ("api/openapi.yaml", "kubernetes"),
        ("api/spec.json", "openapi"),
    ] {
        assert_eq!(
            AnalyzerPipeline::provider_name_for_path(Path::new(rel)),
            Some(provider),
            "{rel}"
        );
        assert!(should_analyze_path(Path::new(rel)), "{rel}");
    }
    assert_eq!(
        router().route("api/openapi.yaml"),
        router().route("api/other.yml"),
        "a path alone cannot tell an OpenAPI yaml from another yaml"
    );
}

/// Contract (C5 Open point 1): the cold walk drops a file over the one
/// `SizeCap` (1 MiB by default) and the graph records it as a `file-too-large`
/// blind spot, never silently. A 2 MiB file is between today's two caps.
///
/// Pins today's cold walk (it already enforces 1 MiB): the omission stays a
/// blind spot and the small neighbour stays indexed.
#[test]
fn test_cold_index_file_between_1mib_and_16mib_omitted_with_blind_spot() {
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "ok.py", &python_of_size("ok_fn", 64));
    write(src.path(), "big.py", &python_of_size("big_fn", 2 * ONE_MIB));

    let graph = cold_index(src.path());

    assert!(
        has_blind_spot(&graph, "file-too-large", "big.py"),
        "the omitted file must surface as a blind spot, got {:?}",
        blind_spots(&graph)
    );
    let functions = node_names(&graph, NodeKind::Function);
    assert!(functions.contains(&"ok_fn".to_string()), "{functions:?}");
    assert!(!functions.contains(&"big_fn".to_string()), "{functions:?}");
}

/// Contract: one `SizeCap` reaches the parse step. `AnalyzerPipeline::analyze`
/// (the overlay, reanalysis and baseline path) omits a 2 MiB file with a
/// `file-too-large` blind spot and parses nothing of it.
///
/// Fails on today's code: the pipeline has its own 16 MiB default, so it parses
/// the 2 MiB file.
#[test]
fn test_analyze_file_between_1mib_and_16mib_omitted_with_blind_spot() {
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "big.py", &python_of_size("big_fn", 2 * ONE_MIB));

    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(make_provider("python").unwrap().unwrap());
    let graphs = pipeline.analyze(vec![(src.path().join("big.py"), PathBuf::from("big.py"))]);

    assert_eq!(graphs.len(), 1);
    let graph = &graphs[0];
    assert!(graph.nodes.is_empty(), "nothing of the file is parsed");
    assert_eq!(
        graph
            .blind_spots
            .iter()
            .map(|spot| spot.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["file-too-large"]
    );
}

/// Contract: a repo-config cap raises the cap for the walk and for the parse
/// step alike. Guards the plumbing: a pipeline that reads only the built-in
/// default would omit the file that the walk admitted.
///
/// Pins today's behaviour (the pipeline's 16 MiB default admits the file).
#[test]
fn test_cold_index_repo_config_raises_cap_file_indexed() {
    let src = tempfile::tempdir().unwrap();
    write(
        src.path(),
        ".ecp/config.toml",
        b"[index]\nmax_file_bytes = 4194304\n",
    );
    write(src.path(), "big.py", &python_of_size("big_fn", 2 * ONE_MIB));

    let graph = cold_index(src.path());

    let functions = node_names(&graph, NodeKind::Function);
    assert!(functions.contains(&"big_fn".to_string()), "{functions:?}");
    assert!(
        !has_blind_spot(&graph, "file-too-large", "big.py"),
        "{:?}",
        blind_spots(&graph)
    );
}

/// Contract (C5 Open point 6, the chain ends at OpenAPI): an OpenAPI YAML
/// reaches the OpenAPI provider, not only the Kubernetes provider whose
/// content gate rejects it. The consumer reads the schema as a `Struct` node.
/// A generic YAML still gets no graph from this slice.
///
/// Fails on today's code: every `.yaml` goes to `kubernetes` alone, so the
/// OpenAPI schema `Pet` is missing from the index.
#[test]
fn test_route_openapi_yaml_not_kubernetes_provider() {
    let rule = match router().route("api/openapi.yaml") {
        RouteOutcome::Routed(route) => route.rule(),
        other => panic!("a yaml path must route, got {other:?}"),
    };
    assert_eq!(rule, PathRule::YamlContent);

    let src = tempfile::tempdir().unwrap();
    write(
        src.path(),
        "api/openapi.yaml",
        b"openapi: 3.0.0\ninfo:\n  title: Pets\n  version: 1.0.0\npaths: {}\ncomponents:\n  schemas:\n    Pet:\n      type: object\n      properties:\n        name:\n          type: string\n",
    );
    write(
        src.path(),
        "config/app.yaml",
        b"name: plain\nvalues:\n  - 1\n  - 2\n",
    );

    let graph = cold_index(src.path());

    assert_eq!(
        node_names(&graph, NodeKind::Struct),
        vec!["Pet".to_string()],
        "the OpenAPI schema is indexed and the generic yaml adds no Struct"
    );
}
