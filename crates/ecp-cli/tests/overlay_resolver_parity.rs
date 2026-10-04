use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
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
                calls: n.calls,
            })
            .collect(),
    }
}

type Target = (String, String, String);

fn targets(graph: &ZeroCopyGraph) -> Vec<Target> {
    let pool = &graph.string_pool;
    let mut result: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::Calls
                && graph.nodes[e.source as usize].name.resolve(pool) == "run"
        })
        .map(|e| {
            let n = &graph.nodes[e.target as usize];
            (
                graph.files[n.file_idx as usize].path.resolve(pool).into(),
                n.name.resolve(pool).into(),
                n.owner_class.resolve(pool).into(),
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
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"overlay-parity\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let build = |edits: &[(&str, &str)]| {
        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        for &(path, original) in files {
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
        targets(&full),
        expected,
        "full reindex fixture must exercise the intended target"
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
            .any(|s| s.name == "run" && !s.calls.is_empty()),
        "the dirty caller must contain a parsed call, including unresolved fixtures"
    );
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(archived, &files).unwrap();
    let mut actual: Vec<Target> = view
        .edges()
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && view.node(e.source).unwrap().name == "run")
        .map(|e| {
            if let Some(n) = view.node(e.target) {
                (
                    n.rel_path.to_string(),
                    n.name.clone(),
                    n.owner_class.clone().unwrap_or_default(),
                )
            } else {
                let n = &base.nodes[e.target as usize];
                (
                    base.files[n.file_idx as usize]
                        .path
                        .resolve(&base.string_pool)
                        .into(),
                    n.name.resolve(&base.string_pool).into(),
                    n.owner_class.resolve(&base.string_pool).into(),
                )
            }
        })
        .collect();
    actual.sort();
    actual.dedup();
    assert_eq!(
        actual,
        targets(&full),
        "overlay Calls must equal a reindex of the dirty content"
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
            &[("src/a/b.rs".into(), "foo".into(), "".into())],
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
        &[("app.py".into(), "foo".into(), "".into())],
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
        &[("src/other.rs".into(), "foo".into(), "Foo".into())],
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
        &[("src/bin/util.rs".into(), "foo".into(), "".into())],
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
                &[($path.into(), "foo".into(), $owner.into())],
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
#[ignore = "manual before/after timing over the tracked Rust sources of this repository"]
fn test_overlay_build_real_repository_timings() {
    let repo = std::env::var_os("T4_BENCH_REPO")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let listed = std::process::Command::new("git")
        .args(["ls-files", "-z", "*.rs"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(listed.status.success());
    let provider = RustProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    let mut dirty = None;
    for path in std::str::from_utf8(&listed.stdout)
        .unwrap()
        .split('\0')
        .filter(|s| !s.is_empty())
    {
        let source = std::fs::read(repo.join(path)).unwrap();
        let local = provider.parse_file(Path::new(path), &source).unwrap();
        if path == "crates/ecp-cli/src/commands/impact/bfs.rs" {
            let mut changed = source.clone();
            changed.extend_from_slice(b"\nfn overlay_benchmark_dirty_call() { run_bfs(); crate::commands::format::kind_to_str(); super::bfs::run_bfs(); self::missing::probe(); let x = unknown(); x.foo(); }\n");
            dirty = Some(input(
                provider.parse_file(Path::new(path), &changed).unwrap(),
            ));
        }
        builder.add_graph(local);
    }
    let graph = builder.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let files = [dirty.expect("tracked overlay source")];
    let mut samples = Vec::new();
    for _ in 0..6 {
        let start = std::time::Instant::now();
        let view = OverlayView::build(archived, &files).unwrap();
        std::hint::black_box(&view);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.remove(0);
    eprintln!(
        "OVERLAY_BENCH files={} nodes={} dirty_symbols={} ms={samples:?}",
        graph.files.len(),
        graph.nodes.len(),
        files[0].symbols.len()
    );
    samples.sort_by(f64::total_cmp);
    eprintln!(
        "OVERLAY_BENCH median_ms={} range_ms={}..{}",
        samples[2], samples[0], samples[4]
    );
}
