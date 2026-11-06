//! Type-annotation edges out of a dirty file (FU-2026-10-26-0ee756874fb8):
//! the session overlay emits the same `Accesses` edges (reason
//! `type_annotation`) as a full reindex of the edited tree, reason and
//! confidence included, and masks the base edges a dirty file no longer
//! makes.
//!
//! Each language fixture declares `St` and `Other` beside three functions,
//! and one edit changes all three:
//!
//! | function | base               | edit                 | the reindex has |
//! |----------|--------------------|----------------------|-----------------|
//! | `take`   | a primitive param  | a `St` param         | `take -> St`    |
//! | `ret`    | returns `St`       | returns nothing      | none            |
//! | `gen`    | a generic of `St`  | the generic of `Other` | `gen -> Other` |
//!
//! `ret` and `gen` survive the edit, so a base edge the overlay does not
//! mask shows as an extra row. Without generics, C changes a pointee
//! (`struct St *` to `struct Other *`) and PHP a nullable (`?St` to
//! `?Other`); Swift keeps no return annotation of its own, so its `gen`
//! takes the array in a parameter. JavaScript and Ruby annotate nothing:
//! both sides must emit no edge.
//!
//! The last test reads fragments the release binary 0.17.3 wrote
//! (`fixtures/overlay-written-by-0.17.3/`, see its README for what the
//! sample cannot show).

use crate::parity;

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_cli::session::overlay_writer::write_dirty_fragments_from_graphs;
use ecp_core::analyzer::type_ladder::{TYPE_ANNOTATION_REASON, TYPE_ARGUMENT_REASON};
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::OverlayView;
use ecp_core::session::{layout, DirtyEvents, SessionMeta};
use parity::{edges_after_edit, parse, EdgeParity, ReasonedEdge};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// `(source, target file, target)` of one `type_annotation` edge.
type Row = (&'static str, &'static str, &'static str);

struct LangFixture {
    lang: &'static str,
    path: &'static str,
    base: &'static str,
    edited: &'static str,
}

/// The reindex rows of every fixture whose language annotates.
fn take_and_gen(path: &'static str) -> Vec<Row> {
    vec![("gen", path, "Other"), ("take", path, "St")]
}

const LANGS: &[LangFixture] = &[
    LangFixture {
        lang: "Rust",
        path: "src/app.rs",
        base: "pub struct St;\npub struct Other;\npub fn take(a: u32) {}\npub fn ret() -> St {\n    St\n}\npub fn gen() -> Option<St> {\n    None\n}\n",
        edited: "pub struct St;\npub struct Other;\npub fn take(a: St) {}\npub fn ret() {}\npub fn gen() -> Option<Other> {\n    None\n}\n",
    },
    LangFixture {
        lang: "TypeScript",
        path: "app.ts",
        base: "export class St {}\nexport class Other {}\nexport function take(a: number): void {}\nexport function ret(): St { return new St(); }\nexport function gen(): Promise<St> { return Promise.resolve(new St()); }\n",
        edited: "export class St {}\nexport class Other {}\nexport function take(a: St): void {}\nexport function ret() { return 1; }\nexport function gen(): Promise<Other> { return Promise.resolve(new Other()); }\n",
    },
    LangFixture {
        lang: "JavaScript",
        path: "app.js",
        base: "export class St {}\nexport class Other {}\nexport function take(a) {}\nexport function ret() { return new St(); }\nexport function gen() { return [new St()]; }\n",
        edited: "export class St {}\nexport class Other {}\nexport function take(a, b) {}\nexport function ret() { return 1; }\nexport function gen() { return [new Other()]; }\n",
    },
    LangFixture {
        lang: "Python",
        path: "app.py",
        base: "from typing import Optional\nclass St: pass\nclass Other: pass\ndef take(a: int) -> None: ...\ndef ret() -> St: ...\ndef gen() -> Optional[St]: ...\n",
        edited: "from typing import Optional\nclass St: pass\nclass Other: pass\ndef take(a: St) -> None: ...\ndef ret(): ...\ndef gen() -> Optional[Other]: ...\n",
    },
    LangFixture {
        lang: "Java",
        path: "App.java",
        base: "import java.util.*;\nclass St {}\nclass Other {}\nclass App {\n    void take(int a) {}\n    St ret() { return null; }\n    List<St> gen() { return null; }\n}\n",
        edited: "import java.util.*;\nclass St {}\nclass Other {}\nclass App {\n    void take(St a) {}\n    void ret() {}\n    List<Other> gen() { return null; }\n}\n",
    },
    LangFixture {
        lang: "Kotlin",
        path: "app.kt",
        base: "class St\nclass Other\nfun take(a: Int) {}\nfun ret(): St = St()\nfun gen(): List<St> = listOf()\n",
        edited: "class St\nclass Other\nfun take(a: St) {}\nfun ret() {}\nfun gen(): List<Other> = listOf()\n",
    },
    LangFixture {
        lang: "C#",
        path: "App.cs",
        base: "using System.Collections.Generic;\nclass St { }\nclass Other { }\nclass App {\n    void take(int a) { }\n    St ret() { return null; }\n    List<St> gen() { return null; }\n}\n",
        edited: "using System.Collections.Generic;\nclass St { }\nclass Other { }\nclass App {\n    void take(St a) { }\n    void ret() { }\n    List<Other> gen() { return null; }\n}\n",
    },
    LangFixture {
        lang: "Go",
        path: "app.go",
        base: "package main\n\ntype St struct{}\ntype Other struct{}\n\nfunc take(a int) {}\nfunc ret() St { return St{} }\nfunc gen() []St { return nil }\n",
        edited: "package main\n\ntype St struct{}\ntype Other struct{}\n\nfunc take(a St) {}\nfunc ret() {}\nfunc gen() []Other { return nil }\n",
    },
    LangFixture {
        lang: "PHP",
        path: "app.php",
        base: "<?php\nclass St {}\nclass Other {}\nfunction take(int $a) {}\nfunction ret(): St { return new St(); }\nfunction gen(): ?St { return null; }\n",
        edited: "<?php\nclass St {}\nclass Other {}\nfunction take(St $a) {}\nfunction ret() {}\nfunction gen(): ?Other { return null; }\n",
    },
    LangFixture {
        lang: "Ruby",
        path: "app.rb",
        base: "class St; end\nclass Other; end\ndef take(a)\n  a\nend\ndef ret\n  St.new\nend\ndef gen\n  [St.new]\nend\n",
        edited: "class St; end\nclass Other; end\ndef take(a, b)\n  a\nend\ndef ret\n  1\nend\ndef gen\n  [Other.new]\nend\n",
    },
    LangFixture {
        lang: "Swift",
        path: "App.swift",
        base: "class St {}\nclass Other {}\nfunc take(a: Int) {}\nfunc ret() -> St { return St() }\nfunc gen(v: [St]) {}\n",
        edited: "class St {}\nclass Other {}\nfunc take(a: St) {}\nfunc ret() {}\nfunc gen(v: [Other]) {}\n",
    },
    LangFixture {
        lang: "C",
        path: "app.c",
        base: "struct St { int n; };\nstruct Other { int n; };\nvoid take(int a) {}\nstruct St *ret(void) { return 0; }\nstruct St *gen(void) { return 0; }\n",
        edited: "struct St { int n; };\nstruct Other { int n; };\nvoid take(struct St *a) {}\nvoid ret(void) {}\nstruct Other *gen(void) { return 0; }\n",
    },
    LangFixture {
        lang: "C++",
        path: "app.cpp",
        base: "#include <vector>\nstruct St { int n; };\nstruct Other { int n; };\nvoid take(int a) {}\nSt ret() { return St{}; }\nstd::vector<St> gen() { return {}; }\n",
        edited: "#include <vector>\nstruct St { int n; };\nstruct Other { int n; };\nvoid take(St a) {}\nvoid ret() {}\nstd::vector<Other> gen() { return {}; }\n",
    },
    LangFixture {
        lang: "Dart",
        path: "lib/app.dart",
        base: "class St {}\nclass Other {}\nvoid take(int a) {}\nSt ret() => St();\nList<St> gen() => [];\n",
        edited: "class St {}\nclass Other {}\nvoid take(St a) {}\nvoid ret() {}\nList<Other> gen() => [];\n",
    },
];

/// One parity run: index `files`, apply `edits`, and compare the
/// `type_annotation` edges out of `source_file`.
struct Case {
    label: String,
    files: Vec<(&'static str, &'static str)>,
    edits: Vec<(&'static str, &'static str)>,
    source_file: &'static str,
    expected: Vec<Row>,
}

fn lang_case(fixture: &LangFixture) -> Case {
    let annotates = !matches!(fixture.lang, "JavaScript" | "Ruby");
    Case {
        label: format!("{}/take+ret+gen", fixture.lang),
        files: vec![(fixture.path, fixture.base)],
        edits: vec![(fixture.path, fixture.edited)],
        source_file: fixture.path,
        expected: if annotates {
            take_and_gen(fixture.path)
        } else {
            Vec::new()
        },
    }
}

/// A field's or property's annotation (the text the fragment now carries)
/// and a declaring-type keyword, which binds the owner.
fn member_cases() -> Vec<Case> {
    let case = |label: &str, path, base, edited, expected| Case {
        label: label.to_string(),
        files: vec![(path, base)],
        edits: vec![(path, edited)],
        source_file: path,
        expected,
    };
    vec![
        case(
            "Swift/property type changed",
            "Sources/Owner.swift",
            "class St {}\nclass Other {}\nclass Owner {\n    var f: Other\n}\n",
            "class St {}\nclass Other {}\nclass Owner {\n    var f: St\n}\n",
            vec![("f", "Sources/Owner.swift", "St")],
        ),
        case(
            "Swift/property typed Self",
            "Sources/Resolver.swift",
            "class Resolver {\n    var twin: Int\n}\n",
            "class Resolver {\n    var twin: Self?\n}\n",
            vec![("twin", "Sources/Resolver.swift", "Resolver")],
        ),
        case(
            "C#/property type changed",
            "Owner.cs",
            "class St { }\nclass Other { }\nclass Owner {\n    public Other F { get; set; }\n}\n",
            "class St { }\nclass Other { }\nclass Owner {\n    public St F { get; set; }\n}\n",
            vec![("F", "Owner.cs", "St")],
        ),
        case(
            "Rust/return Self added",
            "src/owner.rs",
            "pub struct Owner;\nimpl Owner {\n    pub fn make() {}\n}\n",
            "pub struct Owner;\nimpl Owner {\n    pub fn make() -> Self {\n        Owner\n    }\n}\n",
            vec![("make", "src/owner.rs", "Owner")],
        ),
        case(
            "Rust/Self moved inside a generic",
            "src/owner.rs",
            "pub struct Owner;\nimpl Owner {\n    pub fn make() -> Self {\n        Owner\n    }\n}\n",
            "pub struct Owner;\nimpl Owner {\n    pub fn make() -> Option<Self> {\n        None\n    }\n}\n",
            Vec::new(),
        ),
        case(
            "PHP/return static added",
            "Svc.php",
            "<?php\nclass Svc {\n    public function make() { return $this; }\n}\n",
            "<?php\nclass Svc {\n    public function make(): static { return $this; }\n}\n",
            vec![("make", "Svc.php", "Svc")],
        ),
    ]
}

/// The type sits in a clean file and the dirty file names it through an
/// import, a package or a qualifier: the overlay looks it up through the
/// base index.
fn cross_file_cases() -> Vec<Case> {
    let case = |label: &str,
                files: Vec<(&'static str, &'static str)>,
                edit: (&'static str, &'static str),
                expected| Case {
        label: label.to_string(),
        source_file: edit.0,
        files,
        edits: vec![edit],
        expected,
    };
    vec![
        case(
            "Java/package type",
            vec![
                ("St.java", "class St { int n; }\n"),
                ("Other.java", "class Other { int n; }\n"),
                (
                    "App.java",
                    "import java.util.*;\nclass App {\n    void take(int a) {}\n    List<St> gen() { return null; }\n}\n",
                ),
            ],
            (
                "App.java",
                "import java.util.*;\nclass App {\n    void take(St a) {}\n    List<Other> gen() { return null; }\n}\n",
            ),
            vec![("gen", "Other.java", "Other"), ("take", "St.java", "St")],
        ),
        case(
            "Rust/module-qualified param",
            vec![
                ("src/parse.rs", "pub struct Entry { pub n: u32 }\n"),
                ("src/other.rs", "pub struct Entry { pub m: u32 }\n"),
                (
                    "src/lib.rs",
                    "pub mod parse;\npub mod other;\npub fn q(a: u32) {}\n",
                ),
            ],
            (
                "src/lib.rs",
                "pub mod parse;\npub mod other;\npub fn q(a: parse::Entry) {}\npub fn deep(a: Vec<crate::parse::Entry>) {}\n",
            ),
            vec![
                ("deep", "src/parse.rs", "Entry"),
                ("q", "src/parse.rs", "Entry"),
            ],
        ),
        case(
            "TypeScript/namespace-qualified param",
            vec![
                ("types.ts", "export class St { n = 1 }\n"),
                ("other.ts", "export class St { m = 2 }\n"),
                (
                    "app.ts",
                    "import * as ns from './types';\nexport function q(a: number): void {}\n",
                ),
            ],
            (
                "app.ts",
                "import * as ns from './types';\nexport function q(a: ns.St): void {}\n",
            ),
            vec![("q", "types.ts", "St")],
        ),
        case(
            "Go/package-qualified param",
            vec![
                ("types/t.go", "package types\n\ntype St struct{ N int }\n"),
                ("other/o.go", "package other\n\ntype St struct{ M int }\n"),
                (
                    "main.go",
                    "package main\n\nimport \"app/types\"\n\nfunc Q(a int) {}\n",
                ),
            ],
            (
                "main.go",
                "package main\n\nimport \"app/types\"\n\nfunc Q(a []types.St) {}\n",
            ),
            vec![("Q", "types/t.go", "St")],
        ),
        case(
            "C++/namespace-qualified param",
            vec![
                ("a.h", "namespace a { struct St { int n; }; }\n"),
                ("b.h", "namespace b { struct St { int m; }; }\n"),
                (
                    "m.cpp",
                    "#include \"a.h\"\n#include \"b.h\"\n#include <vector>\nvoid q(int x) {}\nvoid p(std::vector<a::St> v) {}\n",
                ),
            ],
            (
                "m.cpp",
                "#include \"a.h\"\n#include \"b.h\"\n#include <vector>\nvoid q(a::St x) {}\nvoid p(std::vector<b::St> v) {}\n",
            ),
            vec![("p", "b.h", "St"), ("q", "a.h", "St")],
        ),
    ]
}

/// A dirty file adds a generic function over a project type named like its
/// type variable: the overlay replays the variable list the fragment carries,
/// so it binds the bound type and the plain function's `T`, never the
/// generic function's `T`.
fn generic_cases() -> Vec<Case> {
    vec![
        Case {
            label: "Rust/generic fn over a project T".to_string(),
            files: vec![(
                "src/gen.rs",
                "pub struct T;\npub struct Foo;\npub fn other() {}\n",
            )],
            edits: vec![(
                "src/gen.rs",
                "pub struct T;\npub struct Foo;\npub fn other() {}\npub fn generic_fn<T>(v: T, f: Foo) {}\npub fn plain(v: T) {}\n",
            )],
            source_file: "src/gen.rs",
            expected: vec![
                ("generic_fn", "src/gen.rs", "Foo"),
                ("plain", "src/gen.rs", "T"),
            ],
        },
        Case {
            label: "Java/generic method over a project T".to_string(),
            files: vec![(
                "Gen.java",
                "class T {}\nclass Foo {}\nclass Gen {\n    static void other() {}\n}\n",
            )],
            edits: vec![(
                "Gen.java",
                "class T {}\nclass Foo {}\nclass Gen {\n    static void other() {}\n    static <T> void generic_fn(T v, Foo f) {}\n    static void plain(T v) {}\n}\n",
            )],
            source_file: "Gen.java",
            expected: vec![
                ("generic_fn", "Gen.java", "Foo"),
                ("plain", "Gen.java", "T"),
            ],
        },
    ]
}

/// The `type_annotation` rows of `edges`, sorted.
fn rows(edges: &[ReasonedEdge]) -> Vec<(&str, &str, &str)> {
    let mut rows: Vec<_> = edges
        .iter()
        .filter(|e| e.reason == TYPE_ANNOTATION_REASON)
        .map(|e| {
            (
                e.edge.caller.as_str(),
                e.edge.callee_file.as_str(),
                e.edge.callee.as_str(),
            )
        })
        .collect();
    rows.sort_unstable();
    rows
}

/// Every mismatch of `cases`, one line each: a reindex that differs from
/// the expected rows (the fixture would prove nothing), or an overlay that
/// differs from the reindex in any field, reason and confidence included.
fn mismatches(cases: &[Case]) -> Vec<String> {
    let mut failures = Vec::new();
    for case in cases {
        let EdgeParity { reindex, overlay } = edges_after_edit(
            &parse,
            &case.files,
            &case.edits,
            RelType::Accesses,
            &|file, _| file == case.source_file,
        );
        let mut expected = case.expected.clone();
        expected.sort_unstable();
        let got = rows(&reindex);
        if got != expected {
            failures.push(format!(
                "{}: the reindex emits {got:?}, the fixture expects {expected:?}",
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
fn test_overlay_type_annotation_edges_match_reindex_across_14_languages() {
    let cases: Vec<Case> = LANGS.iter().map(lang_case).collect();
    assert_eq!(cases.len(), 14);
    let failures = mismatches(&cases);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn test_overlay_type_annotation_member_and_self_edges_match_reindex() {
    let failures = mismatches(&member_cases());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn test_overlay_type_annotation_cross_file_edges_match_reindex() {
    let failures = mismatches(&cross_file_cases());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ------------------------------------------------ fragments 0.17.3 wrote

const DIRTY: [&str; 2] = ["Sources/App.swift", "src/lib.rs"];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/overlay-written-by-0.17.3")
}

fn mtime_ns(path: &Path) -> u64 {
    std::fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// A worktree holding the uncommitted sources and a session whose
/// `l1/` holds the bins 0.17.3 wrote, under a manifest of the running
/// builder and cap: the entries' format is then the one thing that can
/// refuse them. Each entry's `mtime_ns` is the copied file's.
fn session_written_by_0_17_3(tmp: &Path) -> (PathBuf, PathBuf) {
    let fixture = fixture();
    let worktree = tmp.join("wt");
    for rel in DIRTY {
        let to = worktree.join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixture.join("source/uncommitted").join(rel), &to).unwrap();
    }
    let session_dir = tmp.join("sessions").join("v7fixture");
    std::fs::create_dir_all(&session_dir).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: "v7fixture".into(),
        pid: None,
        started_at: "2026-11-05T08:36:14Z".into(),
        last_touched: "2026-11-05T08:36:14Z".into(),
        base_sha: "ebfd6cf48534ebfec89b70adafb305c4fb12078d".into(),
        source_worktree: worktree.to_string_lossy().into_owned(),
        overlay_version: 2,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &meta).unwrap();
    let written = DirtyEvents::read(&fixture.join("l1/dirty_events.json")).unwrap();
    let mut manifest = DirtyEvents::empty(ecp_core::config::resolve_size_cap(&worktree));
    manifest.entries = written.entries;
    std::fs::create_dir_all(layout::fragments_dir(&session_dir)).unwrap();
    for (rel, entry) in &mut manifest.entries {
        entry.mtime_ns = mtime_ns(&worktree.join(rel));
        let id = entry.fragment_id.as_deref().unwrap();
        std::fs::copy(
            fixture.join("l1/fragments").join(format!("{id}.bin")),
            layout::fragment_path(&session_dir, id),
        )
        .unwrap();
    }
    DirtyEvents::write_atomic(&layout::dirty_events_path(&session_dir), &manifest).unwrap();
    (session_dir, worktree)
}

/// The graph of `tree`, parsed as the parity tests parse.
fn build(tree: &[(&str, String)]) -> Vec<u8> {
    let mut builder = GraphBuilder::new();
    for (path, source) in tree {
        builder.add_graph(parse(path, source));
    }
    rkyv::to_bytes::<rkyv::rancor::Error>(&builder.build())
        .unwrap()
        .into_vec()
}

fn sources(side: &str) -> Vec<(&'static str, String)> {
    DIRTY
        .iter()
        .map(|&rel| {
            let text = std::fs::read_to_string(fixture().join("source").join(side).join(rel));
            (rel, text.unwrap())
        })
        .collect()
}

/// `(source file, source, target file, target, confidence)` of every
/// `type_annotation` edge out of the dirty files, sorted.
fn annotation_rows(
    graph: &ArchivedZeroCopyGraph,
    merged: &MergedGraph<'_>,
) -> Vec<(String, String, String, String, f32)> {
    let mut rows: Vec<_> = merged
        .all_edges()
        .filter(|e| e.rel_type() == RelType::Accesses && e.reason(graph) == TYPE_ANNOTATION_REASON)
        .filter_map(|e| {
            let source = merged.node(e.source)?;
            let target = merged.node(e.target)?;
            let source_file = source.file_path(merged)?;
            DIRTY.contains(&source_file).then(|| {
                (
                    source_file.to_string(),
                    source.name(merged).to_string(),
                    target.file_path(merged).unwrap_or_default().to_string(),
                    target.name(merged).to_string(),
                    e.confidence(),
                )
            })
        })
        .collect();
    rows.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    rows.dedup();
    rows
}

/// Pass/fail criterion (the fixture README, written before the run): the
/// V7 entries are refused, never decoded as V8; after the re-parse the
/// freshness gate makes, the view's `type_annotation` edges out of the
/// dirty files equal the full reindex of the uncommitted tree, and the base
/// edges `f -> Other` and `make -> Other` are gone.
#[test]
fn test_view_inputs_fragments_written_by_0_17_3_refused_then_reparse_serves_reindex_edges() {
    let written = DirtyEvents::read(&fixture().join("l1/dirty_events.json")).unwrap();
    assert_eq!(written.builder_fingerprint, "v0.17.3+schema52");
    for rel in DIRTY {
        assert_eq!(written.entries[rel].format, 7, "{rel}");
    }
    let tmp = tempfile::tempdir().unwrap();
    let (session_dir, worktree) = session_written_by_0_17_3(tmp.path());
    let manifest = DirtyEvents::read(&layout::dirty_events_path(&session_dir)).unwrap();
    assert!(manifest.is_current(ecp_core::config::resolve_size_cap(&worktree)));

    let (refused, _) = accepted_fragments(&session_dir, &worktree).unwrap();
    assert_eq!(refused.len(), 0, "a V7 bin is never served as V8");

    let uncommitted = sources("uncommitted");
    let graphs: Vec<LocalGraph> = uncommitted
        .iter()
        .map(|(rel, text)| parse(rel, text))
        .collect();
    let mtimes: Vec<u64> = DIRTY
        .iter()
        .map(|rel| mtime_ns(&worktree.join(rel)))
        .collect();
    write_dirty_fragments_from_graphs(&session_dir, &graphs, &mtimes).unwrap();
    let (accepted, _) = accepted_fragments(&session_dir, &worktree).unwrap();
    let inputs = decode_view_inputs(&accepted);
    assert_eq!(inputs.len(), 2);
    let property = inputs
        .iter()
        .flat_map(|input| &input.symbols)
        .find(|s| s.name == "f")
        .expect("the Swift property f");
    assert!(
        property
            .type_annotation
            .as_deref()
            .is_some_and(|ty| ty.contains("St")),
        "{:?}",
        property.type_annotation
    );

    let base = build(&sources("committed"));
    let base = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&base).unwrap();
    let view = OverlayView::build(base, &inputs).expect("edits make a view");
    let overlay = annotation_rows(base, &MergedGraph::new(base, Some(&view)));
    let full = build(&uncommitted);
    let full = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&full).unwrap();
    let reindex = annotation_rows(full, &MergedGraph::new(full, None));

    let named = |rows: &[(String, String, String, String, f32)]| -> Vec<(String, String, String)> {
        rows.iter()
            .map(|(file, source, _, target, _)| (file.clone(), source.clone(), target.clone()))
            .collect()
    };
    let want: Vec<(String, String, String)> = [
        ("Sources/App.swift", "f", "St"),
        ("Sources/App.swift", "many", "Other"),
        ("Sources/App.swift", "take", "St"),
        ("src/lib.rs", "make", "St"),
        ("src/lib.rs", "take", "Other"),
    ]
    .iter()
    .map(|(file, source, target)| (file.to_string(), source.to_string(), target.to_string()))
    .collect();
    assert_eq!(named(&reindex), want, "the reindex of the uncommitted tree");
    assert_eq!(overlay, reindex);
}

#[test]
fn test_overlay_type_annotation_generic_params_match_reindex() {
    let failures = mismatches(&generic_cases());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ------------------------- A10 / A15a / A15b: field type arguments

/// One language's field fixture: a clean part, and the dirty file `path` in
/// two forms. `Box` names its class type parameter `Token` in `generic`
/// (the field binds nothing) and has none in `plain` (the field binds the
/// project class `Token`). `rows_*` are the reindex rows out of `path`.
struct FieldLang {
    lang: &'static str,
    clean: &'static [(&'static str, &'static str)],
    path: &'static str,
    generic: &'static str,
    plain: &'static str,
    rows_generic: &'static [Row],
    rows_plain: &'static [Row],
}

const FIELD_LANGS: &[FieldLang] = &[
    FieldLang {
        lang: "Java",
        clean: &[
            ("Token.java", "class Token {}\n"),
            ("Holder.java", "import java.util.List;\nclass Holder {\n    List<Token> tokens;\n}\n"),
        ],
        path: "Box.java",
        generic: "import java.util.List;\nclass Box<Token> {\n    List<Token> items;\n}\n",
        plain: "import java.util.List;\nclass Box {\n    List<Token> items;\n}\n",
        rows_generic: &[],
        rows_plain: &[("items", "Token.java", "Token")],
    },
    FieldLang {
        lang: "C#",
        clean: &[
            ("Token.cs", "class Token {}\n"),
            ("Holder.cs", "using System.Collections.Generic;\nclass Holder {\n    List<Token> tokens;\n}\n"),
        ],
        path: "Box.cs",
        generic: "using System.Collections.Generic;\nclass Box<Token> {\n    List<Token> items;\n}\n",
        plain: "using System.Collections.Generic;\nclass Box {\n    List<Token> items;\n}\n",
        rows_generic: &[],
        rows_plain: &[("items", "Token.cs", "Token")],
    },
    FieldLang {
        lang: "Kotlin",
        clean: &[
            ("Token.kt", "class Token\n"),
            ("Holder.kt", "class Holder {\n    val tokens: List<Token> = listOf()\n}\n"),
        ],
        path: "Box.kt",
        generic: "class Box<Token> {\n    val items: List<Token> = listOf()\n}\n",
        plain: "class Box {\n    val items: List<Token> = listOf()\n}\n",
        rows_generic: &[],
        rows_plain: &[("items", "Token.kt", "Token")],
    },
    FieldLang {
        lang: "TypeScript",
        clean: &[
            ("token.ts", "export class Token {}\n"),
            ("holder.ts", "import { Token } from './token';\nexport class Holder {\n  tokens: Array<Token> = [];\n}\n"),
        ],
        path: "box.ts",
        generic: "export class Box<Token> {\n  items: Array<Token> = [];\n}\n",
        plain: "import { Token } from './token';\nexport class Box {\n  items: Array<Token> = [];\n}\n",
        rows_generic: &[],
        rows_plain: &[("items", "token.ts", "Token")],
    },
    FieldLang {
        lang: "Swift",
        clean: &[
            ("Token.swift", "class Token {}\n"),
            ("Holder.swift", "class Holder {\n    var tokens: Array<Token> = []\n}\n"),
        ],
        path: "Box.swift",
        generic: "class Box<Token> {\n    var items: Array<Token> = []\n}\n",
        plain: "class Box {\n    var items: Array<Token> = []\n}\n",
        rows_generic: &[],
        rows_plain: &[("items", "Token.swift", "Token")],
    },
    FieldLang {
        lang: "Python",
        clean: &[
            ("tokens.py", "class Token: ...\n"),
            ("holder.py", "from tokens import Token\n\nclass Holder:\n    tokens: list[Token]\n"),
        ],
        path: "box.py",
        generic: "class Box[Token]:\n    items: list[Token]\n",
        plain: "from tokens import Token\n\nclass Box:\n    items: list[Token]\n",
        rows_generic: &[],
        rows_plain: &[("items", "tokens.py", "Token")],
    },
    FieldLang {
        lang: "Rust",
        clean: &[],
        path: "src/lib.rs",
        generic: "pub struct Token;\npub struct Bag<Token> {\n    items: Vec<Token>,\n}\npub struct Holder {\n    tokens: Vec<Token>,\n}\n",
        plain: "pub struct Token;\npub struct Bag {\n    items: Vec<Token>,\n}\npub struct Holder {\n    tokens: Vec<Token>,\n}\n",
        rows_generic: &[("tokens", "src/lib.rs", "Token")],
        rows_plain: &[
            ("items", "src/lib.rs", "Token"),
            ("tokens", "src/lib.rs", "Token"),
        ],
    },
    FieldLang {
        lang: "Dart",
        clean: &[],
        path: "lib/app.dart",
        generic: "class Token {}\nclass Box<Token> {\n  List<Token> items = [];\n}\nclass Holder {\n  List<Token> tokens = [];\n}\n",
        plain: "class Token {}\nclass Box {\n  List<Token> items = [];\n}\nclass Holder {\n  List<Token> tokens = [];\n}\n",
        rows_generic: &[("tokens", "lib/app.dart", "Token")],
        rows_plain: &[
            ("items", "lib/app.dart", "Token"),
            ("tokens", "lib/app.dart", "Token"),
        ],
    },
    FieldLang {
        lang: "Go",
        clean: &[],
        path: "main.go",
        generic: "package main\n\ntype Token struct{}\ntype List[E any] struct{}\ntype Box[Token any] struct {\n\titems List[Token]\n}\ntype Holder struct {\n\ttokens List[Token]\n}\n",
        plain: "package main\n\ntype Token struct{}\ntype List[E any] struct{}\ntype Box struct {\n\titems List[Token]\n}\ntype Holder struct {\n\ttokens List[Token]\n}\n",
        rows_generic: &[
            ("items", "main.go", "List"),
            ("tokens", "main.go", "List"),
            ("tokens", "main.go", "Token"),
        ],
        rows_plain: &[
            ("items", "main.go", "List"),
            ("items", "main.go", "Token"),
            ("tokens", "main.go", "List"),
            ("tokens", "main.go", "Token"),
        ],
    },
    FieldLang {
        lang: "C++",
        clean: &[],
        path: "tokens.hpp",
        generic: "#include <vector>\nstruct Token {};\ntemplate <class Token>\nstruct Box {\n    std::vector<Token> items;\n};\nstruct Holder {\n    std::vector<Token> tokens;\n};\n",
        plain: "#include <vector>\nstruct Token {};\nstruct Box {\n    std::vector<Token> items;\n};\nstruct Holder {\n    std::vector<Token> tokens;\n};\n",
        rows_generic: &[("tokens", "tokens.hpp", "Token")],
        rows_plain: &[
            ("items", "tokens.hpp", "Token"),
            ("tokens", "tokens.hpp", "Token"),
        ],
    },
];

/// The `type_annotation` and `type_annotation:type_argument` rows of
/// `edges`, sorted: a field's generic argument carries the second reason.
fn field_rows(edges: &[ReasonedEdge]) -> Vec<(&str, &str, &str)> {
    let mut rows: Vec<_> = edges
        .iter()
        .filter(|e| e.reason == TYPE_ANNOTATION_REASON || e.reason == TYPE_ARGUMENT_REASON)
        .map(|e| {
            (
                e.edge.caller.as_str(),
                e.edge.callee_file.as_str(),
                e.edge.callee.as_str(),
            )
        })
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// Like [`mismatches`] over both annotation reasons. A case whose reindex
/// holds fewer rows than `non_empty` expects fails: the positive control.
fn field_mismatches(cases: &[Case]) -> Vec<String> {
    let mut failures = Vec::new();
    for case in cases {
        let EdgeParity { reindex, overlay } = edges_after_edit(
            &parse,
            &case.files,
            &case.edits,
            RelType::Accesses,
            &|file, _| file == case.source_file,
        );
        let mut expected = case.expected.clone();
        expected.sort_unstable();
        let got = field_rows(&reindex);
        if got != expected {
            failures.push(format!(
                "{}: the reindex emits {got:?}, the fixture expects {expected:?}",
                case.label
            ));
            continue;
        }
        if field_rows(&overlay) != got {
            failures.push(format!(
                "{}: overlay {overlay:#?}\nreindex {reindex:#?}",
                case.label
            ));
        }
    }
    failures
}

/// The case for one language and direction: `to_generic` makes `Box` gain
/// its class type parameter (a base `items -> Token` edge must be masked),
/// else lose it (the edge appears).
fn field_case(fixture: &FieldLang, to_generic: bool) -> Case {
    let (base, edited, expected, direction) = if to_generic {
        (
            fixture.plain,
            fixture.generic,
            fixture.rows_generic,
            "gains",
        )
    } else {
        (fixture.generic, fixture.plain, fixture.rows_plain, "loses")
    };
    let mut files = fixture.clean.to_vec();
    files.push((fixture.path, base));
    Case {
        label: format!("{}/Box {direction} its class type parameter", fixture.lang),
        files,
        edits: vec![(fixture.path, edited)],
        source_file: fixture.path,
        expected: expected.to_vec(),
    }
}

/// Stands for (A10, dirty `Box`): the overlay agrees with a reindex on a
/// field whose class type parameter shares a project class's name, in both
/// edit directions. One reindex expects a non-empty row set, so "emit
/// nothing" cannot pass. The class type parameters must reach the overlay
/// input too: a fix in the builder alone fails here.
#[test]
fn test_overlay_field_class_type_parameter_matches_reindex() {
    let cases: Vec<Case> = FIELD_LANGS
        .iter()
        .flat_map(|f| [field_case(f, true), field_case(f, false)])
        .collect();
    assert!(
        cases.iter().any(|c| !c.expected.is_empty()),
        "positive control: some case binds the project class"
    );
    let failures = field_mismatches(&cases);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Stands for (A10, dirty `Token`): renaming the project class in its clean
/// file leaves `Holder.tokens` with no target, in the overlay as in a reindex.
#[test]
fn test_overlay_field_project_class_renamed_matches_reindex() {
    let holder = "import java.util.List;\nclass Holder {\n    List<Token> tokens;\n}\n";
    let case = |label: &str, edited: &'static str, expected: Vec<Row>| Case {
        label: label.to_string(),
        files: vec![("Token.java", "class Token {}\n"), ("Holder.java", holder)],
        edits: vec![("Token.java", edited)],
        source_file: "Holder.java",
        expected,
    };
    let cases = [
        case(
            "Java/Token kept dirty",
            "class Token { int n; }\n",
            vec![("tokens", "Token.java", "Token")],
        ),
        case("Java/Token renamed", "class Tok {}\n", Vec::new()),
    ];
    let failures = field_mismatches(&cases);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Stands for (A15a): an interface property's annotation reaches the overlay
/// like a class field's, whether the interface or the annotated class is dirty.
#[test]
fn test_overlay_typescript_interface_property_annotation_matches_reindex() {
    let cases = [
        Case {
            label: "TypeScript/interface property added".to_string(),
            files: vec![(
                "cat.ts",
                "export class Owner {}\nexport interface Cat {\n  name: string;\n}\n",
            )],
            edits: vec![(
                "cat.ts",
                "export class Owner {}\nexport interface Cat {\n  name: string;\n  owner: Owner;\n}\n",
            )],
            source_file: "cat.ts",
            expected: vec![("owner", "cat.ts", "Owner")],
        },
        Case {
            label: "TypeScript/Owner dirty".to_string(),
            files: vec![
                ("owner.ts", "export class Owner {}\n"),
                (
                    "cat.ts",
                    "import { Owner } from './owner';\nexport interface Cat {\n  owner: Owner;\n}\n",
                ),
            ],
            edits: vec![("owner.ts", "export class Owner { n = 1; }\n")],
            source_file: "cat.ts",
            expected: vec![("owner", "owner.ts", "Owner")],
        },
    ];
    let failures = field_mismatches(&cases);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Stands for (A15b): a Dart top-level variable's generic or nullable
/// annotation binds the argument, never `List`, in the overlay as in a reindex.
#[test]
fn test_overlay_dart_top_level_variable_annotation_matches_reindex() {
    let cases = [Case {
        label: "Dart/top-level generic and nullable".to_string(),
        files: vec![(
            "lib/app.dart",
            "class Item {}\n\nList<int> items = [];\nint maybe = 0;\n",
        )],
        edits: vec![(
            "lib/app.dart",
            "class Item {}\n\nList<Item> items = [];\nItem? maybe;\n",
        )],
        source_file: "lib/app.dart",
        expected: vec![
            ("items", "lib/app.dart", "Item"),
            ("maybe", "lib/app.dart", "Item"),
        ],
    }];
    let failures = field_mismatches(&cases);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
