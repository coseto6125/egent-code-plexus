use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_analyzer::{
    c::parser::CProvider, c_sharp::parser::CSharpProvider, cpp::parser::CppProvider,
    dart::parser::DartProvider, go::parser::GoProvider, java::parser::JavaProvider,
    javascript::parser::JavaScriptProvider, kotlin::parser::KotlinProvider,
    php::parser::PhpProvider, ruby::parser::RubyProvider, swift::parser::SwiftProvider,
};
use ecp_cli::config_parser::parse_configs;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use std::path::Path;

fn input(local: LocalGraph) -> OverlayFileInput {
    local.into()
}

type Target = (String, String, String, u32);

// Closure edges retain multiplicity here: deduplication would hide a stale
// base edge surviving beside the freshly rebuilt lexical reference.
fn closure_parity(
    provider: &dyn LanguageProvider,
    path: &str,
    before: &str,
    after: &str,
    count: usize,
) {
    let parse = |source: &str| {
        provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap()
    };
    let build = |source: &str| {
        let mut builder = GraphBuilder::new();
        builder.add_graph(parse(source));
        builder.build()
    };
    let base = build(before);
    let full = build(after);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(archived, &[input(parse(after))]).unwrap();
    // Every view a test builds must survive the session view cache unchanged.
    assert_eq!(
        OverlayView::from_cache_bytes(&view.to_cache_bytes(), archived.nodes.len()).as_ref(),
        Some(&view),
        "view cache round trip"
    );
    let merged = MergedGraph::new(archived, Some(&view));
    let reason = "closure:lexical_reference";
    let mut expected: Vec<_> = full
        .edges
        .iter()
        .filter(|e| e.reason.resolve(&full.string_pool) == reason)
        .map(|e| {
            let source = &full.nodes[e.source as usize];
            (
                source.uid,
                full.nodes[e.target as usize].uid,
                source.span.0 + 1,
                (e.confidence * 100.0).round() as u32,
                e.reason.resolve(&full.string_pool).to_owned(),
            )
        })
        .collect();
    assert_eq!(
        expected.len(),
        count,
        "full rebuild must contain the intended closures: {path}: {after}"
    );
    let mut actual: Vec<_> = merged
        .all_edges()
        .filter(|e| {
            e.rel_type() == RelType::References
                && merged
                    .node(e.target)
                    .unwrap()
                    .name(&merged)
                    .starts_with("<anonymous:")
        })
        .map(|e| {
            let source = merged.node(e.source).unwrap();
            assert!(
                e.is_overlay(),
                "dirty lexical references must come from the fresh parse: {path}"
            );
            (
                source.uid(),
                merged.node(e.target).unwrap().uid(),
                source.start_line(),
                (e.confidence() * 100.0).round() as u32,
                e.reason(archived).to_owned(),
            )
        })
        .collect();
    expected.sort();
    actual.sort();
    assert_eq!(
        actual, expected,
        "closure references differ: {path}: {after}"
    );
    // Exercise incoming traversal too, as upstream impact does.
    let target = (merged.base_len()..merged.node_count())
        .find(|&idx| merged.node(idx).unwrap().name(&merged) == "target")
        .unwrap();
    let mut reached = std::collections::HashSet::from([target]);
    let mut frontier = vec![target];
    for _ in 0..3 {
        frontier = frontier
            .into_iter()
            .flat_map(|node| merged.in_edges(node))
            .filter(|e| matches!(e.rel_type(), RelType::Calls | RelType::References))
            .map(|e| e.source)
            .filter(|node| reached.insert(*node))
            .collect();
    }
    assert!(
        reached
            .into_iter()
            .any(|node| merged.node(node).unwrap().name(&merged) == "enclosing"),
        "upstream impact must reach enclosing: {path}: {after}"
    );
}

macro_rules! closure_cases {
    ($module:ident, $provider:path, $path:literal, $source:literal, $empty:literal, $nested:literal) => {
        mod $module {
            use super::*;
            #[test]
            fn test_overlay_closure_shift_preserves_enclosing() {
                closure_parity(
                    &<$provider>::new().unwrap(),
                    $path,
                    $source,
                    &format!("\n{}", $source),
                    1,
                );
            }
            #[test]
            fn test_overlay_closure_added_preserves_enclosing() {
                closure_parity(&<$provider>::new().unwrap(), $path, $empty, $source, 1);
            }
            #[test]
            fn test_overlay_closure_body_edit_has_one_reference() {
                closure_parity(
                    &<$provider>::new().unwrap(),
                    $path,
                    $source,
                    &$source.replace("target(1)", "target(2)"),
                    1,
                );
            }
            #[test]
            fn test_overlay_closure_nested_matches_rebuild() {
                closure_parity(&<$provider>::new().unwrap(), $path, $source, $nested, 2);
            }
        }
    };
}

closure_cases!(ts_closures, ecp_analyzer::typescript::TypeScriptProvider, "app.ts",
    "function target(value: number) {}\nfunction enclosing() {\n register(() => target(1));\n}\n",
    "function target(value: number) {}\nfunction enclosing() {}\n",
    "function target(value: number) {}\nfunction enclosing() {\n register(() => register(() => target(1)));\n}\n");
closure_cases!(js_closures, ecp_analyzer::javascript::parser::JavaScriptProvider, "app.js",
    "function target(value) {}\nfunction enclosing() {\n register(() => target(1));\n}\n",
    "function target(value) {}\nfunction enclosing() {}\n",
    "function target(value) {}\nfunction enclosing() {\n register(() => register(() => target(1)));\n}\n");
closure_cases!(
    py_closures,
    ecp_analyzer::python::PythonProvider,
    "app.py",
    "def target(value): pass\ndef enclosing():\n register(lambda: target(1))\n",
    "def target(value): pass\ndef enclosing(): pass\n",
    "def target(value): pass\ndef enclosing():\n register(lambda: register(lambda: target(1)))\n"
);
closure_cases!(rs_closures, ecp_analyzer::rust::parser::RustProvider, "app.rs",
    "fn target(value: i32) {}\nfn enclosing() {\n register(|| { target(1); });\n}\n",
    "fn target(value: i32) {}\nfn enclosing() {}\n",
    "fn target(value: i32) {}\nfn enclosing() {\n register(|| { register(|| { target(1); }); });\n}\n");
closure_cases!(go_closures, ecp_analyzer::go::parser::GoProvider, "app.go",
    "package demo\nfunc target(value int) {}\nfunc enclosing() {\n register(func() { target(1) })\n}\n",
    "package demo\nfunc target(value int) {}\nfunc enclosing() {}\n",
    "package demo\nfunc target(value int) {}\nfunc enclosing() {\n register(func() { register(func() { target(1) }) })\n}\n");
closure_cases!(java_closures, ecp_analyzer::java::parser::JavaProvider, "App.java",
    "class App {\n void target(int value) {}\n void enclosing() {\n register(() -> target(1));\n }\n}\n",
    "class App {\n void target(int value) {}\n void enclosing() {}\n}\n",
    "class App {\n void target(int value) {}\n void enclosing() {\n register(() -> register(() -> target(1)));\n }\n}\n");

#[test]
fn test_overlay_closure_overloads_use_live_parent() {
    let source = "class App {\n void target(int value) {}\n void enclosing() { register(() -> target(1)); }\n void enclosing(int value) { register(() -> target(1)); }\n}\n";
    closure_parity(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        "App.java",
        source,
        &format!("\n{source}"),
        2,
    );
}

fn targets(graph: &ZeroCopyGraph, caller: &str) -> Vec<Target> {
    let pool = &graph.string_pool;
    let mut result: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::Calls
                && graph.nodes[e.source as usize].name.resolve(pool) == caller
        })
        .map(|e| {
            let n = &graph.nodes[e.target as usize];
            (
                graph.files[n.file_idx as usize].path.resolve(pool).into(),
                n.name.resolve(pool).into(),
                n.owner_class.resolve(pool).into(),
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    result.sort();
    result.dedup();
    result
}

fn parity(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    expected: &[Target],
) {
    check_parity(provider, files, dirty, expected, expected);
}

fn check_parity(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    expected: &[Target],
    overlay_expected: &[Target],
) {
    check_parity_from("run", provider, files, dirty, expected, overlay_expected);
}

fn parity_from(
    caller: &str,
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    expected: &[Target],
) {
    check_parity_from(caller, provider, files, dirty, expected, expected);
}

#[test]
fn test_overlay_rust_bare_call_in_inline_module_matches_reindex() {
    let source = r#"
mod imp {
    struct Config;
    impl Config {
        fn load() {}
        fn run() { load(); }
    }
    fn load() {}
}
struct Top;
impl Top { fn load() {} }
fn load() {}
"#;
    parity(
        &RustProvider::new().unwrap(),
        &[("src/lib.rs", source)],
        &[("src/lib.rs", &format!("\n{source}"))],
        &[("src/lib.rs".into(), "load".into(), "imp".into(), 100)],
    );
}

#[test]
fn test_overlay_nested_free_function_precedes_top_level_namesake_matches_reindex() {
    let rust = RustProvider::new().unwrap();
    let python = PythonProvider::new().unwrap();
    let cases: &[(&dyn LanguageProvider, &str, &str)] = &[
        (
            &rust,
            "src/lib.rs",
            "fn outer() { fn helper() {} fn inner() { helper(); } } fn helper() {}",
        ),
        (
            &python,
            "main.py",
            "def outer():\n    def helper():\n        pass\n    def inner():\n        helper()\ndef helper():\n    pass\n",
        ),
    ];
    for &(provider, path, source) in cases {
        parity_from(
            "inner",
            provider,
            &[(path, source)],
            &[(path, &format!("\n{source}"))],
            &[(path.into(), "helper".into(), "outer".into(), 100)],
        );
    }
}

#[test]
fn test_overlay_go_declared_package_name_matches_exact_import_directory() {
    let provider = GoProvider::new().unwrap();
    for (path, package, import, receiver) in [
        ("v2/helper.go", "app", "\"example.com/repo/v2\"", "app"),
        (
            "internal/go-utils/helper.go",
            "utils",
            "\"example.com/repo/internal/go-utils\"",
            "utils",
        ),
        (
            "v2/helper.go",
            "app",
            "client \"example.com/repo/v2\"",
            "client",
        ),
    ] {
        let dependency = format!("package {package}\nfunc F() {{}}\n");
        let source = format!(
            "package main\nimport {import}\nfunc F() {{}}\nfunc run() {{ {receiver}.F() }}\n"
        );
        let files = [("main.go", source.as_str()), (path, dependency.as_str())];
        for dirty_dependency in [false, true] {
            let edited = format!("\n{source}");
            let mut dirty = vec![("main.go", edited.as_str())];
            if dirty_dependency {
                dirty.push((path, dependency.as_str()));
            }
            parity(
                &provider,
                &files,
                &dirty,
                &[(path.into(), "F".into(), "".into(), 95)],
            );
        }
    }
}

#[test]
fn test_overlay_go_unaliased_import_preserves_value_receiver_matches_reindex() {
    let provider = GoProvider::new().unwrap();
    let source = "package main\nimport \"fmt\"\ntype Store struct {}\nfunc (s *Store) Fetch() {}\nvar global = &Store{}\nfunc run() { global.Fetch() }\n";
    let baseline = source.replace("import \"fmt\"\n", "");
    let expected = [("main.go".into(), "Fetch".into(), "Store".into(), 100)];
    parity(
        &provider,
        &[("main.go", &baseline)],
        &[("main.go", source)],
        &expected,
    );
}

#[test]
fn test_overlay_go_package_namesake_type_cross_file_method_matches_reindex() {
    let provider = GoProvider::new().unwrap();
    let source = "package cache\ntype cache struct {}\nfunc New() *cache { return &cache{} }\nfunc run() { New().Purge() }\n";
    let method = "package cache\nfunc (c *cache) Purge() {}\n";
    parity(
        &provider,
        &[("cache/cache.go", source), ("cache/purge.go", method)],
        &[
            ("cache/cache.go", &format!("\n{source}")),
            ("cache/purge.go", method),
        ],
        &[
            ("cache/cache.go".into(), "New".into(), "".into(), 100),
            ("cache/purge.go".into(), "Purge".into(), "cache".into(), 85),
        ],
    );
}

#[test]
fn test_overlay_go_module_root_import_matches_reindex() {
    let provider = GoProvider::new().unwrap();
    let source = "package main\nimport \"example.com/repo\"\nfunc run() { app.F() }\n";
    parity(
        &provider,
        &[
            ("main/main.go", source),
            ("app.go", "package app\nfunc F() {}\n"),
        ],
        &[("main/main.go", &format!("\n{source}"))],
        &[("app.go".into(), "F".into(), "".into(), 95)],
    );
}

#[test]
fn test_overlay_go_block_and_closure_alias_shadowing_matches_reindex() {
    let provider = GoProvider::new().unwrap();
    let dependency = "package pkg\nfunc F() {}\n";
    let before = "package main\nimport p \"example.com/repo/pkg\"\nfunc run() {}\n";
    let after_block = "package main\nimport p \"example.com/repo/pkg\"\nfunc run() { if true { p := 1; _ = p }; p.F() }\n";
    parity(
        &provider,
        &[("main.go", before), ("pkg/helper.go", dependency)],
        &[("main.go", after_block)],
        &[("pkg/helper.go".into(), "F".into(), "".into(), 95)],
    );
    let closure = "package main\nimport p \"example.com/repo/pkg\"\nfunc run() { func(p interface{ F() }) { p.F() }(nil) }\n";
    parity(
        &provider,
        &[("main.go", before), ("pkg/helper.go", dependency)],
        &[("main.go", closure)],
        &[],
    );
}

#[test]
fn test_overlay_go_parenthesized_selector_preserves_member_target() {
    for call in ["b.F()", "(b.F)()"] {
        let source = format!("package main\ntype Box struct{{}}\nfunc (b Box) F() {{}}\nfunc F() {{}}\nfunc run() {{ var b Box; {call} }}\n");
        parity(
            &GoProvider::new().unwrap(),
            &[("main.go", &source)],
            &[("main.go", &format!("\n{source}"))],
            &[("main.go".into(), "F".into(), "Box".into(), 85)],
        );
    }
}

#[test]
fn test_overlay_rust_nested_helpers_under_same_named_methods_keep_lexical_identity() {
    let source = "struct A; struct B; impl A { fn new() { fn helper() {} helper(); } } impl B { fn new() { fn helper() { marker(); } helper(); } } fn marker() {}";
    parity_from(
        "new",
        &RustProvider::new().unwrap(),
        &[("src/lib.rs", source)],
        &[("src/lib.rs", &format!("\n{source}"))],
        &[
            ("src/lib.rs".into(), "helper".into(), "A::new".into(), 100),
            ("src/lib.rs".into(), "helper".into(), "B::new".into(), 100),
        ],
    );
}

#[test]
fn test_overlay_rust_outer_call_prefers_its_nested_function() {
    let source = "fn helper() {} fn run() { fn helper() { marker(); } helper(); } fn marker() {}";
    parity(
        &RustProvider::new().unwrap(),
        &[("src/lib.rs", source)],
        &[("src/lib.rs", &format!("\n{source}"))],
        &[("src/lib.rs".into(), "helper".into(), "run".into(), 100)],
    );
}

#[test]
fn test_overlay_rust_nested_helper_in_another_scope_is_not_global() {
    let provider = RustProvider::new().unwrap();
    let source = "struct A; struct B; impl A { fn new() { fn helper() {} } } impl B { fn new() { helper(); } }";
    parity_from(
        "new",
        &provider,
        &[("src/lib.rs", source)],
        &[("src/lib.rs", &format!("\n{source}"))],
        &[],
    );
    parity(
        &provider,
        &[
            ("src/lib.rs", "fn run() {}"),
            ("src/other.rs", "fn outer() { fn helper() {} }"),
        ],
        &[("src/lib.rs", "fn run() { helper(); }")],
        &[],
    );
}

/// A full reindex of `files` with `edits` applied, written under `root` so
/// a Rust fixture gets its module tree.
fn reindex(
    root: &Path,
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    edits: &[(&str, &str)],
) -> ZeroCopyGraph {
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"overlay-parity\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    if provider.name() == "go" {
        std::fs::write(root.join("go.mod"), "module example.com/repo\n\ngo 1.21\n").unwrap();
    }
    let mut builder = GraphBuilder::new()
        .with_path_aliases(parse_configs(root))
        .with_repo_root(root.to_path_buf());
    let mut sources = files.to_vec();
    sources.extend(
        edits
            .iter()
            .filter(|(p, _)| !files.iter().any(|(old, _)| old == p)),
    );
    for &(path, original) in &sources {
        let source = edits
            .iter()
            .find(|(p, _)| *p == path)
            .map_or(original, |(_, s)| *s);
        let disk = root.join(path);
        std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
        std::fs::write(disk, source).unwrap();
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

fn check_parity_from(
    caller: &str,
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    expected: &[Target],
    overlay_expected: &[Target],
) {
    let tmp = tempfile::tempdir().unwrap();
    let base = reindex(tmp.path(), provider, files, &[]);
    let full = reindex(tmp.path(), provider, files, dirty);
    assert_eq!(
        targets(&full, caller),
        expected,
        "full reindex fixture must exercise the intended target: {dirty:?}"
    );
    let files: Vec<_> = dirty
        .iter()
        .map(|(path, source)| {
            input(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .unwrap(),
            )
        })
        .collect();
    assert!(
        files
            .iter()
            .flat_map(|f| &f.symbols)
            .any(|s| s.name == caller && !s.calls.is_empty()),
        "the dirty caller must contain a parsed call, including unresolved fixtures"
    );
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(archived, &files).unwrap();
    // Every view a test builds must survive the session view cache unchanged.
    assert_eq!(
        OverlayView::from_cache_bytes(&view.to_cache_bytes(), archived.nodes.len()).as_ref(),
        Some(&view),
        "view cache round trip"
    );
    let merged = MergedGraph::new(archived, Some(&view));
    let mut actual: Vec<Target> = merged
        .all_edges()
        .filter(|e| {
            e.rel_type() == RelType::Calls && merged.node(e.source).unwrap().name(&merged) == caller
        })
        .map(|e| {
            let n = merged.node(e.target).unwrap();
            if dirty
                .iter()
                .any(|(path, _)| Some(*path) == n.file_path(&merged))
            {
                assert!(
                    e.target >= merged.base_len(),
                    "dirty targets must redirect to virtual nodes"
                );
            }
            (
                n.file_path(&merged).unwrap().into(),
                n.name(&merged).into(),
                n.owner_class(&merged).unwrap_or_default().into(),
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    actual.sort();
    actual.dedup();
    assert_eq!(
        actual, overlay_expected,
        "merged overlay Calls must equal the expected reindex targets: {dirty:?}"
    );
}

#[test]
fn test_overlay_rust_untyped_member_with_free_function_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod other; fn foo() {}\nfn run() {}"),
            ("src/other.rs", "struct Foo; impl Foo { fn foo(&self) {} }"),
        ],
        &[(
            "src/lib.rs",
            "mod other; fn foo() {}\nfn run() { let x = unknown(); x.foo(); }",
        )],
        &[],
    );
}

#[test]
fn test_overlay_rust_module_path_selects_named_module_like_reindex() {
    for (call, dirty_target) in [
        ("crate::a::b::foo", false),
        ("crate::a::b::foo", true),
        ("self::a::b::foo", false),
    ] {
        let source = format!("mod a; mod other;\nfn run() {{ {call}(); }}");
        let mut dirty = vec![("src/lib.rs", source.as_str())];
        if dirty_target {
            dirty.push(("src/a/b.rs", "pub fn foo() { let changed = 1; }"));
        }
        parity(
            &RustProvider::new().unwrap(),
            &[
                ("src/lib.rs", "mod a; mod other; fn run() {}"),
                ("src/a/mod.rs", "pub mod b;"),
                ("src/a/b.rs", "pub fn foo() {}"),
                ("src/other.rs", "pub fn foo() {}"),
            ],
            &dirty,
            &[("src/a/b.rs".into(), "foo".into(), "".into(), 100)],
        );
    }
}

/// The index tries the qualifier tier before the module tree: `util` names
/// the `mod util;` declaration in `lib.rs`, so the qualifier tier binds the
/// `foo` that `lib.rs` declares (0.85) before the module tree reaches
/// `util.rs` (1.0). The overlay must keep that order, not the better answer.
#[test]
fn test_overlay_rust_qualifier_tier_before_module_tree_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod util;\npub fn foo() {}\nfn run() {}"),
            ("src/util.rs", "pub fn foo() {}"),
        ],
        &[(
            "src/lib.rs",
            "mod util;\npub fn foo() {}\nfn run() { crate::util::foo(); }",
        )],
        &[("src/lib.rs".into(), "foo".into(), "".into(), 85)],
    );
}

/// `use crate::k::K` names the qualifier's file, with the module tree on.
#[test]
fn test_overlay_rust_use_imported_qualifier_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod k;\nuse crate::k::K;\nfn run() {}"),
            (
                "src/k.rs",
                "pub struct K;\nimpl K {\n pub fn st(id: i32) {}\n}\n",
            ),
        ],
        &[(
            "src/lib.rs",
            "mod k;\nuse crate::k::K;\nfn run() { K::st(2); }",
        )],
        &[("src/k.rs".into(), "st".into(), "K".into(), 85)],
    );
}

#[test]
fn test_overlay_python_untyped_member_collision_matches_reindex() {
    parity(
        &PythonProvider::new().unwrap(),
        &[
            ("app.py", "def foo(): pass\ndef run(x): pass\n"),
            ("other.py", "class Foo:\n    def foo(self): pass\n"),
        ],
        &[("app.py", "def foo(): pass\ndef run(x): x.foo()\n")],
        // An untyped receiver never reaches the caller's own module's free
        // function, and two owners elsewhere leave the global tier empty.
        &[],
    );
}

#[test]
fn test_overlay_python_bare_call_unrelated_method_no_edge_matches_reindex() {
    parity(
        &PythonProvider::new().unwrap(),
        &[
            ("app.py", "def run(): pass\n"),
            ("other.py", "class K:\n    def config(self): pass\n"),
        ],
        &[("app.py", "def run(): config()\n")],
        // A bare Python call never reaches a method of another class.
        &[],
    );
}

#[test]
fn test_overlay_python_bare_call_free_function_beats_method_matches_reindex() {
    parity(
        &PythonProvider::new().unwrap(),
        &[
            ("app.py", "def run(): pass\n"),
            ("helpers.py", "def config(): pass\n"),
            ("other.py", "class K:\n    def config(self): pass\n"),
        ],
        &[("app.py", "def run(): config()\n")],
        &[("helpers.py".into(), "config".into(), "".into(), 70)],
    );
}

#[test]
fn test_overlay_php_bare_call_unrelated_method_no_edge_matches_reindex() {
    parity(
        &php(),
        &[
            ("app.php", "<?php\nfunction run() {}\n"),
            (
                "tests/MiddlewareConfigTest.php",
                "<?php\nclass MiddlewareConfigTest { private function config() {} }\n",
            ),
        ],
        &[("app.php", "<?php\nfunction run() { config(); }\n")],
        &[],
    );
}

#[test]
fn test_overlay_php_namespace_constant_in_other_namespace_returns_no_edge_matches_reindex() {
    let (full, overlay) = field_read_edges(
        &php(),
        &[
            (
                "lib.php",
                "<?php\nnamespace Company\\Lib { const LIMIT = 1; }\n",
            ),
            (
                "app.php",
                "<?php\nnamespace Other\\Lib;\nfunction readApp(): int { return LIMIT; }\n",
            ),
        ],
        &[(
            "app.php",
            "\n<?php\nnamespace Other\\Lib;\nfunction readApp(): int { return LIMIT; }\n",
        )],
        &["value_read"],
    );
    assert!(full.is_empty(), "cross-namespace constant read: {full:?}");
    assert_eq!(overlay, full);
}

#[test]
fn test_overlay_php_bare_call_same_file_method_no_edge_matches_reindex() {
    parity(
        &php(),
        &[(
            "src/Settings.php",
            "<?php\nclass Settings {\n    protected function config($k) {}\n    public function run() {}\n}\n",
        )],
        &[(
            "src/Settings.php",
            "<?php\nclass Settings {\n    protected function config($k) {}\n    public function run() { return config('app.name'); }\n}\n",
        )],
        // A bare PHP call never reaches the caller's own method.
        &[],
    );
}

#[test]
fn test_overlay_php_bare_call_parent_method_no_edge_matches_reindex() {
    parity(
        &php(),
        &[
            (
                "src/Base.php",
                "<?php\nclass Base {\n    protected function config($k) {}\n}\n",
            ),
            (
                "src/Child.php",
                "<?php\nclass Child extends Base {\n    public function run() {}\n}\n",
            ),
        ],
        &[(
            "src/Child.php",
            "<?php\nclass Child extends Base {\n    public function run() { return config('app.name'); }\n}\n",
        )],
        // Nor its base's: the heritage tier does not run.
        &[],
    );
}

/// `svc.mailer.send` retries as the short `send`, which is no bare call:
/// the reindex keeps the original site's member reach and binds the unique
/// method at the Global tier.
#[test]
fn test_overlay_python_module_alias_retry_reaches_method_matches_reindex() {
    parity(
        &py(),
        &[
            (
                "app/mailer.py",
                "class Mailer:\n    def send(self, msg):\n        pass\n",
            ),
            (
                "app/services.py",
                "from app.mailer import Mailer\n\nmailer = Mailer()\n",
            ),
            (
                "main.py",
                "import app.services as svc\n\n\ndef run(msg):\n    pass\n",
            ),
        ],
        &[(
            "main.py",
            "import app.services as svc\n\n\ndef run(msg):\n    svc.mailer.send(msg)\n",
        )],
        &[("app/mailer.py".into(), "send".into(), "Mailer".into(), 70)],
    );
}

#[test]
fn test_overlay_rust_untyped_member_without_free_function_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod other; fn run() {}"),
            ("src/other.rs", "struct Foo; impl Foo { fn foo(&self) {} }"),
        ],
        &[(
            "src/lib.rs",
            "mod other; fn run() { let x = unknown(); x.foo(); }",
        )],
        &[("src/other.rs".into(), "foo".into(), "Foo".into(), 70)],
    );
}

#[test]
fn test_overlay_rust_external_module_never_uses_local_namesake() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod b; fn run() {}"),
            ("src/b.rs", "pub fn foo() {}"),
        ],
        &[("src/lib.rs", "mod b; fn run() { external::b::foo(); }")],
        &[],
    );
}

#[test]
fn test_overlay_rust_unanchored_nested_module_matches_unresolved_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod a; mod other;\nfn run() {}"),
            ("src/a/mod.rs", "pub mod b;"),
            ("src/a/b.rs", "pub fn foo() {}"),
            ("src/other.rs", "pub fn foo() {}"),
        ],
        &[("src/lib.rs", "mod a; mod other;\nfn run() { a::b::foo(); }")],
        &[],
    );
}

#[test]
fn test_overlay_rust_bin_crate_root_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod util;"),
            ("src/util.rs", "pub fn foo() {}"),
            ("src/bin/tool.rs", "mod util;\nfn run() {}"),
            ("src/bin/util.rs", "pub fn foo() {}"),
        ],
        &[(
            "src/bin/tool.rs",
            "mod util;\nfn run() { crate::util::foo(); }",
        )],
        &[("src/bin/util.rs".into(), "foo".into(), "".into(), 100)],
    );
}

#[test]
fn test_overlay_typescript_untyped_member_collision_matches_reindex() {
    parity(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("app.ts", "function foo() {}\nfunction run(x: any) {}"),
            ("other.ts", "class Foo { foo() {} }"),
        ],
        &[(
            "app.ts",
            "function foo() {}\nfunction run(x: any) { x.foo(); }",
        )],
        &[],
    );
}

macro_rules! language_parity {
    ($test:ident, $provider:path, $path:literal, $before:literal, $after:literal, $owner:literal) => {
        #[test]
        fn $test() {
            parity(
                &<$provider>::new().unwrap(),
                &[($path, $before)],
                &[($path, $after)],
                &[($path.into(), "foo".into(), $owner.into(), 100)],
            );
        }
    };
}

language_parity!(
    test_overlay_typescript_plain_call_matches_reindex,
    TypeScriptProvider,
    "app.ts",
    "function foo() {}\nfunction run() {}",
    "function foo() {}\nfunction run() { foo(); }",
    ""
);
language_parity!(
    test_overlay_javascript_plain_call_matches_reindex,
    ecp_analyzer::javascript::parser::JavaScriptProvider,
    "app.js",
    "function foo() {}\nfunction run() {}",
    "function foo() {}\nfunction run() { foo(); }",
    ""
);
language_parity!(
    test_overlay_python_plain_call_matches_reindex,
    PythonProvider,
    "app.py",
    "def foo(): pass\ndef run(): pass\n",
    "def foo(): pass\ndef run(): foo()\n",
    ""
);
language_parity!(
    test_overlay_java_plain_call_matches_reindex,
    ecp_analyzer::java::JavaProvider,
    "App.java",
    "class App { void foo() {}\nvoid run() {} }",
    "class App { void foo() {}\nvoid run() { foo(); } }",
    "App"
);
language_parity!(
    test_overlay_kotlin_plain_call_matches_reindex,
    ecp_analyzer::kotlin::parser::KotlinProvider,
    "app.kt",
    "fun foo() {}\nfun run() {}",
    "fun foo() {}\nfun run() { foo() }",
    ""
);
language_parity!(
    test_overlay_csharp_plain_call_matches_reindex,
    ecp_analyzer::c_sharp::parser::CSharpProvider,
    "App.cs",
    "class App { void foo() {}\nvoid run() {} }",
    "class App { void foo() {}\nvoid run() { foo(); } }",
    "App"
);
language_parity!(
    test_overlay_go_plain_call_matches_reindex,
    ecp_analyzer::go::parser::GoProvider,
    "app.go",
    "package app\nfunc foo() {}\nfunc run() {}",
    "package app\nfunc foo() {}\nfunc run() { foo() }",
    ""
);
language_parity!(
    test_overlay_rust_plain_call_matches_reindex,
    RustProvider,
    "src/lib.rs",
    "fn foo() {}\nfn run() {}",
    "fn foo() {}\nfn run() { foo(); }",
    ""
);
language_parity!(
    test_overlay_php_plain_call_matches_reindex,
    ecp_analyzer::php::parser::PhpProvider,
    "app.php",
    "<?php function foo() {}\nfunction run() {}",
    "<?php function foo() {}\nfunction run() { foo(); }",
    ""
);
language_parity!(
    test_overlay_ruby_plain_call_matches_reindex,
    ecp_analyzer::ruby::parser::RubyProvider,
    "app.rb",
    "def foo; end\ndef run; end",
    "def foo; end\ndef run; foo(); end",
    ""
);
language_parity!(
    test_overlay_swift_plain_call_matches_reindex,
    ecp_analyzer::swift::SwiftProvider,
    "app.swift",
    "func foo() {}\nfunc run() {}",
    "func foo() {}\nfunc run() { foo() }",
    ""
);
language_parity!(
    test_overlay_c_plain_call_matches_reindex,
    ecp_analyzer::c::CProvider,
    "app.c",
    "void foo() {}\nvoid run() {}",
    "void foo() {}\nvoid run() { foo(); }",
    ""
);
language_parity!(
    test_overlay_cpp_plain_call_matches_reindex,
    ecp_analyzer::cpp::CppProvider,
    "app.cpp",
    "void foo() {}\nvoid run() {}",
    "void foo() {}\nvoid run() { foo(); }",
    ""
);
language_parity!(
    test_overlay_dart_plain_call_matches_reindex,
    ecp_analyzer::dart::DartProvider,
    "app.dart",
    "void foo() {}\nvoid run() {}",
    "void foo() {}\nvoid run() { foo(); }",
    ""
);

#[test]
fn test_overlay_rust_mod_rs_crate_uses_crate_root() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod a;\nmod x;"),
            ("src/a/mod.rs", "pub mod x;\nfn run() {}"),
            ("src/a/x.rs", "pub fn foo() {}"),
            ("src/x.rs", "pub fn foo() {}"),
        ],
        &[("src/a/mod.rs", "pub mod x;\nfn run() { crate::x::foo(); }")],
        &[("src/x.rs".into(), "foo".into(), "".into(), 100)],
    );
}

#[test]
fn test_overlay_rust_bin_descendant_uses_target_root() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod x;"),
            ("src/x.rs", "pub fn foo() {}"),
            ("src/bin/tool/main.rs", "mod support;\nmod x;\nfn main() {}"),
            ("src/bin/tool/support.rs", "fn run() {}"),
            ("src/bin/tool/x.rs", "pub fn foo() {}"),
        ],
        &[("src/bin/tool/support.rs", "fn run() { crate::x::foo(); }")],
        &[("src/bin/tool/x.rs".into(), "foo".into(), "".into(), 100)],
    );
}

#[test]
fn test_overlay_rust_removed_module_does_not_bind_stale_file() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod a;\nfn run() { crate::a::b::foo(); }"),
            ("src/a/mod.rs", "pub mod b;"),
            ("src/a/b.rs", "pub fn foo() {}"),
        ],
        &[
            ("src/lib.rs", "mod a;\nfn run() { crate::a::b::foo(); }"),
            ("src/a/mod.rs", ""),
        ],
        &[],
    );
}

#[test]
fn test_overlay_rust_cfg_duplicates_share_target_identity() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod a;\nfn run() {}"),
            ("src/a/mod.rs", "pub mod b;"),
            ("src/a/b.rs", "pub fn foo() {}"),
        ],
        &[
            ("src/lib.rs", "mod a;\nfn run() { crate::a::b::foo(); }"),
            (
                "src/a/b.rs",
                "#[cfg(unix)]\npub fn foo() {}\n#[cfg(windows)]\npub fn foo() {}",
            ),
        ],
        &[("src/a/b.rs".into(), "foo".into(), "".into(), 100)],
    );
}

#[test]
fn test_overlay_rust_root_member_heads_match_reindex() {
    for call in ["crate::foo", "self::foo"] {
        let dirty = format!("fn foo() {{}}\nfn run() {{ {call}(); }}");
        parity(
            &RustProvider::new().unwrap(),
            &[("src/lib.rs", "fn foo() {}\nfn run() {}")],
            &[("src/lib.rs", &dirty)],
            &[("src/lib.rs".into(), "foo".into(), "".into(), 100)],
        );
    }
}

#[test]
fn test_overlay_rust_parent_root_member_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod util; pub fn foo() {}"),
            ("src/util.rs", "fn run() {}"),
        ],
        &[("src/util.rs", "fn run() { super::foo(); }")],
        &[("src/lib.rs".into(), "foo".into(), "".into(), 100)],
    );
}

#[test]
fn test_overlay_rust_module_path_excludes_method_despite_index_bug() {
    check_parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod x;\nfn run() {}"),
            ("src/x.rs", "pub struct P; impl P { pub fn parse() {} }"),
        ],
        &[("src/lib.rs", "mod x;\nfn run() { crate::x::parse(); }")],
        &[("src/x.rs".into(), "parse".into(), "P".into(), 85)],
        &[],
    );
}

#[test]
fn test_overlay_rust_declared_plain_module_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod util;\nfn run() {}"),
            ("src/util.rs", "pub fn foo() {}"),
        ],
        &[("src/lib.rs", "mod util;\nfn run() { util::foo(); }")],
        &[("src/util.rs".into(), "foo".into(), "".into(), 85)],
    );
}

#[test]
fn test_overlay_rust_new_dirty_module_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[("src/lib.rs", "fn run() {}")],
        &[
            ("src/lib.rs", "mod util;\nfn run() { util::foo(); }"),
            ("src/util.rs", "pub fn foo() {}"),
        ],
        &[("src/util.rs".into(), "foo".into(), "".into(), 85)],
    );
}

#[test]
fn test_overlay_rust_nonroot_heads_match_reindex() {
    for (caller, declaration, calls) in [
        (
            "src/a/b.rs",
            "pub mod x;",
            [
                ("crate::x::foo", "src/x.rs", 100),
                ("self::x::foo", "src/a/b/x.rs", 100),
                ("super::x::foo", "src/a/x.rs", 100),
                ("super::super::x::foo", "src/x.rs", 100),
            ],
        ),
        (
            "src/a/b/mod.rs",
            "pub mod x;",
            [
                ("crate::x::foo", "src/x.rs", 100),
                ("self::x::foo", "src/a/b/x.rs", 100),
                ("super::x::foo", "src/a/x.rs", 100),
                ("super::super::x::foo", "src/x.rs", 100),
            ],
        ),
    ] {
        for (call, target, confidence) in calls {
            let before = format!("{declaration}\nfn run() {{}}");
            let after = format!("{declaration}\nfn run() {{ {call}(); }}");
            parity(
                &RustProvider::new().unwrap(),
                &[
                    ("src/lib.rs", "mod a;\nmod x;"),
                    ("src/a/mod.rs", "pub mod b;\npub mod x;"),
                    (caller, &before),
                    ("src/x.rs", "pub fn foo() {}"),
                    ("src/a/x.rs", "pub fn foo() {}"),
                    ("src/a/b/x.rs", "pub fn foo() {}"),
                ],
                &[(caller, &after)],
                &[(target.into(), "foo".into(), "".into(), confidence)],
            );
        }
    }
}

#[test]
fn test_overlay_rust_unique_qualifier_confidence_matches_reindex() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod util;\nfn run() {}"),
            ("src/util.rs", "pub fn foo() {}"),
        ],
        &[("src/lib.rs", "mod util;\nfn run() { crate::util::foo(); }")],
        &[("src/util.rs".into(), "foo".into(), "".into(), 85)],
    );
}

#[test]
fn test_overlay_rust_mod_rs_relative_heads_match_reindex() {
    for (call, target) in [
        ("crate::x::foo", Some("src/x.rs")),
        ("self::x::foo", Some("src/a/x.rs")),
        ("super::x::foo", Some("src/x.rs")),
        ("super::super::x::foo", None),
    ] {
        let dirty = format!("pub mod x;\nfn run() {{ {call}(); }}");
        let expected: Vec<Target> = target
            .into_iter()
            .map(|file| (file.into(), "foo".into(), "".into(), 100))
            .collect();
        parity(
            &RustProvider::new().unwrap(),
            &[
                ("src/lib.rs", "mod a;\nmod x;"),
                ("src/a/mod.rs", "pub mod x;\nfn run() {}"),
                ("src/x.rs", "pub fn foo() {}"),
                ("src/a/x.rs", "pub fn foo() {}"),
            ],
            &[("src/a/mod.rs", &dirty)],
            &expected,
        );
    }
}

#[test]
fn test_overlay_rust_clean_caller_redirects_to_dirty_target() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod util;\nfn run() { foo(); }"),
            ("src/util.rs", "pub fn foo() {}"),
        ],
        &[(
            "src/util.rs",
            "pub fn foo() {}\nfn bar() {}\nfn run() { bar(); }",
        )],
        &[
            ("src/util.rs".into(), "bar".into(), "".into(), 100),
            ("src/util.rs".into(), "foo".into(), "".into(), 70),
        ],
    );
}

#[test]
fn test_overlay_rust_nested_main_is_an_ordinary_module() {
    parity(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod util;"),
            ("src/util/mod.rs", "pub mod main;\npub fn foo() {}"),
            ("src/util/main.rs", "fn run() {}"),
        ],
        &[("src/util/main.rs", "fn run() { crate::util::foo(); }")],
        &[("src/util/mod.rs".into(), "foo".into(), "".into(), 100)],
    );
}

// Import-member policy (Python, Java, Kotlin, PHP): the import that binds a
// callee decides its target, or that it has none. Each fixture names the
// overlay's pre-policy divergence from the reindex.

fn py() -> PythonProvider {
    PythonProvider::new().unwrap()
}

fn java() -> ecp_analyzer::java::JavaProvider {
    ecp_analyzer::java::JavaProvider::new().unwrap()
}

fn kotlin() -> ecp_analyzer::kotlin::parser::KotlinProvider {
    ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap()
}

fn php() -> ecp_analyzer::php::parser::PhpProvider {
    ecp_analyzer::php::parser::PhpProvider::new().unwrap()
}

const JAVA_HELPER: (&str, &str) = (
    "src/main/java/pkg/Helper.java",
    "package pkg; public class Helper { public static void helper() {} }",
);

const KOTLIN_UTIL: (&str, &str) = (
    "src/main/kotlin/pkg/Util.kt",
    "package pkg\nfun helper() {}\n",
);

// Was: the member name bound the repo's only `helper` globally (false caller).
#[test]
fn test_overlay_python_external_module_alias_matches_reindex() {
    parity(
        &py(),
        &[
            ("elsewhere/util.py", "def helper():\n    pass\n"),
            ("app.py", "import external as u\n\ndef run():\n    pass\n"),
        ],
        &[(
            "app.py",
            "import external as u\n\ndef run():\n    return u.helper()\n",
        )],
        &[],
    );
}

// Was: two `helper`s made the member name ambiguous (dropped caller).
#[test]
fn test_overlay_python_module_alias_duplicate_name_matches_reindex() {
    parity(
        &py(),
        &[
            ("pkg/util.py", "def helper():\n    pass\n"),
            ("pkg/other.py", "def helper():\n    pass\n"),
            ("app.py", "import pkg.util as u\n\ndef run():\n    pass\n"),
        ],
        &[(
            "app.py",
            "import pkg.util as u\n\ndef run():\n    return u.helper()\n",
        )],
        &[("pkg/util.py".into(), "helper".into(), "".into(), 95)],
    );
}

// Was: the stripped member name hit the wrapper itself at Tier 1.
#[test]
fn test_overlay_python_same_name_wrapper_matches_reindex() {
    parity_from(
        "helper",
        &py(),
        &[
            ("pkg/util.py", "def helper():\n    pass\n"),
            (
                "app.py",
                "import pkg.util as u\n\ndef helper():\n    pass\n",
            ),
        ],
        &[(
            "app.py",
            "import pkg.util as u\n\ndef helper():\n    return u.helper()\n",
        )],
        &[("pkg/util.py".into(), "helper".into(), "".into(), 95)],
    );
}

// Was: the named import fell through to the repo's only `helper` globally.
#[test]
fn test_overlay_python_dotted_object_attribute_does_not_bind_same_file_function() {
    let source = "from app import settings\n\ndef commit():\n    pass\n\ndef caller():\n    return settings.db.commit()\n";
    let before = "from app import settings\n\ndef commit():\n    pass\n\ndef caller():\n    pass\n";
    let files = [
        ("app/__init__.py", "settings = object()\n"),
        ("main.py", before),
    ];
    for dirty in [
        vec![("main.py", source)],
        vec![
            ("main.py", source),
            ("app/__init__.py", "settings = object()\n"),
        ],
    ] {
        check_parity_from("caller", &py(), &files, &dirty, &[], &[]);
    }
}

#[test]
fn test_overlay_python_from_external_import_matches_reindex() {
    parity(
        &py(),
        &[
            ("elsewhere/util.py", "def helper():\n    pass\n"),
            (
                "app.py",
                "from external import helper\n\ndef run():\n    pass\n",
            ),
        ],
        &[(
            "app.py",
            "from external import helper\n\ndef run():\n    return helper()\n",
        )],
        &[],
    );
}

// `pkg` is an indexed directory, so the missing `pkg.extra` may be local: the
// index keeps the global tier (0.7). Was: the path-segment tier matched
// `lib/extra/` at 0.95. A wrong suppression would return no edge.
#[test]
fn test_overlay_python_indexed_first_segment_keeps_global_matches_reindex() {
    parity(
        &py(),
        &[
            ("pkg/util.py", "def other():\n    pass\n"),
            ("lib/extra/x.py", "def helper():\n    pass\n"),
            (
                "app.py",
                "from pkg.extra import helper\n\ndef run():\n    pass\n",
            ),
        ],
        &[(
            "app.py",
            "from pkg.extra import helper\n\ndef run():\n    return helper()\n",
        )],
        &[("lib/extra/x.py".into(), "helper".into(), "".into(), 70)],
    );
}

// Was: the qualified callee `Helper.helper` matched no name (dropped caller).
#[test]
fn test_overlay_java_class_import_matches_reindex() {
    parity(
        &java(),
        &[
            JAVA_HELPER,
            ("App.java", "import pkg.Helper; class App { void run() {} }"),
        ],
        &[(
            "App.java",
            "import pkg.Helper; class App { void run() { Helper.helper(); } }",
        )],
        &[(JAVA_HELPER.0.into(), "helper".into(), "Helper".into(), 95)],
    );
}

// Was: two `helper` methods made the static import's name ambiguous.
#[test]
fn test_overlay_java_static_import_matches_reindex() {
    parity(
        &java(),
        &[
            JAVA_HELPER,
            (
                "src/main/java/other/Tools.java",
                "package other; public class Tools { public static void helper() {} }",
            ),
            (
                "App.java",
                "import static pkg.Helper.helper; class App { void run() {} }",
            ),
        ],
        &[(
            "App.java",
            "import static pkg.Helper.helper; class App { void run() { helper(); } }",
        )],
        &[(JAVA_HELPER.0.into(), "helper".into(), "Helper".into(), 95)],
    );
}

// Was: the external static import fell through to the repo's only `helper`.
#[test]
fn test_overlay_java_external_static_import_matches_reindex() {
    parity(
        &java(),
        &[
            JAVA_HELPER,
            (
                "App.java",
                "import static com.ext.Other.helper; class App { void run() {} }",
            ),
        ],
        &[(
            "App.java",
            "import static com.ext.Other.helper; class App { void run() { helper(); } }",
        )],
        &[],
    );
}

// `pkg` is an indexed directory, so the missing `pkg.Missing` may be local:
// the index keeps the global tier (0.7). Was: the path-segment tier matched
// the `helper/` directory at 0.95. A wrong suppression would return no edge.
#[test]
fn test_overlay_java_indexed_first_segment_keeps_global_matches_reindex() {
    parity(
        &java(),
        &[
            (
                "src/main/java/pkg/Other.java",
                "package pkg; public class Other {}",
            ),
            (
                "src/main/java/lib/helper/Tools.java",
                "package lib.helper; public class Tools { public static void helper() {} }",
            ),
            (
                "App.java",
                "import static pkg.Missing.helper; class App { void run() {} }",
            ),
        ],
        &[(
            "App.java",
            "import static pkg.Missing.helper; class App { void run() { helper(); } }",
        )],
        &[(
            "src/main/java/lib/helper/Tools.java".into(),
            "helper".into(),
            "Tools".into(),
            70,
        )],
    );
}

// Was: two `helper`s made the member import's name ambiguous.
#[test]
fn test_overlay_kotlin_member_import_matches_reindex() {
    parity(
        &kotlin(),
        &[
            KOTLIN_UTIL,
            ("Other.kt", "fun helper() {}\n"),
            ("App.kt", "import pkg.helper\nfun run() {}\n"),
        ],
        &[("App.kt", "import pkg.helper\nfun run() { helper() }\n")],
        &[(KOTLIN_UTIL.0.into(), "helper".into(), "".into(), 95)],
    );
}

// An external class receiver can still reach an in-repo extension function.
#[test]
fn test_overlay_kotlin_extension_on_external_class_matches_reindex() {
    let factory = (
        "conv/Factory.kt",
        "package conv\nimport kotlinx.serialization.BinaryFormat\nfun BinaryFormat.asConverterFactory(t: String): Any = t\n",
    );
    parity(
        &kotlin(),
        &[
            factory,
            (
                "conv/App.kt",
                "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun run() {}\n",
            ),
        ],
        &[(
            "conv/App.kt",
            "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun run() { ProtoBuf.asConverterFactory(\"x\") }\n",
        )],
        &[(factory.0.into(), "asConverterFactory".into(), "".into(), 70)],
    );
}

// The import's first segment names an in-repo directory, so it is not
// external; the class-bound callee still reaches the extension function.
#[test]
fn test_overlay_kotlin_extension_under_indexed_root_matches_reindex() {
    let factory = (
        "kotlinx/conv/Factory.kt",
        "package kotlinx.conv\nimport kotlinx.serialization.BinaryFormat\nfun BinaryFormat.asConverterFactory(t: String): Any = t\n",
    );
    parity(
        &kotlin(),
        &[
            factory,
            (
                "conv/App.kt",
                "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun run() {}\n",
            ),
        ],
        &[(
            "conv/App.kt",
            "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun run() { ProtoBuf.asConverterFactory(\"x\") }\n",
        )],
        &[(factory.0.into(), "asConverterFactory".into(), "".into(), 70)],
    );
}

#[test]
fn test_overlay_kotlin_computed_receiver_extension_matches_reindex() {
    let before = "class Other {}\nfun Other.ext() {}\nclass C { fun other(): Other = Other()\nfun run() {} }\n";
    let after = "class Other {}\nfun Other.ext() {}\nclass C { fun other(): Other = Other()\nfun run() { other().ext() } }\n";
    parity(
        &KotlinProvider::new().unwrap(),
        &[("C.kt", before)],
        &[("C.kt", after)],
        &[
            ("C.kt".into(), "ext".into(), "".into(), 100),
            ("C.kt".into(), "other".into(), "C".into(), 100),
        ],
    );
}

// A typed library receiver that retries onto the caller itself: the index
// drops self-recursion Calls, so the overlay must too.
#[test]
fn test_overlay_kotlin_typed_receiver_retry_onto_caller_matches_reindex() {
    let before = "package com.example\nimport com.google.gson.Gson\nfun toJson(o: Any, gson: Gson): Any = o\n";
    let after = "package com.example\nimport com.google.gson.Gson\nfun toJson(o: Any, gson: Gson): Any = gson.toJson(o)\n";
    parity_from(
        "toJson",
        &kotlin(),
        &[("com/example/Json.kt", before)],
        &[("com/example/Json.kt", after)],
        &[],
    );
}

// `super.onResume()` on an external base must not turn into a self-call.
#[test]
fn test_overlay_kotlin_super_of_external_base_matches_reindex() {
    let before = "package com.example\nimport android.app.Activity\nclass Screen : Activity() {\n override fun onResume() {}\n}\n";
    let after = "package com.example\nimport android.app.Activity\nclass Screen : Activity() {\n override fun onResume() { super.onResume() }\n}\n";
    parity_from(
        "onResume",
        &kotlin(),
        &[("com/example/Screen.kt", before)],
        &[("com/example/Screen.kt", after)],
        &[],
    );
}

// Was: the alias `h` named no symbol (dropped caller).
#[test]
fn test_overlay_kotlin_alias_import_matches_reindex() {
    parity(
        &kotlin(),
        &[
            KOTLIN_UTIL,
            ("App.kt", "import pkg.helper as h\nfun run() {}\n"),
        ],
        &[("App.kt", "import pkg.helper as h\nfun run() { h() }\n")],
        &[(KOTLIN_UTIL.0.into(), "helper".into(), "".into(), 95)],
    );
}

// Was: two `Helper` classes made the construction ambiguous.
#[test]
fn test_overlay_php_class_use_construction_matches_reindex() {
    parity(
        &php(),
        &[
            ("src/pkg/Helper.php", "<?php namespace pkg; class Helper {}"),
            (
                "src/other/Helper.php",
                "<?php namespace other; class Helper {}",
            ),
            ("app.php", "<?php use pkg\\Helper; function run() {}"),
        ],
        &[(
            "app.php",
            "<?php use pkg\\Helper; function run() { new Helper(); }",
        )],
        &[("src/pkg/Helper.php".into(), "Helper".into(), "".into(), 95)],
    );
}

// A global-namespace import is not a namespaced namesake's file.
#[test]
fn test_overlay_php_builtin_use_skips_namespaced_namesake_matches_reindex() {
    parity(
        &php(),
        &[
            (
                "src/Testing/InvalidArgumentException.php",
                "<?php namespace Lib\\Testing; class InvalidArgumentException {}",
            ),
            ("app.php", "<?php use InvalidArgumentException; function run() {}"),
        ],
        &[(
            "app.php",
            "<?php use InvalidArgumentException; function run() { new InvalidArgumentException(); }",
        )],
        &[(
            "src/Testing/InvalidArgumentException.php".into(),
            "InvalidArgumentException".into(),
            "".into(),
            70,
        )],
    );
}

// Was: two `helper` functions made the function import's name ambiguous.
#[test]
fn test_overlay_php_use_function_matches_reindex() {
    parity(
        &php(),
        &[
            (
                "src/pkg/Helper.php",
                "<?php namespace pkg; function helper() {}",
            ),
            (
                "src/other/util.php",
                "<?php namespace other; function helper() {}",
            ),
            (
                "app.php",
                "<?php use function pkg\\helper; function run() {}",
            ),
        ],
        &[(
            "app.php",
            "<?php use function pkg\\helper; function run() { helper(); }",
        )],
        &[("src/pkg/Helper.php".into(), "helper".into(), "".into(), 95)],
    );
}

// Was: the vendor class import fell through to the repo's only `Command`.
#[test]
fn test_overlay_php_external_vendor_use_matches_reindex() {
    parity(
        &php(),
        &[
            (
                "src/pkg/Command.php",
                "<?php namespace pkg; class Command {}",
            ),
            (
                "app.php",
                "<?php use Symfony\\Component\\Console\\Command; function run() {}",
            ),
        ],
        &[(
            "app.php",
            "<?php use Symfony\\Component\\Console\\Command; function run() { new Command(); }",
        )],
        &[],
    );
}

// `super.setup()` names the imported base, whose file lacks `setup`: the
// receiver ladder binds the inherited Root.setup on both sides. Was: both
// retried the bare name and bound the caller to itself.
#[test]
fn test_overlay_kotlin_super_member_outside_imported_file_matches_reindex() {
    let screen = |body: &str| {
        format!(
            "package app\nimport base.BaseScreen\nclass Screen : BaseScreen() {{\n override fun setup() {{ {body} }}\n}}\n"
        )
    };
    let (clean, dirty) = (screen(""), screen("super.setup()"));
    parity_from(
        "setup",
        &kotlin(),
        &[
            (
                "src/main/kotlin/base/Root.kt",
                "package base\nopen class Root {\n open fun setup() {}\n}\n",
            ),
            (
                "src/main/kotlin/base/BaseScreen.kt",
                "package base\nopen class BaseScreen : Root()\n",
            ),
            ("src/main/kotlin/app/Screen.kt", clean.as_str()),
        ],
        &[("src/main/kotlin/app/Screen.kt", dirty.as_str())],
        &[(
            "src/main/kotlin/base/Root.kt".into(),
            "setup".into(),
            "Root".into(),
            80,
        )],
    );
}

// `import com.app.repository.*` binds no receiver, and a package wildcard
// imports no class member: the call resolves by its member name (0.7). Was:
// the receiver became `repository.fetchUsers`, which nothing resolves.
#[test]
fn test_overlay_kotlin_wildcard_package_named_receiver_matches_reindex() {
    let vm = |body: &str| {
        format!(
            "package com.app.ui\nimport com.app.repository.*\nclass VM(private val repository: UserRepository) {{\n fun run() {{ {body} }}\n}}\n"
        )
    };
    let (clean, dirty) = (vm(""), vm("repository.fetchUsers()"));
    parity(
        &kotlin(),
        &[
            (
                "src/main/kotlin/com/app/repository/UserRepository.kt",
                "package com.app.repository\nclass UserRepository {\n fun fetchUsers() {}\n}\n",
            ),
            ("src/main/kotlin/com/app/ui/VM.kt", clean.as_str()),
        ],
        &[("src/main/kotlin/com/app/ui/VM.kt", dirty.as_str())],
        &[(
            "src/main/kotlin/com/app/repository/UserRepository.kt".into(),
            "fetchUsers".into(),
            "UserRepository".into(),
            70,
        )],
    );
}

// `use function helper;` names no namespace, so it may be the repo's global
// function in `helpers.php`: the index keeps the global tier. Was: both
// suppressed the genuine caller.
#[test]
fn test_overlay_php_single_segment_use_function_matches_reindex() {
    parity(
        &php(),
        &[
            ("helpers.php", "<?php function helper() {}"),
            (
                "app.php",
                "<?php namespace App; use function helper; function run() {}",
            ),
        ],
        &[(
            "app.php",
            "<?php namespace App; use function helper; function run() { helper(); }",
        )],
        &[("helpers.php".into(), "helper".into(), "".into(), 70)],
    );
}

// A member the caller's class inherits beats a star import, and a package
// wildcard imports no class member: both resolve `refresh` on Base (0.8).
// Was: both bound the wildcard's `Adapter.refresh` at 0.95.
#[test]
fn test_overlay_kotlin_wildcard_below_heritage_matches_reindex() {
    let screen = |body: &str| {
        format!(
            "package app\nimport lib.Base\nimport util.*\nclass Screen : Base() {{\n fun run() {{ {body} }}\n}}\n"
        )
    };
    let (clean, dirty) = (screen(""), screen("refresh()"));
    parity(
        &kotlin(),
        &[
            (
                "src/main/kotlin/lib/Base.kt",
                "package lib\nopen class Base {\n fun refresh() {}\n}\n",
            ),
            (
                "src/main/kotlin/util/Adapter.kt",
                "package util\nclass Adapter {\n fun refresh() {}\n}\n",
            ),
            ("src/main/kotlin/app/Screen.kt", clean.as_str()),
        ],
        &[("src/main/kotlin/app/Screen.kt", dirty.as_str())],
        &[(
            "src/main/kotlin/lib/Base.kt".into(),
            "refresh".into(),
            "Base".into(),
            80,
        )],
    );
}

/// Sorted (source uid, target uid, rel, confidence %) of the Calls edges
/// from `caller` and of every overload-set References edge: from a full
/// reindex of `files` with `dirty` applied, then from the overlay of `dirty`
/// over the index of `files`. Uids tell overloads apart, which share file,
/// name and owner.
fn overload_edges(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    caller: &str,
) -> (Vec<OverloadEdge>, Vec<OverloadEdge>) {
    let parse = |path: &str, source: &str| {
        provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap()
    };
    let build = |edits: &[(&str, &str)]| {
        let mut builder = GraphBuilder::new();
        for &(path, original) in files {
            let source = edits
                .iter()
                .find(|(p, _)| *p == path)
                .map_or(original, |(_, s)| *s);
            builder.add_graph(parse(path, source));
        }
        builder.build()
    };
    let wanted = |rel: RelType, source_name: &str, reason: &str| {
        (rel == RelType::Calls && source_name == caller)
            || (rel == RelType::References && reason == "overload_set")
    };
    let base = build(&[]);
    let full = build(dirty);
    let pool = &full.string_pool;
    let mut expected: Vec<_> = full
        .edges
        .iter()
        .filter(|e| {
            wanted(
                e.rel_type,
                full.nodes[e.source as usize].name.resolve(pool),
                e.reason.resolve(pool),
            )
        })
        .map(|e| {
            (
                full.nodes[e.source as usize].uid,
                full.nodes[e.target as usize].uid,
                e.rel_type == RelType::Calls,
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let inputs: Vec<_> = dirty
        .iter()
        .map(|&(path, source)| input(parse(path, source)))
        .collect();
    let view = OverlayView::build(archived, &inputs).unwrap();
    // Every view a test builds must survive the session view cache unchanged.
    assert_eq!(
        OverlayView::from_cache_bytes(&view.to_cache_bytes(), archived.nodes.len()).as_ref(),
        Some(&view),
        "view cache round trip"
    );
    let merged = MergedGraph::new(archived, Some(&view));
    let mut actual: Vec<_> = merged
        .all_edges()
        .filter(|e| {
            wanted(
                e.rel_type(),
                merged.node(e.source).unwrap().name(&merged),
                e.reason(archived),
            )
        })
        .map(|e| {
            (
                merged.node(e.source).unwrap().uid(),
                merged.node(e.target).unwrap().uid(),
                e.rel_type() == RelType::Calls,
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    expected.sort();
    actual.sort();
    (expected, actual)
}

/// (source uid, target uid, is Calls, confidence %).
type OverloadEdge = (u64, u64, bool, u32);

#[test]
fn test_overlay_overload_set_in_dirty_file_matches_reindex() {
    let k = "class K {\n void audit() {}\n void save(int id) {}\n void save(String name) { audit(); }\n void run() { save(1); }\n}\n";
    let shifted = format!("\n{k}");
    let (full, overlay) = overload_edges(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        &[("K.java", k)],
        &[("K.java", &shifted)],
        "run",
    );
    assert_eq!(full.len(), 2, "one call, one overload link");
    assert_eq!(overlay, full);
}

/// The dirty caller binds the first overloaded constructor in a clean file,
/// whose overload link stays the base edge.
#[test]
fn test_overlay_overload_set_in_clean_file_matches_reindex() {
    let app = "public class App {\n    public Multi makeMulti() {\n        return new Multi(1);\n    }\n}\n";
    let shifted = format!("\n{app}");
    let (full, overlay) = overload_edges(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        &[
            ("src/App.java", app),
            (
                "src/Multi.java",
                "public class Multi {\n    public Multi(int x) {\n    }\n\n    public Multi(String s) {\n    }\n}\n",
            ),
        ],
        &[("src/App.java", &shifted)],
        "makeMulti",
    );
    assert_eq!(full.len(), 2, "one call, one overload link");
    assert_eq!(overlay, full);
}

/// `save(int)` delegates to `save(String)`: the name binds the caller
/// itself, so the overload link alone connects the two.
#[test]
fn test_overlay_overload_delegating_to_sibling_matches_reindex() {
    let k = "class K {\n void save(int id) { save(\"x\"); }\n void save(String name) {}\n}\n";
    let shifted = format!("\n{k}");
    let (full, overlay) = overload_edges(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        &[("K.java", k)],
        &[("K.java", &shifted)],
        "save",
    );
    assert_eq!(full.len(), 1, "only the overload link");
    assert_eq!(overlay, full);
}

/// (source uid, target uid, rel, reason, confidence %).
type NameRefEdge = (u64, u64, RelType, String, u32);

/// The selected `field_reads` edges from a reindex of `dirty` and an overlay
/// of `dirty` over the base index. This covers ordinary property reads plus
/// tagged NameRefs for values and callbacks.
fn field_read_edges(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    reasons: &[&str],
) -> (Vec<NameRefEdge>, Vec<NameRefEdge>) {
    let parse = |path: &str, source: &str| {
        provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap()
    };
    let build = |edits: &[(&str, &str)]| {
        let mut builder = GraphBuilder::new();
        for &(path, original) in files {
            let source = edits
                .iter()
                .find(|(p, _)| *p == path)
                .map_or(original, |(_, s)| *s);
            builder.add_graph(parse(path, source));
        }
        builder.build()
    };
    let wanted = |reason: &str| reasons.contains(&reason);
    let base = build(&[]);
    let full = build(dirty);
    let pool = &full.string_pool;
    let mut expected: Vec<NameRefEdge> = full
        .edges
        .iter()
        .filter(|e| wanted(e.reason.resolve(pool)))
        .map(|e| {
            (
                full.nodes[e.source as usize].uid,
                full.nodes[e.target as usize].uid,
                e.rel_type,
                e.reason.resolve(pool).to_owned(),
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let inputs: Vec<_> = dirty
        .iter()
        .map(|&(path, source)| input(parse(path, source)))
        .collect();
    let view = OverlayView::build(archived, &inputs).unwrap();
    assert_eq!(
        OverlayView::from_cache_bytes(&view.to_cache_bytes(), archived.nodes.len()).as_ref(),
        Some(&view),
        "view cache round trip"
    );
    let merged = MergedGraph::new(archived, Some(&view));
    let mut actual: Vec<NameRefEdge> = merged
        .all_edges()
        .filter(|e| wanted(e.reason(archived)))
        .map(|e| {
            (
                merged.node(e.source).unwrap().uid(),
                merged.node(e.target).unwrap().uid(),
                e.rel_type(),
                e.reason(archived).to_owned(),
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    expected.sort_by(|a, b| (a.0, a.1, &a.3, a.4).cmp(&(b.0, b.1, &b.3, b.4)));
    actual.sort_by(|a, b| (a.0, a.1, &a.3, a.4).cmp(&(b.0, b.1, &b.3, b.4)));
    (expected, actual)
}

macro_rules! name_ref_parity {
    ($test:ident, $provider:expr, $path:literal, $value:literal, $value_count:expr, $callback:literal, $callback_count:expr) => {
        #[test]
        fn $test() {
            let provider = $provider;
            for (source, reason, expected_count) in [
                ($value, "value_read", $value_count),
                ($callback, "function_reference", $callback_count),
            ] {
                let shifted = format!("\n{source}");
                let (full, overlay) = field_read_edges(
                    &provider,
                    &[($path, source)],
                    &[($path, &shifted)],
                    &[reason],
                );
                assert_eq!(
                    full.len(),
                    expected_count,
                    "{reason} fixture must exercise the indexed path: {source}"
                );
                assert_eq!(overlay, full, "{reason} overlay parity: {source}");
            }
        }
    };
}

name_ref_parity!(
    test_overlay_typescript_name_refs_match_reindex,
    TypeScriptProvider::new().unwrap(),
    "app.ts",
    "const LIMITS = { max: 3 };\nfunction readLimit(): number { return LIMITS.max; }\n",
    1,
    "function handler(): void {}\nexport function wire(el: any): void {\n  el.addEventListener('click', handler);\n}\n",
    1
);
name_ref_parity!(
    test_overlay_javascript_name_refs_match_reindex,
    JavaScriptProvider::new().unwrap(),
    "app.js",
    "const LIMITS = { max: 3 };\nfunction readLimit() { return LIMITS.max; }\n",
    1,
    "function tick() {}\nexport function start() {\n  setTimeout(tick, 10);\n}\n",
    1
);
name_ref_parity!(
    test_overlay_python_name_refs_match_reindex,
    PythonProvider::new().unwrap(),
    "app.py",
    "LIMITS = [1]\ndef read_limit():\n    return LIMITS[0]\n",
    1,
    "import threading\n\ndef worker():\n    pass\n\ndef start():\n    threading.Thread(target=worker).start()\n",
    1
);
name_ref_parity!(
    test_overlay_java_name_refs_match_reindex,
    JavaProvider::new().unwrap(),
    "App.java",
    "class App { static final int LIMIT = 3; int read() { return LIMIT; } }\n",
    1,
    "class App {\n    void print(String s) {}\n    void run(java.util.List<String> xs) {\n        xs.forEach(this::print);\n    }\n}\n",
    1
);
name_ref_parity!(
    test_overlay_kotlin_name_refs_match_reindex,
    KotlinProvider::new().unwrap(),
    "app.kt",
    "val LIMIT = 3\nfun read(): Int = LIMIT\n",
    1,
    "fun transform(x: Int): Int = x\nfun apply(xs: List<Int>): List<Int> = xs.map(::transform)\n",
    1
);
name_ref_parity!(
    test_overlay_csharp_name_refs_match_reindex,
    CSharpProvider::new().unwrap(),
    "App.cs",
    "class App { const int Limit = 3; int Read() { return Limit; } }\n",
    1,
    "class App { void Print(string s) {} void Run(System.Collections.Generic.List<string> xs) { xs.ForEach(Print); } }\n",
    1
);
name_ref_parity!(
    test_overlay_go_name_refs_match_reindex,
    GoProvider::new().unwrap(),
    "app.go",
    "package app\nconst Limit = 3\nfunc Read() int { return Limit }\n",
    1,
    "package app\nfunc handler() {}\nfunc wire() { register(handler) }\nfunc register(f func()) {}\n",
    1
);
name_ref_parity!(
    test_overlay_rust_name_refs_match_reindex,
    RustProvider::new().unwrap(),
    "src/lib.rs",
    "const LIMIT: u32 = 3;\nfn read() -> u32 { LIMIT }\n",
    1,
    "fn parse_line(s: &str) -> u32 {\n    s.len() as u32\n}\n\npub fn parse_all(lines: &[&str]) -> Vec<u32> {\n    lines.iter().copied().map(parse_line).collect()\n}\n",
    1
);
name_ref_parity!(
    test_overlay_php_name_refs_match_reindex,
    PhpProvider::new().unwrap(),
    "app.php",
    "<?php\nconst LIMIT = 3;\nfunction read_limit() { return LIMIT; }\n",
    1,
    "<?php\nfunction fmt($x) { return $x; }\n\nfunction apply($xs) {\n    return array_map('fmt', $xs);\n}\n",
    1
);
name_ref_parity!(
    test_overlay_ruby_name_refs_match_reindex,
    RubyProvider::new().unwrap(),
    "app.rb",
    "LIMIT = 3\ndef read_limit\n  LIMIT\nend\n",
    1,
    "class App\n  def handler(x)\n    x\n  end\n  def wire(items)\n    items.each(&method(:handler))\n  end\nend\n",
    1
);
name_ref_parity!(
    test_overlay_swift_name_refs_match_reindex,
    SwiftProvider::new().unwrap(),
    "app.swift",
    "let limit = 3\nfunc read() -> Int { return limit }\n",
    1,
    "func transform(_ x: Int) -> Int { return x }\nfunc apply(_ xs: [Int]) -> [Int] { return xs.map(transform) }\n",
    1
);
name_ref_parity!(
    test_overlay_c_name_refs_match_reindex,
    CProvider::new().unwrap(),
    "app.c",
    "static const int LIMIT = 3;\nint read_limit(void) { return LIMIT; }\n",
    1,
    "#include <stdlib.h>\n\nint cmp(const void *a, const void *b) {\n    return 0;\n}\n\nvoid sort_all(int *xs, int n) {\n    qsort(xs, n, sizeof(int), cmp);\n}\n",
    1
);
name_ref_parity!(
    test_overlay_cpp_name_refs_match_reindex,
    CppProvider::new().unwrap(),
    "app.cpp",
    "constexpr int limit = 3;\nint read_limit() { return limit; }\n",
    1,
    "#include <algorithm>\n#include <vector>\n\nbool byValue(int a, int b) {\n    return a < b;\n}\n\nvoid sortAll(std::vector<int>& xs) {\n    std::sort(xs.begin(), xs.end(), byValue);\n}\n",
    1
);
name_ref_parity!(
    test_overlay_dart_name_refs_match_reindex,
    DartProvider::new().unwrap(),
    "app.dart",
    "const limit = 3;\nint readLimit() { return limit; }\n",
    1,
    "int transform(int x) { return x; }\nList<int> apply(List<int> xs) { return xs.map(transform).toList(); }\n",
    1
);

#[test]
fn test_overlay_java_inherited_value_read_matches_reindex() {
    let child =
        "package app;\npublic class Child extends Parent {\n    int read() { return LIMIT; }\n}\n";
    let shifted = format!("\n{child}");
    let (full, overlay) = field_read_edges(
        &JavaProvider::new().unwrap(),
        &[
            (
                "src/app/Parent.java",
                "package app;\npublic class Parent { static final int LIMIT = 3; }\n",
            ),
            ("src/app/Child.java", child),
        ],
        &[("src/app/Child.java", &shifted)],
        &["value_read"],
    );
    assert_eq!(
        full.len(),
        1,
        "the inherited field must be indexed: {full:?}"
    );
    assert_eq!(overlay, full, "the overlay must preserve inherited fields");
}

#[test]
fn test_overlay_csharp_inherited_value_read_matches_reindex() {
    let child = "public class Derived : Base {\n    int Read() { return LIMIT; }\n}\n";
    let shifted = format!("\n{child}");
    let (full, overlay) = field_read_edges(
        &CSharpProvider::new().unwrap(),
        &[
            (
                "Base.cs",
                "public class Base { public const int LIMIT = 3; }\n",
            ),
            ("Derived.cs", child),
        ],
        &[("Derived.cs", &shifted)],
        &["value_read"],
    );
    assert_eq!(
        full.len(),
        1,
        "the inherited field must be indexed: {full:?}"
    );
    assert_eq!(overlay, full, "the overlay must preserve inherited fields");
}

#[test]
fn test_overlay_java_static_value_import_matches_reindex() {
    let app = "package use;\nimport static app.Consts.TABLE;\npublic class User { int read() { return TABLE; } }\n";
    let shifted = format!("\n{app}");
    let (full, overlay) = field_read_edges(
        &java(),
        &[
            (
                "src/app/Consts.java",
                "package app; public class Consts { public static int TABLE = 1; }",
            ),
            ("src/use/User.java", app),
        ],
        &[("src/use/User.java", &shifted)],
        &["value_read"],
    );
    assert_eq!(
        full.len(),
        1,
        "the static value import must be indexed: {full:?}"
    );
    assert_eq!(
        overlay, full,
        "the overlay must keep the static value import"
    );
}

#[test]
fn test_overlay_kotlin_static_value_import_matches_reindex() {
    let app = "package use\nimport app.Consts.TABLE\nclass User { fun read(): Int = TABLE }\n";
    let shifted = format!("\n{app}");
    let (full, overlay) = field_read_edges(
        &kotlin(),
        &[
            (
                "src/app/Consts.kt",
                "package app\nobject Consts { val TABLE = 1 }\n",
            ),
            ("src/use/User.kt", app),
        ],
        &[("src/use/User.kt", &shifted)],
        &["value_read"],
    );
    assert_eq!(
        full.len(),
        1,
        "the static value import must be indexed: {full:?}"
    );
    assert_eq!(
        overlay, full,
        "the overlay must keep the static value import"
    );
}

#[test]
fn test_overlay_typescript_constant_read_shifted_reader_matches_reindex() {
    let app = "import { LIMITS } from './lib';\nexport function readLimit(): number {\n  return LIMITS.max;\n}\n";
    let shifted = format!("\n{app}");
    let (full, overlay) = field_read_edges(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("lib.ts", "export const LIMITS = { max: 3 };\n"),
            ("app.ts", app),
        ],
        &[("app.ts", &shifted)],
        &["value_read", "function_reference"],
    );
    assert_eq!(full.len(), 1, "one imported constant read: {full:?}");
    assert_eq!(full[0].2, RelType::ReadsField);
    assert_eq!(overlay, full);
}

#[test]
fn test_overlay_typescript_callback_argument_shifted_registrar_matches_reindex() {
    let app = "function handler(): void {}\nexport function wire(el: any): void {\n  el.addEventListener('click', handler);\n}\n";
    let shifted = format!("\n{app}");
    let (full, overlay) = field_read_edges(
        &TypeScriptProvider::new().unwrap(),
        &[("app.ts", app)],
        &[("app.ts", &shifted)],
        &["value_read", "function_reference"],
    );
    assert_eq!(full.len(), 1, "one callback reference: {full:?}");
    assert_eq!(full[0].2, RelType::References);
    assert_eq!(overlay, full);
}

#[test]
fn test_overlay_python_constant_read_and_callback_shifted_matches_reindex() {
    let app = "from lib import TIMEOUT\nimport threading\n\ndef worker():\n    pass\n\ndef start():\n    threading.Thread(target=worker).start()\n    return TIMEOUT\n";
    let shifted = format!("\n{app}");
    let (full, overlay) = field_read_edges(
        &PythonProvider::new().unwrap(),
        &[("lib.py", "TIMEOUT = 5\n"), ("app.py", app)],
        &[("app.py", &shifted)],
        &["value_read", "function_reference"],
    );
    assert_eq!(full.len(), 2, "one constant read, one callback: {full:?}");
    assert_eq!(overlay, full);
}

#[test]
fn test_overlay_typescript_value_read_added_and_removed_matches_reindex() {
    let base =
        "import { LIMITS } from './lib';\nexport function readLimit(): number { return 0; }\n";
    let with_read = "import { LIMITS } from './lib';\nexport function readLimit(): number { return LIMITS.max; }\n";
    let files = [
        ("lib.ts", "export const LIMITS = { max: 3 };\n"),
        ("app.ts", base),
    ];
    let provider = TypeScriptProvider::new().unwrap();
    let reasons = ["value_read", "function_reference"];

    let (added, added_overlay) =
        field_read_edges(&provider, &files, &[("app.ts", with_read)], &reasons);
    assert_eq!(added.len(), 1, "the dirty read must be indexed: {added:?}");
    assert_eq!(added_overlay, added, "the overlay must add a value_read");

    let (removed, removed_overlay) = field_read_edges(
        &provider,
        &[
            ("lib.ts", "export const LIMITS = { max: 3 };\n"),
            ("app.ts", with_read),
        ],
        &[("app.ts", base)],
        &reasons,
    );
    assert!(
        removed.is_empty(),
        "the reindex removes the value_read: {removed:?}"
    );
    assert_eq!(
        removed_overlay, removed,
        "the overlay must mask the stale value_read"
    );
}

#[test]
fn test_overlay_typescript_function_reference_added_and_removed_matches_reindex() {
    let base = "function handler(): void {}\nexport function wire(el: any): void {}\n";
    let with_reference = "function handler(): void {}\nexport function wire(el: any): void { el.addEventListener('click', handler); }\n";
    let provider = TypeScriptProvider::new().unwrap();
    let reasons = ["value_read", "function_reference"];

    let (added, added_overlay) = field_read_edges(
        &provider,
        &[("app.ts", base)],
        &[("app.ts", with_reference)],
        &reasons,
    );
    assert_eq!(
        added.len(),
        1,
        "the dirty callback must be indexed: {added:?}"
    );
    assert_eq!(added[0].2, RelType::References);
    assert_eq!(
        added_overlay, added,
        "the overlay must add a function_reference"
    );

    let (removed, removed_overlay) = field_read_edges(
        &provider,
        &[("app.ts", with_reference)],
        &[("app.ts", base)],
        &reasons,
    );
    assert!(
        removed.is_empty(),
        "the reindex removes the function_reference: {removed:?}"
    );
    assert_eq!(
        removed_overlay, removed,
        "the overlay must mask the stale function_reference"
    );
}

#[test]
fn test_overlay_java_field_read_external_import_matches_reindex() {
    let source = "package app;\nimport external.value;\nclass Main { int read(Config c) { return c.value; } }\n";
    let shifted = format!("\n{source}");
    let (full, overlay) = field_read_edges(
        &JavaProvider::new().unwrap(),
        &[
            ("src/app/Main.java", source),
            (
                "src/app/Config.java",
                "package app;\nclass Config { int value; }\n",
            ),
        ],
        &[("src/app/Main.java", &shifted)],
        &["field_read"],
    );
    assert_eq!(full.len(), 1, "the index must bind the unique field");
    assert_eq!(overlay, full, "a type import must not hide a field read");
}

#[test]
fn test_overlay_java_field_read_wildcard_import_preserves_ambiguity() {
    let source =
        "package app;\nimport model.*;\nclass Main { int read(Config c) { return c.value; } }\n";
    let shifted = format!("\n{source}");
    let (full, overlay) = field_read_edges(
        &JavaProvider::new().unwrap(),
        &[
            ("src/app/Main.java", source),
            (
                "src/model/Config.java",
                "package model;\nclass Config { int value; }\n",
            ),
            (
                "src/other/Other.java",
                "package other;\nclass Other { int value; }\n",
            ),
        ],
        &[("src/app/Main.java", &shifted)],
        &["field_read"],
    );
    assert!(
        full.is_empty(),
        "the index must preserve field ambiguity: {full:?}"
    );
    assert_eq!(overlay, full, "a wildcard must not invent a field target");
}

#[test]
fn test_overlay_typescript_field_read_retained_matches_reindex() {
    let source = "class Config { timeout: number = 0; }\nexport function readTimeout(c: Config): number { return c.timeout; }\n";
    let shifted = format!("\n{source}");
    let (full, overlay) = field_read_edges(
        &TypeScriptProvider::new().unwrap(),
        &[("app.ts", source)],
        &[("app.ts", &shifted)],
        &["field_read"],
    );
    assert_eq!(full.len(), 1, "the read must exercise the indexed path");
    assert_eq!(overlay, full, "the overlay must rebuild the field read");
}

#[test]
fn test_overlay_typescript_field_read_removed_masks_base_edge() {
    let with_read = "class Config { timeout: number = 0; }\nexport function readTimeout(c: Config): number { return c.timeout; }\n";
    let without_read = "class Config { timeout: number = 0; }\nexport function readTimeout(c: Config): number { return 0; }\n";
    let provider = TypeScriptProvider::new().unwrap();
    let (full, overlay) = field_read_edges(
        &provider,
        &[("app.ts", with_read)],
        &[("app.ts", without_read)],
        &["field_read"],
    );
    assert!(
        full.is_empty(),
        "the reindex removes the field read: {full:?}"
    );
    assert_eq!(overlay, full, "the overlay must mask the stale field_read");
}

// ---- Chained member calls typed by the inner callee's return type ----
// (FU-2026-10-22-67874c602c84). The reindex binds `member` on `owner` only;
// the overlay must bind the same, whether the factory's return type comes
// from the archived base graph (factory clean) or from the fresh parse
// (factory dirty).

fn chain_parity(
    caller: &str,
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    member: &str,
    owner: &str,
) {
    let tmp = tempfile::tempdir().unwrap();
    let expected = targets(&reindex(tmp.path(), provider, files, dirty), caller);
    let mut owners: Vec<&str> = expected
        .iter()
        .filter(|(_, name, _, _)| name == member)
        .map(|(_, _, owner, _)| owner.as_str())
        .collect();
    owners.dedup();
    assert_eq!(
        owners,
        [owner],
        "reindex binds `{owner}.{member}` from the return type: {expected:?}"
    );
    check_parity_from(caller, provider, files, dirty, &expected, &expected);
}

fn chain_without_member_parity(
    caller: &str,
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    member: &str,
) {
    let tmp = tempfile::tempdir().unwrap();
    let expected = targets(&reindex(tmp.path(), provider, files, dirty), caller);
    assert!(
        !expected.iter().any(|(_, name, _, _)| name == member),
        "the unresolved return type must not bind a namesake member: {expected:?}"
    );
    check_parity_from(caller, provider, files, dirty, &expected, &expected);
}

const TS_FACTORY: (&str, &str) = (
    "foo.ts",
    "export class Foo {\n  static create(): Foo { return new Foo(); }\n  bar(): void {}\n}\nexport class Other {\n  bar(): void {}\n}\nexport function make(): Foo { return new Foo(); }\n",
);
const TS_FACTORY_TO_OTHER: &str = "export class Foo {\n  static create(): Other { return new Other(); }\n  bar(): void {}\n}\nexport class Other {\n  bar(): void {}\n}\nexport function make(): Other { return new Other(); }\n";
const TS_CHAIN_CALLER: (&str, &str) = (
    "app.ts",
    "import { Foo, make } from './foo';\nexport function run(): void {}\n",
);
const TS_CHAIN_CALLER_DIRTY: &str = "import { Foo, make } from './foo';\nexport function run(): void {\n  Foo.create().bar();\n  make().bar();\n}\n";

#[test]
fn test_overlay_typescript_chained_member_clean_factory_matches_reindex() {
    chain_parity(
        "run",
        &TypeScriptProvider::new().unwrap(),
        &[TS_FACTORY, TS_CHAIN_CALLER],
        &[("app.ts", TS_CHAIN_CALLER_DIRTY)],
        "bar",
        "Foo",
    );
}

#[test]
fn test_overlay_typescript_chained_member_dirty_factory_return_type_matches_reindex() {
    chain_parity(
        "run",
        &TypeScriptProvider::new().unwrap(),
        &[TS_FACTORY, TS_CHAIN_CALLER],
        &[
            ("foo.ts", TS_FACTORY_TO_OTHER),
            ("app.ts", TS_CHAIN_CALLER_DIRTY),
        ],
        "bar",
        "Other",
    );
}

const PY_FACTORY: (&str, &str) = (
    "foo.py",
    "class Foo:\n    @staticmethod\n    def create() -> \"Foo\":\n        return Foo()\n\n    def bar(self):\n        pass\n\n\nclass Other:\n    def bar(self):\n        pass\n\n\ndef make() -> Foo:\n    return Foo()\n",
);
const PY_FACTORY_TO_OTHER: &str = "class Foo:\n    @staticmethod\n    def create() -> \"Other\":\n        return Other()\n\n    def bar(self):\n        pass\n\n\nclass Other:\n    def bar(self):\n        pass\n\n\ndef make() -> Other:\n    return Other()\n";
const PY_CHAIN_CALLER: (&str, &str) = (
    "app.py",
    "from foo import Foo, make\n\n\ndef run():\n    pass\n",
);
const PY_CHAIN_CALLER_DIRTY: &str =
    "from foo import Foo, make\n\n\ndef run():\n    Foo.create().bar()\n    make().bar()\n";

#[test]
fn test_overlay_python_chained_member_clean_factory_matches_reindex() {
    chain_parity(
        "run",
        &PythonProvider::new().unwrap(),
        &[PY_FACTORY, PY_CHAIN_CALLER],
        &[("app.py", PY_CHAIN_CALLER_DIRTY)],
        "bar",
        "Foo",
    );
}

#[test]
fn test_overlay_python_chained_member_dirty_factory_return_type_matches_reindex() {
    chain_parity(
        "run",
        &PythonProvider::new().unwrap(),
        &[PY_FACTORY, PY_CHAIN_CALLER],
        &[
            ("foo.py", PY_FACTORY_TO_OTHER),
            ("app.py", PY_CHAIN_CALLER_DIRTY),
        ],
        "bar",
        "Other",
    );
}

// A Java callee's imports never reach the overlay (`graph.bin` archives
// Python / TS / JS imports only), so both bind a chain only through a return
// type the callee's own package (directory) declares.

const JAVA_CHAIN_FOO: (&str, &str) = (
    "src/app/Foo.java",
    "package app;\n\npublic class Foo {\n    public static Foo create() { return new Foo(); }\n    public void bar() {}\n}\n",
);
const JAVA_CHAIN_OTHER: (&str, &str) = (
    "src/app/Other.java",
    "package app;\n\npublic class Other {\n    public void bar() {}\n}\n",
);
const JAVA_CHAIN_FACTORY: (&str, &str) = (
    "src/app/Factory.java",
    "package app;\n\npublic class Factory {\n    public static Foo make() { return new Foo(); }\n}\n",
);
const JAVA_CHAIN_USE: (&str, &str) = (
    "src/app/Use.java",
    "package app;\n\npublic class Use {\n    void run() {}\n}\n",
);

#[test]
fn test_overlay_java_chained_member_same_file_return_type_matches_reindex() {
    chain_parity(
        "run",
        &java(),
        &[JAVA_CHAIN_FOO, JAVA_CHAIN_OTHER, JAVA_CHAIN_USE],
        &[(
            "src/app/Use.java",
            "package app;\n\npublic class Use {\n    void run() { Foo.create().bar(); }\n}\n",
        )],
        "bar",
        "Foo",
    );
}

#[test]
fn test_overlay_java_chained_member_same_package_return_type_matches_reindex() {
    chain_parity(
        "run",
        &java(),
        &[
            JAVA_CHAIN_FOO,
            JAVA_CHAIN_OTHER,
            JAVA_CHAIN_FACTORY,
            JAVA_CHAIN_USE,
        ],
        &[(
            "src/app/Use.java",
            "package app;\n\npublic class Use {\n    void run() { Factory.make().bar(); }\n}\n",
        )],
        "bar",
        "Foo",
    );
}

#[test]
fn test_overlay_java_chained_member_imported_return_type_matches_reindex() {
    let files = [
        JAVA_CHAIN_FOO,
        JAVA_CHAIN_OTHER,
        (
            "src/factory/Factory.java",
            "package factory;\n\nimport app.Foo;\n\npublic class Factory {\n    public static Foo make() { return new Foo(); }\n}\n",
        ),
        JAVA_CHAIN_USE,
    ];
    let dirty = [(
        "src/app/Use.java",
        "package app;\n\nimport factory.Factory;\n\npublic class Use {\n    void run() { Factory.make().bar(); }\n}\n",
    )];
    chain_parity("run", &java(), &files, &dirty, "bar", "Foo");
}

#[test]
fn test_overlay_java_chained_member_wildcard_return_type_matches_reindex() {
    let factory = (
        "src/factory/Factory.java",
        "package factory;\nimport app.*;\npublic class Factory { public static Foo make() { return new Foo(); } }\n",
    );
    let caller = (
        "src/app/Use.java",
        "package app;\nimport factory.Factory;\npublic class Use { void run() { Factory.make().bar(); } }\n",
    );
    let files = [JAVA_CHAIN_FOO, JAVA_CHAIN_OTHER, factory, JAVA_CHAIN_USE];
    chain_parity("run", &java(), &files, &[caller], "bar", "Foo");
    chain_parity("run", &java(), &files, &[factory, caller], "bar", "Foo");
}

#[test]
fn test_overlay_java_chained_member_dirty_imported_factory_matches_reindex() {
    let factory = "package factory;\n\nimport app.Other;\n\npublic class Factory {\n    public static Other make() { return new Other(); }\n}\n";
    chain_parity(
        "run",
        &java(),
        &[
            JAVA_CHAIN_FOO,
            JAVA_CHAIN_OTHER,
            (
                "src/factory/Factory.java",
                "package factory;\n\nimport app.Foo;\n\npublic class Factory {\n    public static Foo make() { return new Foo(); }\n}\n",
            ),
            JAVA_CHAIN_USE,
        ],
        &[
            ("src/factory/Factory.java", factory),
            (
                "src/app/Use.java",
                "package app;\n\nimport factory.Factory;\n\npublic class Use {\n    void run() { Factory.make().bar(); }\n}\n",
            ),
        ],
        "bar",
        "Other",
    );
}

#[test]
fn test_overlay_kotlin_chained_member_imported_return_type_matches_reindex() {
    let factory =
        "package services\nimport models.Foo\nobject Factory { fun make(): Foo = Foo() }\n";
    let use_file = "package app\nimport services.Factory\nfun run() { Factory.make().bar() }\n";
    chain_parity(
        "run",
        &kotlin(),
        &[
            (
                "src/models/Foo.kt",
                "package models\nclass Foo { fun bar() {} }\n",
            ),
            (
                "src/decoy/Other.kt",
                "package decoy\nclass Other { fun bar() {} }\n",
            ),
            ("src/services/Factory.kt", factory),
            (
                "src/app/Use.kt",
                "package app\nimport services.Factory\nfun run() {}\n",
            ),
        ],
        &[("src/app/Use.kt", use_file)],
        "bar",
        "Foo",
    );
}

#[test]
fn test_overlay_kotlin_chained_member_dirty_imported_factory_matches_reindex() {
    chain_parity(
        "run",
        &kotlin(),
        &[
            ("src/models/Foo.kt", "package models\nclass Foo { fun bar() {} }\n"),
            ("src/decoy/Other.kt", "package decoy\nclass Other { fun bar() {} }\n"),
            ("src/services/Factory.kt", "package services\nimport models.Foo\nobject Factory { fun make(): Foo = Foo() }\n"),
            ("src/app/Use.kt", "package app\nimport services.Factory\nfun run() {}\n"),
        ],
        &[
            ("src/services/Factory.kt", "package services\nimport decoy.Other\nobject Factory { fun make(): Other = Other() }\n"),
            ("src/app/Use.kt", "package app\nimport services.Factory\nfun run() { Factory.make().bar() }\n"),
        ],
        "bar",
        "Other",
    );
}

#[test]
fn test_overlay_csharp_chained_member_imported_return_type_matches_reindex() {
    let factory = "using Models;\nnamespace Services { public class Factory { public static Foo Make() { return new Foo(); } } }\n";
    let use_file = "using Services;\nnamespace App { public class Use { public void Run() { Factory.Make().Bar(); } } }\n";
    chain_parity(
        "Run",
        &CSharpProvider::new().unwrap(),
        &[
            (
                "src/Models/Foo.cs",
                "namespace Models { public class Foo { public void Bar() {} } }\n",
            ),
            (
                "src/Decoy/Other.cs",
                "namespace Decoy { public class Other { public void Bar() {} } }\n",
            ),
            ("src/Services/Factory.cs", factory),
            (
                "src/App/Use.cs",
                "using Services;\nnamespace App { public class Use { public void Run() {} } }\n",
            ),
        ],
        &[("src/App/Use.cs", use_file)],
        "Bar",
        "Foo",
    );
}

#[test]
fn test_overlay_csharp_chained_member_alias_return_type_matches_reindex() {
    let factory = "using Alias = Models.ModelValue;\nnamespace Services { public class Factory { public static Alias Make() { return new Alias(); } } }\n";
    let use_file = "using Services;\nnamespace App { public class Use { public void Run() { Factory.Make().Bar(); } } }\n";
    chain_parity(
        "Run",
        &CSharpProvider::new().unwrap(),
        &[
            (
                "src/Models/ModelValue.cs",
                "namespace Models { public class ModelValue { public void Bar() {} } }\n",
            ),
            (
                "src/Decoy/DecoyValue.cs",
                "namespace Decoy { public class DecoyValue { public void Bar() {} } }\n",
            ),
            ("src/Services/Factory.cs", factory),
            (
                "src/App/Use.cs",
                "using Services;\nnamespace App { public class Use { public void Run() {} } }\n",
            ),
        ],
        &[("src/App/Use.cs", use_file)],
        "Bar",
        "ModelValue",
    );
}

#[test]
fn test_overlay_csharp_chained_member_file_scoped_return_type_matches_reindex() {
    let factory = "using Models;\nnamespace Services;\npublic class Factory { public static ModelValue Make() { return new ModelValue(); } }\n";
    let use_file = "using Services;\nnamespace App;\npublic class Use { public void Run() { Factory.Make().Bar(); } }\n";
    chain_parity(
        "Run",
        &CSharpProvider::new().unwrap(),
        &[
            (
                "src/Models/ModelValue.cs",
                "namespace Models;\npublic class ModelValue { public void Bar() {} }\n",
            ),
            (
                "src/Decoy/DecoyValue.cs",
                "namespace Decoy;\npublic class DecoyValue { public void Bar() {} }\n",
            ),
            ("src/Services/Factory.cs", factory),
            (
                "src/App/Use.cs",
                "using Services;\nnamespace App;\npublic class Use { public void Run() {} }\n",
            ),
        ],
        &[("src/App/Use.cs", use_file)],
        "Bar",
        "ModelValue",
    );
}

#[test]
fn test_overlay_csharp_nested_known_missing_member_returns_no_outer_edge() {
    let files = [
        ("src/Models/Foo.cs", "namespace Models { public class Foo {} }\n"),
        ("src/Decoy/Decoy.cs", "namespace Decoy { public class Decoy { public void Bar() {} } }\n"),
        ("src/Services/Factory.cs", "using Models;\nnamespace Services { public class Factory { public static Foo Make() { return new Foo(); } } }\n"),
        ("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Run() {} } }\n"),
    ];
    let dirty = [(
        "src/App/Use.cs",
        "using Services;\nnamespace App { public class Use { public void Run() { Factory.Make().Missing().Bar(); } } }\n",
    )];
    let tmp = tempfile::tempdir().unwrap();
    let expected = targets(
        &reindex(tmp.path(), &CSharpProvider::new().unwrap(), &files, &dirty),
        "Run",
    );
    assert!(
        expected
            .iter()
            .any(|(_, name, owner, _)| name == "Make" && owner == "Factory"),
        "the innermost factory call stays bound: {expected:?}"
    );
    assert!(
        !expected
            .iter()
            .any(|(_, name, _, _)| name == "Missing" || name == "Bar"),
        "a known missing member must suppress outer fallback edges: {expected:?}"
    );
    check_parity_from(
        "Run",
        &CSharpProvider::new().unwrap(),
        &files,
        &dirty,
        &expected,
        &expected,
    );
}

#[test]
fn test_overlay_csharp_chained_member_dirty_imported_factory_matches_reindex() {
    chain_parity(
        "Run",
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Models/Foo.cs", "namespace Models { public class Foo { public void Bar() {} } }\n"),
            ("src/Decoy/Other.cs", "namespace Decoy { public class Other { public void Bar() {} } }\n"),
            ("src/Services/Factory.cs", "using Models;\nnamespace Services { public class Factory { public static Foo Make() { return new Foo(); } } }\n"),
            ("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Run() {} } }\n"),
        ],
        &[
            ("src/Services/Factory.cs", "using Decoy;\nnamespace Services { public class Factory { public static Other Make() { return new Other(); } } }\n"),
            ("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Run() { Factory.Make().Bar(); } } }\n"),
        ],
        "Bar",
        "Other",
    );
}

#[test]
fn test_overlay_csharp_chained_member_external_alias_does_not_bind_local_namesake() {
    chain_without_member_parity(
        "Run",
        &CSharpProvider::new().unwrap(),
        &[
            (
                "src/Project/Foo.cs",
                "namespace Project { public class Foo { public void Bar() {} } }\n",
            ),
            (
                "src/Services/Factory.cs",
                "using Alias = External.Foo;\nnamespace Services { public class Factory { public static Alias Make() { return null; } } }\n",
            ),
            (
                "src/App/Use.cs",
                "using Services;\nnamespace App { public class Use { public void Run() {} } }\n",
            ),
        ],
        &[(("src/App/Use.cs"), "using Services;\nnamespace App { public class Use { public void Run() { Factory.Make().Bar(); } } }\n")],
        "Bar",
    );
}

#[test]
fn test_overlay_csharp_chained_member_same_leaf_namespaces_stay_suppressed() {
    chain_without_member_parity(
        "Run",
        &CSharpProvider::new().unwrap(),
        &[
            (
                "src/Models.cs",
                "namespace First.Shared { public class Foo { public void Bar() {} } }\nnamespace Second.Shared { public class Foo { public void Bar() {} } }\n",
            ),
            (
                "src/Services/Factory.cs",
                "using First.Shared;\nnamespace Services { public class Factory { public static Foo Make() { return new Foo(); } } }\n",
            ),
            (
                "src/App/Use.cs",
                "using Services;\nnamespace App { public class Use { public void Run() {} } }\n",
            ),
        ],
        &[(("src/App/Use.cs"), "using Services;\nnamespace App { public class Use { public void Run() { Factory.Make().Bar(); } } }\n")],
        "Bar",
    );
}

#[test]
fn test_overlay_csharp_external_return_type_does_not_bind_invisible_project_namesake() {
    chain_without_member_parity(
        "Run",
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Project/Foo.cs", "namespace Project { public class Foo { public void Bar() {} } }\n"),
            ("src/Services/Factory.cs", "using External;\nnamespace Services { public class Factory { public static Foo Make() { return null; } } }\n"),
            ("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Run() {} } }\n"),
        ],
        &[("src/App/Use.cs", "using Services;\nnamespace App { public class Use { public void Run() { Factory.Make().Bar(); } } }\n")],
        "Bar",
    );
}

#[test]
fn test_overlay_swift_chained_member_imported_return_type_matches_reindex() {
    let factory = "import Models\npublic class Factory { public static func make() -> Foo { return Foo() } }\n";
    let use_file = "import Services\nfunc run() { Factory.make().bar() }\n";
    chain_parity(
        "run",
        &SwiftProvider::new().unwrap(),
        &[
            (
                "Sources/Models/Foo.swift",
                "public class Foo { public func bar() {} }\n",
            ),
            (
                "Sources/Decoy/Other.swift",
                "public class Other { public func bar() {} }\n",
            ),
            (
                "OtherPackage/Sources/Models/Foo.swift",
                "public class Foo { public func bar() {} }\n",
            ),
            ("Sources/Services/Factory.swift", factory),
            ("Sources/App/Use.swift", "import Services\nfunc run() {}\n"),
        ],
        &[("Sources/App/Use.swift", use_file)],
        "bar",
        "Foo",
    );
}

#[test]
fn test_overlay_swift_chained_member_dirty_imported_factory_matches_reindex() {
    chain_parity(
        "run",
        &SwiftProvider::new().unwrap(),
        &[
            ("Sources/Models/Foo.swift", "public class Foo { public func bar() {} }\n"),
            ("Sources/Decoy/Other.swift", "public class Other { public func bar() {} }\n"),
            ("Sources/Services/Factory.swift", "import Models\npublic class Factory { public static func make() -> Foo { return Foo() } }\n"),
            ("Sources/App/Use.swift", "import Services\nfunc run() {}\n"),
        ],
        &[
            ("Sources/Services/Factory.swift", "import Decoy\npublic class Factory { public static func make() -> Other { return Other() } }\n"),
            ("Sources/App/Use.swift", "import Services\nfunc run() { Factory.make().bar() }\n"),
        ],
        "bar",
        "Other",
    );
}
