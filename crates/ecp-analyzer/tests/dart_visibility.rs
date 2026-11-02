//! Per-symbol underscore visibility checks for the Dart provider.
//!
//! In Dart, identifiers starting with `_` are file-private regardless of
//! `library` directive. The provider must reflect this on every emitted
//! RawNode via `is_exported = !name.starts_with('_')`.
//!
//! Covers Wave 3 / Matrix A1 (Dart row) from
//! `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.

mod instantiation_calls_support;

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{calls_from, graph_of};
use std::path::Path;

/// Stands for: an unfiltered import keeps its source as the imported name,
/// and a `show` / `hide` import keeps its combinators in source order, which
/// `reexport::dart_visible` reads to decide what the import binds.
#[test]
fn test_parse_file_import_visibility_preserves_alias_and_filtered_dependencies() {
    let provider = DartProvider::new().unwrap();
    for (suffix, alias, imported_name) in [
        ("", None, "entity/foo.dart"),
        (" as entity", Some("entity"), "entity/foo.dart"),
        (" deferred as entity", Some("entity"), "entity/foo.dart"),
        (" hide Foo", None, "hide Foo"),
        (" show Other", None, "show Other"),
        (" show Foo, Bar", None, "show Foo,Bar"),
        (" show\n  Foo,\n  Bar", None, "show Foo,Bar"),
        (" hide Foo, Bar", None, "hide Foo,Bar"),
        (" show A hide A", None, "show A;hide A"),
        (" as entity show Foo", Some("entity"), "show Foo"),
        (
            " as entity show Foo hide Other",
            Some("entity"),
            "show Foo;hide Other",
        ),
        (" deferred as entity hide Foo", Some("entity"), "hide Foo"),
    ] {
        let source = format!("import 'entity/foo.dart'{suffix};");
        let graph = provider
            .parse_file(Path::new("lib/config.dart"), source.as_bytes())
            .unwrap();
        assert_eq!(graph.imports.len(), 1, "{source}");
        let import = &graph.imports[0];
        assert_eq!(import.source, "entity/foo.dart", "{source}");
        assert_eq!(import.alias.as_deref(), alias, "{source}");
        assert_eq!(import.imported_name, imported_name, "{source}");
    }
}

/// Stands for: every import statement is its own row, so one file imported
/// twice with different combinators keeps both.
#[test]
fn test_parse_file_import_same_file_twice_keeps_each_combinator_row() {
    let provider = DartProvider::new().unwrap();
    let source = "import 'a.dart' show A;\nimport 'a.dart' hide A;\nimport 'a.dart';\n";
    let graph = provider
        .parse_file(Path::new("lib/config.dart"), source.as_bytes())
        .unwrap();
    let names: Vec<&str> = graph
        .imports
        .iter()
        .map(|import| import.imported_name.as_str())
        .collect();
    assert_eq!(names, ["show A", "hide A", "a.dart"]);
}

fn parse(src: &str) -> Vec<RawNode> {
    let provider = DartProvider::new().expect("DartProvider init");
    let graph = provider
        .parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn public_function_is_exported() {
    let nodes = parse("void foo() {}");
    let foo = find(&nodes, "foo", NodeKind::Function);
    assert!(foo.is_exported, "`foo` must be exported");
}

#[test]
fn private_function_is_not_exported() {
    let nodes = parse("void _bar() {}");
    let bar = find(&nodes, "_bar", NodeKind::Function);
    assert!(!bar.is_exported, "`_bar` must be private");
}

#[test]
fn public_class_with_private_method() {
    let src = "class C { void pub() {} void _priv() {} }";
    let nodes = parse(src);

    let class_c = find(&nodes, "C", NodeKind::Class);
    assert!(class_c.is_exported, "class `C` must be exported");

    let pub_m = find(&nodes, "pub", NodeKind::Method);
    assert!(pub_m.is_exported, "method `pub` must be exported");

    let priv_m = find(&nodes, "_priv", NodeKind::Method);
    assert!(!priv_m.is_exported, "method `_priv` must be private");
}

#[test]
fn private_class_is_not_exported() {
    let nodes = parse("class _Hidden {}");
    let hidden = find(&nodes, "_Hidden", NodeKind::Class);
    assert!(!hidden.is_exported, "class `_Hidden` must be private");
}

#[test]
fn property_visibility_in_class() {
    // tree-sitter-dart represents class-body `final p = 1;` as
    // `declaration > initialized_identifier_list > initialized_identifier`,
    // which is what `queries.scm` matches for `@property`.
    let src = "class C { final p = 1; final _p = 2; }";
    let nodes = parse(src);

    let p = find(&nodes, "p", NodeKind::Property);
    assert!(p.is_exported, "property `p` must be exported");

    let _p = find(&nodes, "_p", NodeKind::Property);
    assert!(!_p.is_exported, "property `_p` must be private");
}

#[test]
fn private_enum_is_not_exported() {
    // Enums emit NodeKind::Enum (corrected from the prior Interface mis-mapping).
    let src = "enum _Color { red, blue }";
    let nodes = parse(src);
    let e = find(&nodes, "_Color", NodeKind::Enum);
    assert!(!e.is_exported, "enum `_Color` must be private");
}

const FOO_FILE: &str = "lib/entity/foo.dart";

/// The files of the `name` calls that `run` makes in a project where two
/// files declare the type `Foo`, so only import evidence can pick one.
fn member_call_files(import: &str, name: &str) -> Vec<String> {
    let source = format!("{import} Foo make() => throw 0; void run() {{ make().bar(); }}");
    let graph = graph_of(
        &DartProvider::new().unwrap(),
        &[
            ("lib/config.dart", &source),
            (FOO_FILE, "class Foo { void bar() {} }"),
            ("lib/other/foo.dart", "class Foo { void bar() {} }"),
        ],
    );
    calls_from(&graph, "run")
        .into_iter()
        .filter(|hit| hit.name == name)
        .map(|hit| hit.file)
        .collect()
}

/// Stands for: a combinator import binds only the names it makes visible,
/// so `show Foo` and `hide Other` bind the type `Foo` and `show Other`,
/// `hide Foo` and `show Foo hide Foo` do not.
#[test]
fn test_resolve_call_dart_combinator_import_binds_only_visible_return_type() {
    for (import, binds) in [
        ("import 'entity/foo.dart' show Foo;", true),
        ("import 'entity/foo.dart' show Other, Foo;", true),
        ("import 'entity/foo.dart' hide Other;", true),
        ("import 'entity/foo.dart' show Foo hide Other;", true),
        (
            "import 'entity/foo.dart' show Foo; import 'entity/foo.dart' hide Foo;",
            true,
        ),
        (
            "import 'entity/foo.dart' show Other; import 'entity/foo.dart' hide Other;",
            true,
        ),
        ("import 'entity/foo.dart' show Other;", false),
        ("import 'entity/foo.dart' hide Foo;", false),
        ("import 'entity/foo.dart' show Foo hide Foo;", false),
        ("import 'entity/foo.dart' hide Foo, Other;", false),
        ("import 'entity/foo.dart' as entity show Foo;", false),
        ("import 'package:external/foo.dart' show Foo;", false),
    ] {
        let expected: Vec<String> = if binds { vec![FOO_FILE.into()] } else { vec![] };
        assert_eq!(member_call_files(import, "bar"), expected, "{import}");
    }
}

/// The files of the `helper` calls that `run` makes, when `lib/entity/foo.dart`
/// is the only file that declares `helper`.
fn helper_call_files(import: &str) -> Vec<String> {
    let source = format!("{import} void run() {{ helper(); }}");
    let graph = graph_of(
        &DartProvider::new().unwrap(),
        &[
            ("lib/config.dart", &source),
            (FOO_FILE, "class Foo {} void helper() {} void other() {}"),
        ],
    );
    calls_from(&graph, "run")
        .into_iter()
        .filter(|hit| hit.name == "helper")
        .map(|hit| hit.file)
        .collect()
}

/// Stands for: a function the importer's `show` / `hide` import hides is no
/// fallback target of a bare call, while a visible one, a plain import and no
/// import at all keep the global pick.
#[test]
fn test_resolve_call_dart_combinator_import_hides_fallback_binding_of_hidden_function() {
    for (import, binds) in [
        ("", true),
        ("import 'entity/foo.dart';", true),
        ("import 'entity/foo.dart' show helper;", true),
        ("import 'entity/foo.dart' show Foo, helper;", true),
        ("import 'entity/foo.dart' hide Foo;", true),
        (
            "import 'entity/foo.dart' show Foo; import 'entity/foo.dart' show helper;",
            true,
        ),
        ("import 'entity/foo.dart' show Foo;", false),
        ("import 'entity/foo.dart' hide helper;", false),
        ("import 'entity/foo.dart' show helper hide helper;", false),
        (
            "import 'entity/foo.dart' show Foo; import 'entity/foo.dart' hide helper;",
            false,
        ),
    ] {
        let expected: Vec<String> = if binds { vec![FOO_FILE.into()] } else { vec![] };
        assert_eq!(helper_call_files(import), expected, "{import}");
    }
}

/// A combinator import of one file hides nothing the importer reads from
/// another: `helper` declared in a file the import does not name still binds.
#[test]
fn test_resolve_call_dart_combinator_import_of_other_file_keeps_fallback_binding() {
    let graph = graph_of(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib/config.dart",
                "import 'entity/foo.dart' show Foo; void run() { helper(); }",
            ),
            (FOO_FILE, "class Foo {}"),
            ("lib/util/helper.dart", "void helper() {}"),
        ],
    );
    let files: Vec<String> = calls_from(&graph, "run")
        .into_iter()
        .filter(|hit| hit.name == "helper")
        .map(|hit| hit.file)
        .collect();
    assert_eq!(files, ["lib/util/helper.dart"]);
}

/// Stands for: a Dart combinator filters only a library's top-level names.
/// `show Repo` hides the top-level `helper`, while `p.repo.save()` still
/// binds `Repo.save`: a member is reached through its type, which the
/// import shows.
#[test]
fn test_resolve_call_dart_show_import_keeps_member_call_binding() {
    let graph = graph_of(
        &DartProvider::new().unwrap(),
        &[
            (
                "lib/page.dart",
                "import 'repo.dart' show Repo; class Page { final Repo repo = Repo(); } \
                 void run(Page p) { p.repo.save(); helper(); }",
            ),
            (
                "lib/repo.dart",
                "class Repo { void save() {} } void helper() {}",
            ),
        ],
    );
    let calls: Vec<(String, String, String)> = calls_from(&graph, "run")
        .into_iter()
        .filter(|hit| hit.name == "save" || hit.name == "helper")
        .map(|hit| (hit.name, hit.owner, hit.file))
        .collect();
    assert_eq!(
        calls,
        [("save".into(), "Repo".into(), "lib/repo.dart".into())]
    );
}
