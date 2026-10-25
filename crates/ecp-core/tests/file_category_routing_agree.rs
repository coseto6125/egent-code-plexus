//! `file_category::Language` and `FileMeta` take their language from the C5
//! route (`contracts::routing`). These tests replace the two ledger tests that
//! `routing.rs` held until the readers moved (C5, "Value mapping").
//!
//! The pinned values below stand for the resolver's language barrier: a
//! change in one of them changes which global candidates a caller admits.

use ecp_core::contracts::routing::{FileRouter, Language as RouteLanguage, RouteOutcome};
use ecp_core::file_category::{FileMeta, Language};

fn route_legacy(path: &str) -> Language {
    match FileRouter::PATH_ONLY.route(path) {
        RouteOutcome::Routed(route) => route.to_legacy(),
        RouteOutcome::Excluded(_) | RouteOutcome::Unrouted => Language::Unknown,
    }
}

#[test]
fn test_language_from_path_equals_route_to_legacy_every_extension() {
    for &language in RouteLanguage::ALL {
        for ext in language.extensions() {
            for path in [format!("src/x.{ext}"), format!("x.{ext}")] {
                assert_eq!(Language::from_path(&path), route_legacy(&path), "{path}");
                assert_eq!(
                    Language::from_path(&path),
                    language.to_legacy(),
                    "{path}: an extension default gives its language"
                );
            }
        }
    }
}

#[test]
fn test_language_from_path_equals_route_to_legacy_every_path_rule() {
    // (path, the value the resolver barrier reads)
    let cases: &[(&str, Language)] = &[
        ("Dockerfile", Language::Dockerfile),
        ("svc/dockerfile", Language::Dockerfile),
        ("svc/x.dockerfile", Language::Dockerfile),
        ("docker-compose.yml", Language::DockerCompose),
        ("svc/compose.yaml", Language::DockerCompose),
        (".github/workflows/ci.yml", Language::GitHubActions),
        (
            "sub/.github/workflows/nested/ci.yaml",
            Language::GitHubActions,
        ),
        (".github/ci.yml", Language::Yaml),
        ("k8s/deploy.yaml", Language::Yaml),
        ("api/spec.json", Language::Unknown),
        ("App.vue", Language::Unknown),
        ("App.svelte", Language::Unknown),
        ("Page.astro", Language::Unknown),
        ("api.proto", Language::Unknown),
        ("src/A.RS", Language::Unknown),
        ("src/B.PY", Language::Unknown),
        ("x.mts", Language::Unknown),
        ("noext", Language::Unknown),
        ("", Language::Unknown),
    ];
    for (path, expected) in cases {
        assert_eq!(Language::from_path(path), *expected, "{path}");
        assert_eq!(route_legacy(path), *expected, "route {path}");
    }
}

#[test]
fn test_language_from_path_backslash_routes_like_slash() {
    for (windows, unix) in [
        (".github\\workflows\\ci.yml", ".github/workflows/ci.yml"),
        ("src\\lib.rs", "src/lib.rs"),
        ("svc\\Dockerfile", "svc/Dockerfile"),
    ] {
        assert_eq!(
            Language::from_path(windows),
            Language::from_path(unix),
            "{windows}"
        );
        assert_eq!(
            Language::from_normalized_path(unix),
            Language::from_path(unix),
            "{unix}"
        );
    }
    assert_eq!(
        Language::from_path(".github\\workflows\\ci.yml"),
        Language::GitHubActions
    );
}

#[test]
fn test_language_from_path_template_paths_keep_extension_language() {
    // The route excludes template paths from the index. The language reader
    // skips those two rules: it reads graph files, which never hold a
    // template path, and the rules would scan every component per call.
    assert_eq!(
        FileRouter::PATH_ONLY.route("__brick__/lib/a.dart"),
        RouteOutcome::Excluded(ecp_core::contracts::routing::PathRule::TemplateDir)
    );
    assert_eq!(Language::from_path("__brick__/lib/a.dart"), Language::Dart);
    assert_eq!(Language::from_path("{{name}}/widget.dart"), Language::Dart);
}

#[test]
fn test_route_ignoring_templates_equals_route_for_non_template_paths() {
    for path in [
        "src/lib.rs",
        "ui/App.tsx",
        "Dockerfile",
        "svc/compose.yml",
        ".github/workflows/ci.yml",
        "k8s/deploy.yaml",
        "api/spec.json",
        "a.RS",
        ".bashrc",
        "src/",
        "./src/./a.py",
    ] {
        assert_eq!(
            FileRouter::PATH_ONLY.route_ignoring_templates(path),
            FileRouter::PATH_ONLY.route(path).route().copied(),
            "{path}"
        );
    }
}

#[test]
fn test_language_from_path_docs_stay_markdown() {
    // Documentation has no route (C5 open point 2); the label stays.
    for path in ["README.md", "docs/notes.txt", "a\\guide.rst"] {
        assert_eq!(FileRouter::PATH_ONLY.route(path), RouteOutcome::Unrouted);
        assert_eq!(Language::from_path(path), Language::Markdown, "{path}");
        assert_eq!(Language::from_path(path).as_str(), "Markdown");
    }
}

#[test]
fn test_language_from_path_dotfile_unknown_like_index() {
    // `.py` is a dot-file with no extension: the index never routes it.
    assert_eq!(Language::from_path(".py"), Language::Unknown);
    assert_eq!(Language::from_path("src/.bashrc"), Language::Unknown);
}

#[test]
fn test_file_meta_from_path_vendor_flag_unchanged() {
    let cases: &[(&str, bool, Language)] = &[
        ("vendor/x/lib.rs", true, Language::Rust),
        ("crates/vendor/x/src/lib.rs", true, Language::Rust),
        ("crates\\vendor\\x\\lib.rs", true, Language::Rust),
        ("src/vendored/lib.rs", false, Language::Rust),
        ("src/myvendor/a.py", false, Language::Python),
        ("src/lib.rs", false, Language::Rust),
        ("vendor/App.vue", true, Language::Unknown),
    ];
    for (path, is_vendor, language) in cases {
        let meta = FileMeta::from_path(path);
        assert_eq!(meta.is_vendor, *is_vendor, "vendor {path}");
        assert_eq!(meta.language, *language, "language {path}");
    }
    let normalized = FileMeta::from_normalized_path("crates/vendor/x/src/lib.rs");
    assert_eq!(
        normalized,
        FileMeta::from_path("crates\\vendor\\x\\src\\lib.rs")
    );
}
