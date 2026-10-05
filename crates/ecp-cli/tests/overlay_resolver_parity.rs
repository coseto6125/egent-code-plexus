use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlaySymbol, OverlayView};
use std::path::Path;

fn input(local: LocalGraph) -> OverlayFileInput {
    OverlayFileInput {
        rel_path: local.file_path.to_string_lossy().into_owned(),
        imports: local.imports,
        symbols: local
            .nodes
            .into_iter()
            .map(|n| OverlaySymbol {
                name: n.name,
                kind: n.kind,
                owner_class: n.owner_class,
                start_line: n.span.0 + 1,
                end_line: n.span.2 + 1,
                start_column: n.span.1,
                end_column: n.span.3,
                calls: n.calls,
            })
            .collect(),
    }
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

fn check_parity_from(
    caller: &str,
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    expected: &[Target],
    overlay_expected: &[Target],
) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"overlay-parity\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let build = |edits: &[(&str, &str)]| {
        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
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
            let disk = tmp.path().join(path);
            std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
            std::fs::write(disk, source).unwrap();
            builder.add_graph(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .unwrap(),
            );
        }
        builder.build()
    };
    let base = build(&[]);
    let full = build(dirty);
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

#[test]
fn test_overlay_python_untyped_member_collision_matches_reindex() {
    parity(
        &PythonProvider::new().unwrap(),
        &[
            ("app.py", "def foo(): pass\ndef run(x): pass\n"),
            ("other.py", "class Foo:\n    def foo(self): pass\n"),
        ],
        &[("app.py", "def foo(): pass\ndef run(x): x.foo()\n")],
        &[("app.py".into(), "foo".into(), "".into(), 100)],
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
// index keeps the receiver ladder (Root.setup, inherited). The overlay has
// no ladder, so it leaves the call unresolved. Was: both retried the bare
// name and bound the caller to itself.
#[test]
fn test_overlay_kotlin_super_member_outside_imported_file_matches_reindex() {
    let screen = |body: &str| {
        format!(
            "package app\nimport base.BaseScreen\nclass Screen : BaseScreen() {{\n override fun setup() {{ {body} }}\n}}\n"
        )
    };
    let (clean, dirty) = (screen(""), screen("super.setup()"));
    check_parity_from(
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
        &[],
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
// wildcard imports no class member: the index resolves `refresh` on Base
// (0.8). The overlay has no heritage tier and two `refresh` methods, so it
// leaves no edge. Was: both bound the wildcard's `Adapter.refresh` at 0.95.
#[test]
fn test_overlay_kotlin_wildcard_below_heritage_documents_heritage_divergence() {
    let screen = |body: &str| {
        format!(
            "package app\nimport lib.Base\nimport util.*\nclass Screen : Base() {{\n fun run() {{ {body} }}\n}}\n"
        )
    };
    let (clean, dirty) = (screen(""), screen("refresh()"));
    check_parity(
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
        &[],
    );
}
