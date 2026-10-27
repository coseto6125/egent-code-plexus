//! R24 parity: a plain field read (`c.timeout`) binds the same `Property`
//! through the full build (`Resolver::resolve_field_read`) and through the
//! overlay of dirty copies of its files, as both reach their tiers through
//! `ladder::field_read`.
//!
//! Each fixture plants three reads: one the reader's own file declares
//! (Tier 1, confidence 1.0), one that only another file declares (Tier 3,
//! 0.7), and one that no file declares (no edge). Every file is dirty, so
//! the overlay rebuilds each read from the fresh parse.
//!
//! Ruby is the one exemption among the 14 mainstream languages: `obj.attr`
//! is a method call there, so the parser records no field read
//! (`ruby/parser.rs` passes an empty field-access kind list to
//! `extract_field_reads`), and `reads_field_all_langs.rs` pins that.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

const FIELD_READ: &str = "field_read";

/// One `ReadsField` edge: `(source uid, target uid, reason, confidence x 100)`.
type ReadEdge = (u64, u64, String, u32);

/// The three views of one fixture's reads: the full build's edges, the
/// overlay's edges, and the full build's `(reader, field, confidence x 100)`.
type FieldReadViews = (Vec<ReadEdge>, Vec<ReadEdge>, Vec<(String, String, u32)>);

/// The `field_read` edges of the full build of `files` with every file
/// shifted down one line, the overlay's edges over the unshifted base with
/// the same shifted files dirty, and the full build's edges by name:
/// `(reader, field, confidence x 100)`.
fn field_read_edges(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> FieldReadViews {
    let parse = |path: &str, source: &str| {
        provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap()
    };
    let shifted: Vec<(&str, String)> = files
        .iter()
        .map(|&(path, source)| (path, format!("\n{source}")))
        .collect();
    let build = |sources: &[(&str, &str)]| -> ZeroCopyGraph {
        let mut builder = GraphBuilder::new();
        for &(path, source) in sources {
            builder.add_graph(parse(path, source));
        }
        builder.build()
    };
    let dirty: Vec<(&str, &str)> = shifted
        .iter()
        .map(|(path, source)| (*path, source.as_str()))
        .collect();
    let base = build(files);
    let full = build(&dirty);
    let pool = &full.string_pool;
    let reads = || {
        full.edges
            .iter()
            .filter(|e| e.rel_type == RelType::ReadsField && e.reason.resolve(pool) == FIELD_READ)
    };
    let mut expected: Vec<ReadEdge> = reads()
        .map(|e| {
            (
                full.nodes[e.source as usize].uid,
                full.nodes[e.target as usize].uid,
                e.reason.resolve(pool).to_owned(),
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    let mut by_name: Vec<(String, String, u32)> = reads()
        .map(|e| {
            (
                full.nodes[e.source as usize].name.resolve(pool).to_owned(),
                full.nodes[e.target as usize].name.resolve(pool).to_owned(),
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let inputs: Vec<OverlayFileInput> = dirty
        .iter()
        .map(|&(path, source)| parse(path, source).into())
        .collect();
    let view = OverlayView::build(archived, &inputs).unwrap();
    let merged = MergedGraph::new(archived, Some(&view));
    let mut actual: Vec<ReadEdge> = merged
        .all_edges()
        .filter(|e| e.rel_type() == RelType::ReadsField && e.reason(archived) == FIELD_READ)
        .map(|e| {
            (
                merged.node(e.source).unwrap().uid(),
                merged.node(e.target).unwrap().uid(),
                e.reason(archived).to_owned(),
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    expected.sort();
    actual.sort();
    by_name.sort();
    (expected, actual, by_name)
}

/// The full build binds the local read at Tier 1 and the remote one at
/// Tier 3, the missing field binds nothing, and the overlay emits the same
/// edge multiset.
fn assert_binds_same_on_both_sides(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    (local, remote, field): (&str, &str, &str),
) {
    let (full, overlay, by_name) = field_read_edges(provider, files);
    let mut want = vec![
        (local.to_owned(), field.to_owned(), 100),
        (remote.to_owned(), field.to_owned(), 70),
    ];
    want.sort();
    assert_eq!(
        by_name, want,
        "the full build binds Tier 1 and Tier 3, and no missing field"
    );
    assert_eq!(overlay, full, "the overlay binds every read as the build");
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_typescript() {
    assert_binds_same_on_both_sides(
        &TypeScriptProvider::new().unwrap(),
        &[
            (
                "src/config.ts",
                "class Config { timeout: number = 0; }\nfunction readLocal(c: Config): number { return c.timeout; }\n",
            ),
            (
                "src/remote.ts",
                "function readRemote(c: any): number { return c.timeout + c.missing; }\n",
            ),
        ],
        ("readLocal", "readRemote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_python() {
    assert_binds_same_on_both_sides(
        &PythonProvider::new().unwrap(),
        &[
            (
                "pkg/config.py",
                "class Config:\n    timeout: int = 0\n\ndef read_local(c):\n    return c.timeout\n",
            ),
            (
                "pkg/remote.py",
                "def read_remote(c):\n    return c.timeout + c.missing\n",
            ),
        ],
        ("read_local", "read_remote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_java() {
    assert_binds_same_on_both_sides(
        &JavaProvider::new().unwrap(),
        &[
            (
                "src/app/Config.java",
                "package app;\nclass Config { public int timeout; }\nclass LocalReader { int readLocal(Config c) { return c.timeout; } }\n",
            ),
            (
                "src/app/Remote.java",
                "package app;\nclass RemoteReader { int readRemote(Config c) { return c.timeout + c.missing; } }\n",
            ),
        ],
        ("readLocal", "readRemote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_go() {
    assert_binds_same_on_both_sides(
        &GoProvider::new().unwrap(),
        &[
            (
                "app/config.go",
                "package app\ntype Config struct { Timeout int }\nfunc readLocal(c Config) int { return c.Timeout }\n",
            ),
            (
                "app/remote.go",
                "package app\nfunc readRemote(c Config) int { return c.Timeout + c.Missing }\n",
            ),
        ],
        ("readLocal", "readRemote", "Timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_rust() {
    assert_binds_same_on_both_sides(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/config.rs",
                "pub struct Config { pub timeout: u32 }\nfn read_local(c: &Config) -> u32 { c.timeout }\n",
            ),
            (
                "src/remote.rs",
                "fn read_remote(c: &Config) -> u32 { c.timeout + c.missing }\n",
            ),
        ],
        ("read_local", "read_remote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_csharp() {
    assert_binds_same_on_both_sides(
        &CSharpProvider::new().unwrap(),
        &[
            (
                "src/Config.cs",
                "class Config { public int Timeout; }\nclass LocalReader { int ReadLocal(Config c) { return c.Timeout; } }\n",
            ),
            (
                "src/Remote.cs",
                "class RemoteReader { int ReadRemote(Config c) { return c.Timeout + c.Missing; } }\n",
            ),
        ],
        ("ReadLocal", "ReadRemote", "Timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_javascript() {
    assert_binds_same_on_both_sides(
        &JavaScriptProvider::new().unwrap(),
        &[
            (
                "src/config.js",
                "class Config { timeout = 0; }\nfunction readLocal(c) { return c.timeout; }\n",
            ),
            (
                "src/remote.js",
                "function readRemote(c) { return c.timeout + c.missing; }\n",
            ),
        ],
        ("readLocal", "readRemote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_kotlin() {
    assert_binds_same_on_both_sides(
        &KotlinProvider::new().unwrap(),
        &[
            (
                "src/Config.kt",
                "class Config { val timeout: Int = 0 }\nfun readLocal(c: Config): Int { return c.timeout }\n",
            ),
            (
                "src/Remote.kt",
                "fun readRemote(c: Config): Int { return c.timeout + c.missing }\n",
            ),
        ],
        ("readLocal", "readRemote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_php() {
    assert_binds_same_on_both_sides(
        &PhpProvider::new().unwrap(),
        &[
            (
                "src/Config.php",
                "<?php\nclass Config { public int $timeout = 0; }\nfunction readLocal(Config $c): int { return $c->timeout; }\n",
            ),
            (
                "src/Remote.php",
                "<?php\nfunction readRemote(Config $c): int { return $c->timeout + $c->missing; }\n",
            ),
        ],
        ("readLocal", "readRemote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_swift() {
    assert_binds_same_on_both_sides(
        &SwiftProvider::new().unwrap(),
        &[
            (
                "src/Config.swift",
                "class Config { var timeout: Int = 0 }\nfunc readLocal(c: Config) -> Int { return c.timeout }\n",
            ),
            (
                "src/Remote.swift",
                "func readRemote(c: Config) -> Int { return c.timeout + c.missing }\n",
            ),
        ],
        ("readLocal", "readRemote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_c() {
    assert_binds_same_on_both_sides(
        &CProvider::new().unwrap(),
        &[
            (
                "src/config.c",
                "struct Config { int timeout; };\nint read_local(struct Config *c) { return c->timeout; }\n",
            ),
            (
                "src/remote.c",
                "int read_remote(struct Config *c) { return c->timeout + c->missing; }\n",
            ),
        ],
        ("read_local", "read_remote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_cpp() {
    assert_binds_same_on_both_sides(
        &CppProvider::new().unwrap(),
        &[
            (
                "src/config.cpp",
                "struct Config { int timeout; };\nint read_local(Config* c) { return c->timeout; }\n",
            ),
            (
                "src/remote.cpp",
                "int read_remote(Config* c) { return c->timeout + c->missing; }\n",
            ),
        ],
        ("read_local", "read_remote", "timeout"),
    );
}

#[test]
fn test_ladder_field_read_binds_same_on_both_sides_dart() {
    assert_binds_same_on_both_sides(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib/config.dart",
                "class Config { int timeout = 0; }\nint readLocal(Config c) { return c.timeout; }\n",
            ),
            (
                "lib/remote.dart",
                "int readRemote(Config c) { return c.timeout + c.missing; }\n",
            ),
        ],
        ("readLocal", "readRemote", "timeout"),
    );
}
