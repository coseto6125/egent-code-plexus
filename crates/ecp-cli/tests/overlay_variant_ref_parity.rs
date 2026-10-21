//! Enum-variant references out of a dirty file: the session overlay emits the
//! same `References` edges (reason `enum_variant:use` / `enum_variant:pattern`)
//! as a full reindex of the edited tree, and masks the base edges a dirty
//! file no longer makes.
//!
//! Each language fixture keeps the enum in one file and its uses in another,
//! then runs six edits:
//!
//! | edit           | dirty files      | the reindex has                       |
//! |----------------|------------------|---------------------------------------|
//! | shift          | user             | every use, one line lower             |
//! | both dirty     | user and enum    | every use, both files moved           |
//! | add use        | user             | every use; the base had none          |
//! | remove use     | user             | none; the stale base edges must go    |
//! | empty          | user             | none                                  |
//! | enum deleted   | enum             | none from the clean user file         |
//!
//! N/A, because the index emits no variant reference: JavaScript has no
//! enum declaration; Go has none (`iota` constants are `Const` nodes); Ruby
//! has no enum syntax.

#[path = "common/parity.rs"]
mod parity;

use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::RelType;
use parity::{edges_after_edit, EdgeParity, ReasonedEdge};
use std::path::Path;

const USE: &str = "enum_variant:use";
const PATTERN: &str = "enum_variant:pattern";

struct LangFixture {
    lang: &'static str,
    /// The file that declares the enum.
    enum_file: (&'static str, &'static str),
    /// The user file's path, its source with the variant uses, and the same
    /// functions with no variant use.
    user: (&'static str, &'static str, &'static str),
    /// `(source function, variant, reason)` the reindex emits from `user`.
    rows: &'static [(&'static str, &'static str, &'static str)],
}

const LANGS: &[LangFixture] = &[
    LangFixture {
        lang: "Rust",
        enum_file: ("src/kinds.rs", "pub enum Kind {\n    Alpha,\n    Beta,\n}\n"),
        user: (
            "src/pick.rs",
            "use crate::kinds::Kind;\npub fn pick() -> Kind {\n    Kind::Alpha\n}\npub fn check(k: &Kind) -> u8 {\n    match k {\n        Kind::Beta => 1,\n        _ => 0,\n    }\n}\n",
            "use crate::kinds::Kind;\npub fn pick() -> u8 {\n    0\n}\npub fn check(k: &Kind) -> u8 {\n    0\n}\n",
        ),
        rows: &[("check", "Beta", PATTERN), ("pick", "Alpha", USE)],
    },
    LangFixture {
        lang: "TypeScript",
        enum_file: ("status.ts", "export enum Status { Active, Idle, Done }\n"),
        user: (
            "use.ts",
            "import { Status } from './status';\nexport function make(): Status { return Status.Active; }\nexport function check(s: Status): number { switch (s) { case Status.Idle: return 1; default: return 0; } }\n",
            "import { Status } from './status';\nexport function make(): number { return 0; }\nexport function check(s: Status): number { return 0; }\n",
        ),
        rows: &[("check", "Idle", PATTERN), ("make", "Active", USE)],
    },
    LangFixture {
        lang: "Java",
        enum_file: ("src/p/Day.java", "package p;\nenum Day { MONDAY, TUESDAY, FRIDAY }\n"),
        user: (
            "src/p/UseDay.java",
            "package p;\nclass UseDay {\n  Day make() { return Day.MONDAY; }\n  int check(Day d) { switch (d) { case TUESDAY: return 1; default: return 0; } }\n}\n",
            "package p;\nclass UseDay {\n  Day make() { return null; }\n  int check(Day d) { return 0; }\n}\n",
        ),
        rows: &[("check", "TUESDAY", PATTERN), ("make", "MONDAY", USE)],
    },
    LangFixture {
        lang: "C#",
        enum_file: ("Level.cs", "enum Level { Info, Warn, Err }\n"),
        user: (
            "UseLevel.cs",
            "class UseLevel {\n  Level Make() { return Level.Info; }\n  int Check(Level l) { switch (l) { case Level.Warn: return 1; default: return 0; } }\n}\n",
            "class UseLevel {\n  Level Make() { return default; }\n  int Check(Level l) { return 0; }\n}\n",
        ),
        rows: &[("Check", "Warn", PATTERN), ("Make", "Info", USE)],
    },
    LangFixture {
        lang: "Kotlin",
        enum_file: ("src/p/Color.kt", "package p\nenum class Color { RED, GREEN }\n"),
        user: (
            "src/p/Use.kt",
            "package p\nfun make(): Color { return Color.RED }\nfun check(c: Color): Int { return when (c) { Color.GREEN -> 1; else -> 0 } }\n",
            "package p\nfun make(): Int { return 0 }\nfun check(c: Color): Int { return 0 }\n",
        ),
        rows: &[("check", "GREEN", PATTERN), ("make", "RED", USE)],
    },
    LangFixture {
        lang: "Swift",
        enum_file: ("Suit.swift", "enum Suit { case clubs, hearts }\n"),
        user: (
            "Use.swift",
            "func make() -> Suit { return Suit.clubs }\nfunc check(s: Suit) -> Int { switch s { case Suit.hearts: return 1; default: return 0 } }\n",
            "func make() -> Int { return 0 }\nfunc check(s: Suit) -> Int { return 0 }\n",
        ),
        rows: &[("check", "hearts", PATTERN), ("make", "clubs", USE)],
    },
    LangFixture {
        lang: "Dart",
        enum_file: ("lib/phase.dart", "enum Phase { init, run }\n"),
        user: (
            "lib/use.dart",
            "import 'phase.dart';\nPhase make() { return Phase.init; }\nint check(Phase p) { switch (p) { case Phase.run: return 1; default: return 0; } }\n",
            "import 'phase.dart';\nint make() { return 0; }\nint check(Phase p) { return 0; }\n",
        ),
        rows: &[("check", "run", PATTERN), ("make", "init", USE)],
    },
    LangFixture {
        lang: "PHP",
        enum_file: ("PStatus.php", "<?php\nenum PStatus { case Active; case Idle; }\n"),
        user: (
            "use.php",
            "<?php\nfunction pmake(): PStatus { return PStatus::Active; }\nfunction pcheck(PStatus $s): int { return match($s) { PStatus::Idle => 1, default => 0 }; }\n",
            "<?php\nfunction pmake(): int { return 0; }\nfunction pcheck(PStatus $s): int { return 0; }\n",
        ),
        rows: &[("pcheck", "Idle", PATTERN), ("pmake", "Active", USE)],
    },
    LangFixture {
        lang: "Python",
        enum_file: (
            "colors.py",
            "from enum import Enum\nclass PyColor(Enum):\n    RED = 1\n    GREEN = 2\n",
        ),
        user: (
            "use.py",
            "from colors import PyColor\ndef pymake():\n    return PyColor.RED\ndef pycheck(c):\n    match c:\n        case PyColor.GREEN:\n            return 1\n    return 0\n",
            "from colors import PyColor\ndef pymake():\n    return 0\ndef pycheck(c):\n    return 0\n",
        ),
        rows: &[("pycheck", "GREEN", PATTERN), ("pymake", "RED", USE)],
    },
    // A bare enumerator in another file is a global-tier pick (0.7).
    LangFixture {
        lang: "C",
        enum_file: ("color.c", "enum ccolor { CRED, CGREEN };\n"),
        user: (
            "use.c",
            "int cmake(void) { return CRED; }\nint ccheck(int c) { switch (c) { case CGREEN: return 1; default: return 0; } }\n",
            "int cmake(void) { return 0; }\nint ccheck(int c) { return 0; }\n",
        ),
        rows: &[("ccheck", "CGREEN", PATTERN), ("cmake", "CRED", USE)],
    },
    LangFixture {
        lang: "C++",
        enum_file: (
            "color.hpp",
            "enum class CppColor { Red, Green };\nenum Shade { DARK, LIGHT };\n",
        ),
        user: (
            "use.cpp",
            "#include \"color.hpp\"\nCppColor cppmake() { return CppColor::Red; }\nint cppcheck(CppColor c) { switch (c) { case CppColor::Green: return 1; default: return 0; } }\nint shade() { return DARK; }\n",
            "#include \"color.hpp\"\nint cppmake() { return 0; }\nint cppcheck(CppColor c) { return 0; }\nint shade() { return 0; }\n",
        ),
        rows: &[
            ("cppcheck", "Green", PATTERN),
            ("cppmake", "Red", USE),
            ("shade", "DARK", USE),
        ],
    },
];

/// One parity run: index `files`, apply `edits`, and compare the variant
/// references out of `source_file`.
struct Case {
    label: String,
    files: Vec<(&'static str, String)>,
    edits: Vec<(&'static str, String)>,
    source_file: &'static str,
    /// `(source function, target file, variant, reason)` the reindex emits.
    expected: Vec<(&'static str, &'static str, &'static str, &'static str)>,
}

fn parse(path: &str, source: &str) -> LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `source` with a blank line after its first line: every declaration
/// below moves down one.
fn shifted(source: &str) -> String {
    match source.split_once('\n') {
        Some((first, rest)) => format!("{first}\n\n{rest}"),
        None => format!("\n{source}"),
    }
}

fn lang_cases(fixture: &LangFixture) -> Vec<Case> {
    let (enum_path, enum_src) = fixture.enum_file;
    let (user_path, user_src, user_bare) = fixture.user;
    let rows: Vec<_> = fixture
        .rows
        .iter()
        .map(|&(source, variant, reason)| (source, enum_path, variant, reason))
        .collect();
    let with_uses = vec![
        (enum_path, enum_src.to_string()),
        (user_path, user_src.to_string()),
    ];
    let case = |edit: &str,
                files: &[(&'static str, String)],
                edits: Vec<(&'static str, String)>,
                expected| Case {
        label: format!("{}/{edit}", fixture.lang),
        files: files.to_vec(),
        edits,
        source_file: user_path,
        expected,
    };
    vec![
        case(
            "shift",
            &with_uses,
            vec![(user_path, shifted(user_src))],
            rows.clone(),
        ),
        case(
            "both dirty",
            &with_uses,
            vec![
                (enum_path, shifted(enum_src)),
                (user_path, shifted(user_src)),
            ],
            rows.clone(),
        ),
        case(
            "add use",
            &[
                (enum_path, enum_src.to_string()),
                (user_path, user_bare.to_string()),
            ],
            vec![(user_path, user_src.to_string())],
            rows,
        ),
        case(
            "remove use",
            &with_uses,
            vec![(user_path, user_bare.to_string())],
            Vec::new(),
        ),
        case(
            "empty",
            &with_uses,
            vec![(user_path, String::new())],
            Vec::new(),
        ),
        case(
            "enum deleted",
            &with_uses,
            vec![(enum_path, String::new())],
            Vec::new(),
        ),
    ]
}

/// Two enums share the name `Mode`: a bare `Mode::On` is ambiguous and binds
/// nothing, and an import picks its own file's variant.
fn same_named_enum_cases() -> Vec<Case> {
    let rust_files = vec![
        (
            "src/a.rs",
            "pub enum Mode {\n    On,\n    Off,\n}\n".to_string(),
        ),
        (
            "src/b.rs",
            "pub enum Mode {\n    On,\n    Off,\n}\n".to_string(),
        ),
        ("src/c.rs", "pub fn go() -> u8 {\n    0\n}\n".to_string()),
    ];
    let ts_files = vec![
        ("a.ts", "export enum Mode { On, Off }\n".to_string()),
        ("b.ts", "export enum Mode { On, Off }\n".to_string()),
        (
            "c.ts",
            "export function go(): number { return 0; }\n".to_string(),
        ),
    ];
    vec![
        Case {
            label: "Rust/same-named enum, no import".into(),
            files: rust_files.clone(),
            edits: vec![(
                "src/c.rs",
                "pub fn go() -> u8 {\n    let _ = Mode::On;\n    0\n}\n".into(),
            )],
            source_file: "src/c.rs",
            expected: Vec::new(),
        },
        Case {
            label: "Rust/same-named enum, imported".into(),
            files: rust_files,
            edits: vec![(
                "src/c.rs",
                "use crate::a::Mode;\npub fn go() -> u8 {\n    let _ = Mode::On;\n    0\n}\n"
                    .into(),
            )],
            source_file: "src/c.rs",
            expected: vec![("go", "src/a.rs", "On", USE)],
        },
        Case {
            label: "TypeScript/same-named enum, imported".into(),
            files: ts_files,
            edits: vec![(
                "c.ts",
                "import { Mode } from './b';\nexport function go(): Mode { return Mode.On; }\n"
                    .into(),
            )],
            source_file: "c.ts",
            expected: vec![("go", "b.ts", "On", USE)],
        },
    ]
}

/// Every reference form in one Rust file, `Self::C` and a `matches!` guard
/// included, edited in place.
fn rust_forms_case() -> Case {
    const RUST_FORMS: &str = "pub enum E {\n    A { x: u32 },\n    B(u32),\n    C,\n}\npub enum F {\n    A,\n    D,\n}\npub fn make() -> E {\n    E::A { x: 1 }\n}\npub fn tup() -> E {\n    E::B(2)\n}\npub fn unit() -> E {\n    E::C\n}\npub fn other() -> F {\n    F::A\n}\npub fn m(e: &E, g: &E) -> u32 {\n    match e {\n        E::A { x } => *x,\n        E::B(y) => *y,\n        E::C if matches!(g, E::C) => 0,\n        E::C => 1,\n    }\n}\nimpl E {\n    pub fn mk() -> Self {\n        Self::C\n    }\n}\n";
    Case {
        label: "Rust/every form, same file".into(),
        files: vec![("src/lib.rs", RUST_FORMS.to_string())],
        edits: vec![("src/lib.rs", shifted(RUST_FORMS))],
        source_file: "src/lib.rs",
        expected: vec![
            ("m", "src/lib.rs", "A", PATTERN),
            ("m", "src/lib.rs", "B", PATTERN),
            ("m", "src/lib.rs", "C", PATTERN),
            ("m", "src/lib.rs", "C", USE),
            ("make", "src/lib.rs", "A", USE),
            ("mk", "src/lib.rs", "C", USE),
            ("other", "src/lib.rs", "A", USE),
            ("tup", "src/lib.rs", "B", USE),
            ("unit", "src/lib.rs", "C", USE),
        ],
    }
}

fn variant_refs(edges: Vec<ReasonedEdge>) -> Vec<ReasonedEdge> {
    edges
        .into_iter()
        .filter(|e| e.reason == USE || e.reason == PATTERN)
        .collect()
}

/// Every mismatch of `cases`, one line each: a reindex that differs from
/// the expected rows (the fixture would prove nothing), or an overlay that
/// differs from the reindex in any field, reason and confidence included.
fn mismatches(cases: &[Case]) -> Vec<String> {
    let mut failures = Vec::new();
    for case in cases {
        let files: Vec<(&str, &str)> = case.files.iter().map(|(p, s)| (*p, s.as_str())).collect();
        let edits: Vec<(&str, &str)> = case.edits.iter().map(|(p, s)| (*p, s.as_str())).collect();
        let EdgeParity { reindex, overlay } =
            edges_after_edit(&parse, &files, &edits, RelType::References, &|file, _| {
                file == case.source_file
            });
        let (reindex, overlay) = (variant_refs(reindex), variant_refs(overlay));
        let mut rows: Vec<_> = reindex
            .iter()
            .map(|e| {
                (
                    e.edge.caller.as_str(),
                    e.edge.callee_file.as_str(),
                    e.edge.callee.as_str(),
                    e.reason.as_str(),
                )
            })
            .collect();
        rows.sort();
        let mut expected = case.expected.clone();
        expected.sort();
        if rows != expected {
            failures.push(format!(
                "{}: the reindex emits {rows:?}, the fixture expects {expected:?}",
                case.label
            ));
            continue;
        }
        if overlay != reindex {
            failures.push(format!(
                "{}: overlay {overlay:#?}\nreindex {reindex:#?}",
                case.label
            ));
        }
    }
    failures
}

#[test]
fn test_overlay_variant_refs_match_reindex_across_enum_languages() {
    let cases: Vec<Case> = LANGS.iter().flat_map(lang_cases).collect();
    let failures = mismatches(&cases);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn test_overlay_variant_refs_same_named_enum_in_other_file_matches_reindex() {
    let failures = mismatches(&same_named_enum_cases());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn test_overlay_variant_refs_rust_every_form_matches_reindex() {
    let failures = mismatches(&[rust_forms_case()]);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
