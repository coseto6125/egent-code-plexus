//! R10 (B6b): the import-member tier (R8: explicit, retry, extension,
//! external), the Kotlin extension retry of the ladder (R17) and the
//! wildcard pass after heritage (R19) run once, in
//! `ecp_core::analyzer::ladder`, for the index and the overlay. Each case
//! plants one fact in a small tree. It asserts what a reader of a fresh
//! full index gets (each caller's exact `Calls` targets, at an allowed
//! confidence), then that the overlay view of the same tree (the caller
//! files dirty, unchanged but for a trailing newline; a removed file
//! tombstoned) has no `Calls` edge that differs from that index, by source
//! uid, target uid and exact confidence. The overlay stamps its own
//! uncommitted-edit reason on a `Calls` edge, so the comparison leaves
//! reasons out (`common/strict_parity.rs`); the confidence names the tier.
//!
//! Every fixture carries a decoy that a wrong tier would bind (a namesake in
//! another file or package), so an empty expectation is not a vacuous pass.
//!
//! Languages: the import-member tier runs only where calls bind through an
//! import's member, `import_member_fallback` in
//! `crates/ecp-core/src/analyzer/import_binding.rs`:
//! `language == Language::Python || fqn_language(language)`, and
//! `fqn_extension` names Java, Kotlin and PHP. So these have a case, and the
//! others are exempt, as no tier this slice moved runs for them:
//!
//! ```text
//! ladder_import_member typescript no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member javascript no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member csharp no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member go no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member rust no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member ruby no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member swift no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member c no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member cpp no import-member tier (import_binding.rs import_member_fallback)
//! ladder_import_member dart no import-member tier (import_binding.rs import_member_fallback)
//! ```
//!
//! One divergence stays, behind `ladder::imported_member`'s `wildcards`
//! parameter: a Java / Kotlin name reference the name tiers leave unbound
//! asks the import tier alone for a field. The index
//! (`Resolver::resolve_imported_symbol`) takes the wildcard pass too; the
//! overlay (`resolve_overlay_name_ref`) takes the explicit imports only.
//! [`test_imported_member_java_static_wildcard_field_ref_index_only`] pins it.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::session::merged::MergedGraph;
use std::collections::BTreeSet;
use strict_parity::{run, snapshot, Edit, Observed, Scenario, Snapshot};

type Files<'a> = &'a [(&'a str, &'a str)];

const GLOBAL: &[f32] = &[0.7];
const HERITAGE: &[f32] = &[0.8];
const IMPORT_SCOPED: &[f32] = &[0.95];

/// What one caller must call: `(callee name, callee file, allowed confidences)`.
type Callees<'a> = &'a [(&'a str, &'a str, &'a [f32])];

/// One case: the tree, the caller files the overlay dirties, the files the
/// edit removes, and, per caller, what it must call in a fresh full index of
/// the edited tree.
struct Case {
    name: &'static str,
    files: Files<'static>,
    dirty: &'static [&'static str],
    removed: &'static [&'static str],
    expected: &'static [(&'static str, Callees<'static>)],
}

/// Python. `helper` binds through its explicit import, ahead of the
/// namesake in `lib/extra.py`; `alias()` binds the member its alias names.
/// `missing` is no member of `pkg.util`, a module that may be local: the
/// retry resolves the callee without imports, at the global tier. `requests`
/// is external: no edge, though `lib/extra.py` holds the only `get`. Two
/// imports of `helper`: the first hit in source order binds.
const PYTHON: Case = Case {
    name: "ladder_import_member_python",
    files: &[
        ("pkg/__init__.py", ""),
        (
            "pkg/util.py",
            "def helper():\n    return 1\n\n\ndef other():\n    return 2\n",
        ),
        (
            "lib/extra.py",
            "def helper():\n    return 3\n\n\ndef missing():\n    return 4\n\n\ndef get():\n    return 5\n",
        ),
        (
            "app/main.py",
            "from pkg.util import helper\nfrom pkg.util import other as alias\nfrom pkg.util import missing\nfrom requests import get\n\n\ndef explicit():\n    return helper()\n\n\ndef aliased():\n    return alias()\n\n\ndef retried():\n    return missing()\n\n\ndef external():\n    return get()\n",
        ),
        (
            "app/twice.py",
            "from pkg.util import helper\nfrom lib.extra import helper\n\n\ndef twice():\n    return helper()\n",
        ),
    ],
    dirty: &["app/main.py", "app/twice.py"],
    removed: &[],
    expected: &[
        ("explicit", &[("helper", "pkg/util.py", IMPORT_SCOPED)]),
        ("aliased", &[("other", "pkg/util.py", IMPORT_SCOPED)]),
        ("retried", &[("missing", "lib/extra.py", GLOBAL)]),
        ("external", &[]),
        ("twice", &[("helper", "pkg/util.py", IMPORT_SCOPED)]),
    ],
};

/// Java. A static import binds its owner's member, not `Other.run`.
/// `absent` is no member of `lib.Util`, whose head `lib` is indexed: the
/// general tiers keep the global `misc.Tools.absent`. `com.ext` is external:
/// no edge, though `misc.Tools.fetch` is the only `fetch`. Two static
/// imports of `run` holding different members: no hit, and the global tier
/// sees two `run`s.
const JAVA: Case = Case {
    name: "ladder_import_member_java",
    files: &[
        (
            "lib/Util.java",
            "package lib;\npublic class Util {\n    public static void run() {}\n}\n",
        ),
        (
            "lib/Other.java",
            "package lib;\npublic class Other {\n    public static void run() {}\n}\n",
        ),
        (
            "misc/Tools.java",
            "package misc;\npublic class Tools {\n    public static void absent() {}\n    public static void fetch() {}\n}\n",
        ),
        (
            "app/Main.java",
            "package app;\nimport static lib.Util.run;\nimport static lib.Util.absent;\nimport static com.ext.Net.fetch;\nclass Main {\n    void explicit() { run(); }\n    void notExported() { absent(); }\n    void external() { fetch(); }\n}\n",
        ),
        (
            "app/Twice.java",
            "package app;\nimport static lib.Util.run;\nimport static lib.Other.run;\nclass Twice {\n    void twice() { run(); }\n}\n",
        ),
    ],
    dirty: &["app/Main.java", "app/Twice.java"],
    removed: &[],
    expected: &[
        ("explicit", &[("run", "lib/Util.java", IMPORT_SCOPED)]),
        ("notExported", &[("absent", "misc/Tools.java", GLOBAL)]),
        ("external", &[]),
        ("twice", &[]),
    ],
};

/// Kotlin. A top-level function import binds ahead of `other.helper`. A
/// class import types `b.size()`: the member of `Box` binds, not the
/// same-named extension `fun Box.size()`. `Box` owns no `grow`: the receiver
/// ladder binds nothing, and the extension retry (R17) binds the top-level
/// `fun Box.grow()`.
const KOTLIN: Case = Case {
    name: "ladder_import_member_kotlin",
    files: &[
        (
            "src/main/kotlin/lib/Util.kt",
            "package lib\nfun helper() {}\n",
        ),
        (
            "src/main/kotlin/lib/Box.kt",
            "package lib\nclass Box {\n fun size(): Int = 0\n}\n",
        ),
        (
            "src/main/kotlin/lib/BoxExt.kt",
            "package lib\nfun Box.size(): Int = 1\nfun Box.grow() {}\n",
        ),
        (
            "src/main/kotlin/other/Helpers.kt",
            "package other\nfun helper() {}\n",
        ),
        (
            "src/main/kotlin/app/Main.kt",
            "package app\nimport lib.helper\nimport lib.Box\nfun explicit() { helper() }\nfun member(b: Box) { b.size() }\nfun extension(b: Box) { b.grow() }\n",
        ),
    ],
    dirty: &["src/main/kotlin/app/Main.kt"],
    removed: &[],
    expected: &[
        (
            "explicit",
            &[("helper", "src/main/kotlin/lib/Util.kt", IMPORT_SCOPED)],
        ),
        (
            "member",
            &[("size", "src/main/kotlin/lib/Box.kt", IMPORT_SCOPED)],
        ),
        (
            "extension",
            &[("grow", "src/main/kotlin/lib/BoxExt.kt", GLOBAL)],
        ),
    ],
};

/// PHP. `use function pkg\helper` binds ahead of `other\helper`; an aliased
/// `use function` binds the member it names. `Vendor\Pkg` is external: no
/// edge, though `other\lonely` is the only `lonely`.
const PHP: Case = Case {
    name: "ladder_import_member_php",
    files: &[
        (
            "src/pkg/Helper.php",
            "<?php namespace pkg; function helper() {} function spare() {}",
        ),
        (
            "src/other/util.php",
            "<?php namespace other; function helper() {} function lonely() {}",
        ),
        (
            "app.php",
            "<?php use function pkg\\helper; use function pkg\\spare as s; use function Vendor\\Pkg\\lonely; function explicit() { helper(); } function aliased() { s(); } function external() { lonely(); }",
        ),
    ],
    dirty: &["app.php"],
    removed: &[],
    expected: &[
        ("explicit", &[("helper", "src/pkg/Helper.php", IMPORT_SCOPED)]),
        ("aliased", &[("spare", "src/pkg/Helper.php", IMPORT_SCOPED)]),
        ("external", &[]),
    ],
};

/// The Kotlin wildcard tree: `Screen` inherits `refresh` from `Base`, and
/// `import util.*` imports the top-level `refresh` and `tidy`. `other`
/// declares both names too, so the global tier binds neither.
const WILDCARD_FILES: Files<'static> = &[
    (
        "src/main/kotlin/base/Base.kt",
        "package base\nopen class Base {\n fun refresh() {}\n}\n",
    ),
    (
        "src/main/kotlin/util/Tools.kt",
        "package util\nfun refresh() {}\nfun tidy() {}\n",
    ),
    (
        "src/main/kotlin/other/More.kt",
        "package other\nfun refresh() {}\nfun tidy() {}\n",
    ),
    (
        "src/main/kotlin/app/Screen.kt",
        "package app\nimport base.Base\nimport util.*\nclass Screen : Base() {\n fun run() { refresh() }\n}\nfun go() { tidy() }\n",
    ),
];

/// R19. The caller's base beats the star import (`run` binds `Base.refresh`
/// at the heritage tier); with no base member, the wildcard pass binds
/// `util.tidy` (`go`).
const KOTLIN_WILDCARD_AFTER_HERITAGE: Case = Case {
    name: "ladder_import_member_kotlin_wildcard",
    files: WILDCARD_FILES,
    dirty: &["src/main/kotlin/app/Screen.kt"],
    removed: &[],
    expected: &[
        (
            "run",
            &[("refresh", "src/main/kotlin/base/Base.kt", HERITAGE)],
        ),
        (
            "go",
            &[("tidy", "src/main/kotlin/util/Tools.kt", IMPORT_SCOPED)],
        ),
    ],
};

/// R19 with the wildcard's only file removed: the star import names no
/// file, so `go` falls to the global tier, where `other.tidy` is now the
/// only `tidy`.
const KOTLIN_WILDCARD_REMOVED_FILE: Case = Case {
    name: "ladder_import_member_kotlin_wildcard_removed",
    files: WILDCARD_FILES,
    dirty: &["src/main/kotlin/app/Screen.kt"],
    removed: &["src/main/kotlin/util/Tools.kt"],
    expected: &[
        (
            "run",
            &[("refresh", "src/main/kotlin/base/Base.kt", HERITAGE)],
        ),
        ("go", &[("tidy", "src/main/kotlin/other/More.kt", GLOBAL)]),
    ],
};

/// `name` of a snapshot node line `<kind> <file>:<line> <name> #<uid>`, with its file.
fn name_and_file(node: &str) -> (&str, &str) {
    let mut parts = node.split(' ');
    let (_kind, location, name) = (parts.next(), parts.next(), parts.next());
    let location = location.unwrap_or_default();
    let file = location.rsplit_once(':').map_or(location, |(file, _)| file);
    (name.unwrap_or_default(), file)
}

/// `(caller, callee, callee file, confidence)` of every `rel` edge.
fn edges(snap: &Snapshot, rel: &str) -> Vec<(String, String, String, String)> {
    let prefix = format!("{rel} ");
    snap.edge
        .iter()
        .filter_map(|line| {
            let (source, target) = line.strip_prefix(prefix.as_str())?.split_once(" -> ")?;
            let (target, confidence) = target.rsplit_once(" conf=")?;
            let (caller, _) = name_and_file(source);
            let (callee, file) = name_and_file(target);
            Some((
                caller.to_string(),
                callee.to_string(),
                file.to_string(),
                confidence.to_string(),
            ))
        })
        .collect()
}

/// The production index of `case`'s edited tree and the overlay of the edit.
fn observe_case(case: &Case) -> Observed {
    let edited: Vec<(&str, String)> = case
        .dirty
        .iter()
        .map(|&path| {
            let (_, source) = case
                .files
                .iter()
                .find(|(p, _)| *p == path)
                .expect("a dirty file is one of the files");
            (path, format!("{source}\n"))
        })
        .collect();
    let edits: Vec<Edit<'_>> = edited
        .iter()
        .map(|(path, source)| Edit::Write(path, source.as_str()))
        .chain(case.removed.iter().map(|&path| Edit::Delete(path)))
        .collect();
    run(&Scenario {
        name: case.name,
        owner: "R10 (B6b import-member tier)",
        files: case.files,
        edits: &edits,
    })
}

/// Hold the full index of `case`'s edited tree to `case.expected`, then the
/// overlay's `Calls` edges to the index's.
fn check(case: &Case) {
    let observed = observe_case(case);
    let calls = edges(&snapshot(&MergedGraph::new(observed.full(), None)), "Calls");
    for (caller, callees) in case.expected {
        let mut actual: Vec<(&str, &str, &str)> = calls
            .iter()
            .filter(|(source, ..)| source == caller)
            .map(|(_, callee, file, confidence)| {
                (callee.as_str(), file.as_str(), confidence.as_str())
            })
            .collect();
        actual.sort();
        let names: Vec<(&str, &str)> = actual.iter().map(|&(n, f, _)| (n, f)).collect();
        let mut want: Vec<(&str, &str)> = callees.iter().map(|&(n, f, _)| (n, f)).collect();
        want.sort();
        assert_eq!(
            names, want,
            "{}: index Calls edges of {caller}: {actual:?}",
            case.name
        );
        for (name, file, confidence) in &actual {
            let (_, _, allowed) = callees
                .iter()
                .find(|(n, f, _)| n == name && f == file)
                .unwrap();
            assert!(
                allowed.iter().any(|c| format!("{c:?}") == *confidence),
                "{}: {caller} -> {name} at {confidence}, allowed {allowed:?}",
                case.name
            );
        }
    }
    let differing: BTreeSet<String> = observed
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("edge ") && line.contains(" Calls "))
        .collect();
    assert!(
        differing.is_empty(),
        "{}: the overlay's Calls edges differ from the full index: {differing:#?}",
        case.name
    );
}

#[test]
fn test_ladder_import_member_python_same_both_sides() {
    check(&PYTHON);
}

#[test]
fn test_ladder_import_member_java_same_both_sides() {
    check(&JAVA);
}

#[test]
fn test_ladder_import_member_kotlin_same_both_sides() {
    check(&KOTLIN);
}

#[test]
fn test_ladder_import_member_php_same_both_sides() {
    check(&PHP);
}

#[test]
fn test_ladder_wildcard_member_kotlin_after_heritage_same_both_sides() {
    check(&KOTLIN_WILDCARD_AFTER_HERITAGE);
}

#[test]
fn test_ladder_wildcard_member_kotlin_removed_file_same_both_sides() {
    check(&KOTLIN_WILDCARD_REMOVED_FILE);
}

/// The divergence the slice keeps: `TABLE` is a field of `app.Consts`, which
/// `q` reads through the static wildcard `import static app.Consts.*`. The
/// name tiers bind no `Value` named `TABLE`, so the import tier alone runs
/// for a field. The index takes the wildcard pass and binds it at the import
/// tier; the overlay takes the explicit imports only and binds nothing. The
/// fix that converges them flips this test: the overlay line goes away.
#[test]
fn test_imported_member_java_static_wildcard_field_ref_index_only() {
    let case = Case {
        name: "ladder_import_member_java_static_wildcard_field_ref",
        files: &[
            (
                "app/Consts.java",
                "package app;\npublic class Consts {\n    public static final int TABLE = 3;\n}\n",
            ),
            (
                "web/Repo.java",
                "package web;\nimport static app.Consts.*;\npublic class Repo {\n    int q() {\n        return TABLE;\n    }\n}\n",
            ),
        ],
        dirty: &["web/Repo.java"],
        removed: &[],
        expected: &[],
    };
    let observed = observe_case(&case);
    let reads: Vec<_> = edges(
        &snapshot(&MergedGraph::new(observed.full(), None)),
        "ReadsField",
    )
    .into_iter()
    .filter(|(caller, callee, ..)| caller == "q" && callee == "TABLE")
    .collect();
    assert_eq!(
        reads,
        [(
            "q".to_string(),
            "TABLE".to_string(),
            "app/Consts.java".to_string(),
            "0.95".to_string()
        )],
        "the index binds the field through the wildcard"
    );
    let differing: Vec<String> = observed
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("edge ") && line.contains(" TABLE "))
        .collect();
    assert_eq!(differing.len(), 1, "{differing:#?}");
    assert!(
        differing[0].starts_with("edge full-only x1 ReadsField ")
            && differing[0].ends_with(" conf=0.95"),
        "only the index reads TABLE: {differing:#?}"
    );
}
