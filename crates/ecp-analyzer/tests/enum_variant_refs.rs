//! Enum-variant references across the mainstream languages that have enums:
//! `ecp impact --target <variant> --direction upstream` must reach every
//! function that constructs, reads or matches the variant. Each reference is
//! a `References` edge from the enclosing function to the `EnumVariant`,
//! reason `enum_variant:use` or `enum_variant:pattern`, at most one per
//! (function, variant, reason).
//!
//! The assertions name the exact edge set: an edge to another enum's
//! same-named variant, a missing pattern, or a duplicate is a regression of
//! the contract "who uses variant X", not a wording change.
//!
//! N/A: JavaScript has no enum declaration; Go has none either (`iota`
//! constants are `Const` nodes, no `EnumVariant`); Ruby has no enum syntax.

use ecp_analyzer::resolution::builder::{GraphBuilder, VARIANT_PATTERN_REASON, VARIANT_USE_REASON};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const USE: &str = VARIANT_USE_REASON;
const PATTERN: &str = VARIANT_PATTERN_REASON;

fn build(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (path, source) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .expect("parse_file"),
        );
    }
    builder.build()
}

/// `(source name, variant owner, variant name, reason)` of every edge into an
/// `EnumVariant` other than its enum's `Defines`, sorted.
fn variant_refs(graph: &ZeroCopyGraph) -> Vec<(String, String, String, String)> {
    let pool = &graph.string_pool;
    let mut refs: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type != RelType::Defines
                && graph.nodes[e.target as usize].kind == NodeKind::EnumVariant
        })
        .map(|e| {
            let source = &graph.nodes[e.source as usize];
            let target = &graph.nodes[e.target as usize];
            assert_eq!(e.rel_type, RelType::References, "variant edge rel");
            (
                source.name.resolve(pool).to_owned(),
                target.owner_class.resolve(pool).to_owned(),
                target.name.resolve(pool).to_owned(),
                e.reason.resolve(pool).to_owned(),
            )
        })
        .collect();
    refs.sort();
    refs
}

fn expect(rows: &[(&str, &str, &str, &str)]) -> Vec<(String, String, String, String)> {
    let mut rows: Vec<_> = rows
        .iter()
        .map(|(s, o, v, r)| (s.to_string(), o.to_string(), v.to_string(), r.to_string()))
        .collect();
    rows.sort();
    rows
}

fn assert_refs(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    want: &[(&str, &str, &str, &str)],
) {
    let graph = build(provider, files);
    assert_eq!(variant_refs(&graph), expect(want));
}

// ── Rust ────────────────────────────────────────────────────────────────────

const RUST_FORMS: &str = r#"pub enum E {
    A { x: u32 },
    B(u32),
    C,
}
pub enum F {
    A,
    D,
}
pub fn make() -> E {
    E::A { x: 1 }
}
pub fn tup() -> E {
    E::B(2)
}
pub fn unit() -> E {
    E::C
}
pub fn other() -> F {
    F::A
}
pub fn m(e: &E, g: &E) -> u32 {
    match e {
        E::A { x } => *x,
        E::B(y) => *y,
        E::C if matches!(g, E::C) => 0,
        E::C => 1,
    }
}
impl E {
    pub fn mk() -> Self {
        Self::C
    }
}
"#;

#[test]
fn test_variant_refs_rust_every_form_binds_its_own_enums_variant() {
    assert_refs(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[("lib.rs", RUST_FORMS)],
        &[
            ("make", "E", "A", USE),
            ("tup", "E", "B", USE),
            ("unit", "E", "C", USE),
            ("other", "F", "A", USE),
            ("m", "E", "A", PATTERN),
            ("m", "E", "B", PATTERN),
            ("m", "E", "C", PATTERN),
            // The guard's `matches!` is a macro token tree: a use, once.
            ("m", "E", "C", USE),
            ("mk", "E", "C", USE),
        ],
    );
}

#[test]
fn test_variant_refs_rust_same_file_reference_has_full_confidence() {
    let graph = build(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[("lib.rs", RUST_FORMS)],
    );
    let confidences: Vec<f32> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::References)
        .filter(|e| graph.nodes[e.target as usize].kind == NodeKind::EnumVariant)
        .map(|e| e.confidence)
        .collect();
    assert!(!confidences.is_empty());
    assert!(confidences.iter().all(|&c| c == 1.0), "{confidences:?}");
}

#[test]
fn test_variant_refs_rust_imported_enum_cross_file_binds_variant() {
    assert_refs(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[
            (
                "src/kinds.rs",
                "pub enum Kind {\n    Alpha,\n    Beta,\n}\n",
            ),
            (
                "src/pick.rs",
                "use crate::kinds::Kind;\npub fn pick() -> Kind {\n    Kind::Alpha\n}\n",
            ),
        ],
        &[("pick", "Kind", "Alpha", USE)],
    );
}

#[test]
fn test_variant_refs_rust_ambiguous_enum_name_emits_no_edge() {
    assert_refs(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[
            ("src/a.rs", "pub enum Mode {\n    On,\n    Off,\n}\n"),
            ("src/b.rs", "pub enum Mode {\n    On,\n    Off,\n}\n"),
            (
                "src/c.rs",
                "pub fn go() -> u8 {\n    let _ = Mode::On;\n    0\n}\n",
            ),
        ],
        &[],
    );
}

#[test]
fn test_variant_refs_rust_non_variant_path_emits_no_edge() {
    assert_refs(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[(
            "lib.rs",
            "pub enum E {\n    A,\n}\npub struct S;\nimpl S {\n    pub fn new() -> Self {\n        S\n    }\n}\npub fn f() -> S {\n    S::new()\n}\n",
        )],
        &[],
    );
}

// ── TypeScript ──────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_typescript_member_read_and_case_label() {
    assert_refs(
        &ecp_analyzer::typescript::parser::TypeScriptProvider::new().unwrap(),
        &[(
            "status.ts",
            "enum Status { Active, Idle, Done }\nfunction make(): Status { return Status.Active; }\nfunction check(s: Status): number { switch (s) { case Status.Idle: return 1; default: return 0; } }\n",
        )],
        &[("make", "Status", "Active", USE), ("check", "Status", "Idle", PATTERN)],
    );
}

// ── Java ────────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_java_qualified_read_and_bare_case_label() {
    assert_refs(
        &ecp_analyzer::java::parser::JavaProvider::new().unwrap(),
        &[(
            "Day.java",
            "enum Day { MONDAY, TUESDAY, FRIDAY }\nclass UseDay {\n  Day make() { return Day.MONDAY; }\n  int check(Day d) { switch (d) { case TUESDAY: return 1; default: return 0; } }\n}\n",
        )],
        &[("make", "Day", "MONDAY", USE), ("check", "Day", "TUESDAY", PATTERN)],
    );
}

// ── C# ──────────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_csharp_member_read_and_case_pattern() {
    assert_refs(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        &[(
            "Level.cs",
            "enum Level { Info, Warn, Err }\nclass UseLevel {\n  Level Make() { return Level.Info; }\n  int Check(Level l) { switch (l) { case Level.Warn: return 1; default: return 0; } }\n}\n",
        )],
        &[("Make", "Level", "Info", USE), ("Check", "Level", "Warn", PATTERN)],
    );
}

// ── Kotlin ──────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_kotlin_navigation_read_and_when_branch() {
    assert_refs(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        &[(
            "color.kt",
            "enum class Color { RED, GREEN }\nfun make(): Color { return Color.RED }\nfun check(c: Color): Int { return when (c) { Color.GREEN -> 1; else -> 0 } }\n",
        )],
        &[("make", "Color", "RED", USE), ("check", "Color", "GREEN", PATTERN)],
    );
}

// ── Swift ───────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_swift_qualified_case_read_and_switch_pattern() {
    assert_refs(
        &ecp_analyzer::swift::parser::SwiftProvider::new().unwrap(),
        &[(
            "suit.swift",
            "enum Suit { case clubs, hearts }\nfunc make() -> Suit { return Suit.clubs }\nfunc check(s: Suit) -> Int { switch s { case Suit.hearts: return 1; default: return 0 } }\n",
        )],
        &[("make", "Suit", "clubs", USE), ("check", "Suit", "hearts", PATTERN)],
    );
}

// ── Dart ────────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_dart_member_read_and_constant_pattern() {
    assert_refs(
        &ecp_analyzer::dart::parser::DartProvider::new().unwrap(),
        &[(
            "phase.dart",
            "enum Phase { init, run }\nPhase make() { return Phase.init; }\nint check(Phase p) { switch (p) { case Phase.run: return 1; default: return 0; } }\n",
        )],
        &[("make", "Phase", "init", USE), ("check", "Phase", "run", PATTERN)],
    );
}

// ── PHP ─────────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_php_case_constant_read_and_match_arm() {
    assert_refs(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        &[(
            "status.php",
            "<?php\nenum PStatus { case Active; case Idle; }\nfunction pmake(): PStatus { return PStatus::Active; }\nfunction pcheck(PStatus $s): int { return match($s) { PStatus::Idle => 1, default => 0 }; }\n",
        )],
        &[("pmake", "PStatus", "Active", USE), ("pcheck", "PStatus", "Idle", PATTERN)],
    );
}

// ── Python ──────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_python_attribute_read_and_match_case() {
    assert_refs(
        &ecp_analyzer::python::parser::PythonProvider::new().unwrap(),
        &[(
            "colors.py",
            "from enum import Enum\nclass PyColor(Enum):\n    RED = 1\n    GREEN = 2\ndef pymake():\n    return PyColor.RED\ndef pycheck(c):\n    match c:\n        case PyColor.GREEN:\n            return 1\n    return 0\n",
        )],
        &[("pymake", "PyColor", "RED", USE), ("pycheck", "PyColor", "GREEN", PATTERN)],
    );
}

// ── C ───────────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_c_bare_enumerator_read_and_case_label() {
    assert_refs(
        &ecp_analyzer::c::parser::CProvider::new().unwrap(),
        &[(
            "color.c",
            "enum ccolor { CRED, CGREEN };\nenum ccolor cmake(void) { return CRED; }\nint ccheck(enum ccolor c) { switch (c) { case CGREEN: return 1; default: return 0; } }\n",
        )],
        &[("cmake", "ccolor", "CRED", USE), ("ccheck", "ccolor", "CGREEN", PATTERN)],
    );
}

#[test]
fn test_variant_refs_c_enumerator_in_two_enums_emits_no_edge() {
    assert_refs(
        &ecp_analyzer::c::parser::CProvider::new().unwrap(),
        &[
            ("a.c", "enum a { RED };\n"),
            ("b.c", "enum b { RED };\n"),
            ("c.c", "int f(void) { return RED; }\n"),
        ],
        &[],
    );
}

#[test]
fn test_variant_refs_c_same_file_macro_shadows_enumerator_emits_no_edge() {
    assert_refs(
        &ecp_analyzer::c::parser::CProvider::new().unwrap(),
        &[
            ("color.c", "enum color { BLUE };\n"),
            ("shade.c", "#define BLUE 3\nint f(void) { return BLUE; }\n"),
        ],
        &[],
    );
}

#[test]
fn test_variant_refs_c_macro_in_other_file_sharing_name_emits_no_edge() {
    assert_refs(
        &ecp_analyzer::c::parser::CProvider::new().unwrap(),
        &[
            ("defs.c", "#define BLUE 3\n"),
            ("color.c", "enum color { BLUE };\n"),
            ("shade.c", "int f(void) { return BLUE; }\n"),
        ],
        &[],
    );
}

#[test]
fn test_variant_refs_c_enumerator_in_other_file_binds_global_variant() {
    let graph = build(
        &ecp_analyzer::c::parser::CProvider::new().unwrap(),
        &[
            ("color.c", "enum color { BLUE };\n"),
            ("shade.c", "int f(void) { return BLUE; }\n"),
        ],
    );
    assert_eq!(variant_refs(&graph), expect(&[("f", "color", "BLUE", USE)]));
    let edge = graph
        .edges
        .iter()
        .find(|e| e.rel_type == RelType::References)
        .expect("variant edge");
    assert_eq!(
        edge.confidence, 0.7,
        "a cross-file bare name is a Global-tier pick"
    );
}

// ── C++ ─────────────────────────────────────────────────────────────────────

#[test]
fn test_variant_refs_cpp_scoped_and_unscoped_enumerators() {
    assert_refs(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        &[(
            "color.cpp",
            "enum class CppColor { Red, Green };\nenum Shade { DARK, LIGHT };\nCppColor cppmake() { return CppColor::Red; }\nint cppcheck(CppColor c) { switch (c) { case CppColor::Green: return 1; default: return 0; } }\nint shade() { return DARK; }\n",
        )],
        &[
            ("cppmake", "CppColor", "Red", USE),
            ("cppcheck", "CppColor", "Green", PATTERN),
            ("shade", "Shade", "DARK", USE),
        ],
    );
}
