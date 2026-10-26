//! R13 (B8b): the lexical tail returns `Resolution::Shadowed`, and one
//! import-only path (`ecp_core::analyzer::ladder::call`, `import_only`) serves
//! the index and the overlay. A call whose name a lexical scope of the caller's
//! file declares, though none in reach binds it, skips every name tier: only
//! the import tiers and the constructor fallback may resolve it.
//!
//! Each case plants one tree and asserts what a reader of a fresh full index
//! gets (the caller's exact `Calls` targets, at an allowed confidence), then
//! that the overlay view of the same tree (the caller file dirty, unchanged but
//! for a trailing newline) has no `Calls` edge that differs from that index, by
//! source uid, target uid and exact confidence. A case with no `expected`
//! callers holds the parity claim only. The overlay stamps its own
//! uncommitted-edit reason on a `Calls` edge, so the comparison leaves reasons
//! out (`common/strict_parity.rs`); the confidence names the tier.
//!
//! The L1 differences between the two tails, and what each case pins:
//!
//! ```text
//! tier of the index tail (resolve_imported_call, now gone)  overlay before R13        reachable on a lexical site    case
//! UntypedMember ends before any tier                        same                      JS / TS (name read)            ladder.rs unit test (the JS / TS parsers write no such site for a bare name)
//! Python submodule member, declared attribute               absent                    no: ScopedOnly reads Plain only,
//!                                                                                      JS / TS write no FromReceiver  none (dropped, no index edge moves)
//! import tiers, JS / TS: named-import tier                  reexport::named_import    yes                            named_import, default_import, namespace_import, no_import, two_scopes_deep
//! import tiers, Python: import-member tier                  reexport::named_import    yes (two nested defs in a scope) python_ambiguous_nested_*
//! constructor fallback through the import tiers             R12: constructed_type     yes                            class
//! ```
//!
//! The overlay's Python difference is the one that moves an overlay edge: a
//! name the file's own barrel (`pkg/__init__.py`) re-exports binds in the index
//! through `ladder::imported_member`'s Python re-export step, and not in the
//! overlay's ES `named_import`. The JS / TS tails already agree after R12, so
//! their cases hold the line.
//!
//! `LexicalAdmission` (`lexical.rs`) stays an input of the tail: the Python and
//! Rust cases are `ScopedOnly`, where a name with no scoped node in reach is
//! `Unknown` and keeps its name tiers (the `visible` cases), and only two
//! scoped nodes in one scope are `Shadowed`. The other ten of the 14 mainstream
//! languages have no lexical tier:
//!
//! ```text
//! ladder_lexical_tail java no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Java) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail kotlin no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Kotlin) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail csharp no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(CSharp) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail go no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Go) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail php no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Php) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail ruby no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Ruby) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail swift no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Swift) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail c no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(C) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail cpp no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Cpp) is empty, lexical.rs:32-39, so None)
//! ladder_lexical_tail dart no lexical tier (lexical.rs:45-51 lexical_admission: lexical_scope_kinds(Dart) is empty, lexical.rs:32-39, so None)
//! ```

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::session::merged::MergedGraph;
use std::collections::BTreeSet;
use strict_parity::{run, snapshot, Edit, Observed, Scenario, Snapshot};

const IMPORT_SCOPED: &[f32] = &[0.95];
const SAME_FILE: &[f32] = &[1.0];

/// What one caller must call: `(callee name, callee file, allowed confidences)`.
type Callees = Vec<(&'static str, String, &'static [f32])>;

/// One case: the tree, the caller file the overlay dirties, and, per caller,
/// what it must call in a fresh full index of the edited tree.
struct Case {
    name: String,
    files: Vec<(String, String)>,
    dirty: String,
    expected: Vec<(&'static str, Callees)>,
}

fn case(
    name: &str,
    files: &[(String, &str)],
    dirty: &str,
    expected: Vec<(&'static str, Callees)>,
) -> Case {
    Case {
        name: name.to_owned(),
        files: files
            .iter()
            .map(|(path, source)| (path.clone(), (*source).to_owned()))
            .collect(),
        dirty: dirty.to_owned(),
        expected,
    }
}

/// `helper` is declared inside `other`, out of reach of `run`: `run`'s call
/// is `Shadowed`. The name tiers skip, so `run` never binds the nested
/// function, nor the namesake `elsewhere` exports (a Tier 3 pick); the named
/// import still binds `lib` at 0.95.
fn named_import(ext: &str) -> Case {
    let lib = format!("lib.{ext}");
    let main = format!("main.{ext}");
    case(
        &format!("ladder_lexical_tail_{ext}_named_import"),
        &[
            (lib.clone(), "export function helper() {}\n"),
            (format!("elsewhere.{ext}"), "export function helper() {}\n"),
            (
                main.clone(),
                "import { helper } from \"./lib\";\n\nfunction other() {\n  function helper() {}\n  return helper;\n}\n\nfunction run() {\n  return helper();\n}\n",
            ),
        ],
        &main,
        vec![("run", vec![("helper", lib, IMPORT_SCOPED)])],
    )
}

/// As [`named_import`] with a default import: the import binds the default
/// export of `lib`.
fn default_import(ext: &str) -> Case {
    let lib = format!("lib.{ext}");
    let main = format!("main.{ext}");
    case(
        &format!("ladder_lexical_tail_{ext}_default_import"),
        &[
            (lib.clone(), "export default function helper() {}\n"),
            (format!("elsewhere.{ext}"), "export function helper() {}\n"),
            (
                main.clone(),
                "import helper from \"./lib\";\n\nfunction other() {\n  function helper() {}\n  return helper;\n}\n\nfunction run() {\n  return helper();\n}\n",
            ),
        ],
        &main,
        vec![("run", vec![("helper", lib, IMPORT_SCOPED)])],
    )
}

/// As [`named_import`] with a namespace import: `import * as helper` names a
/// module, no callable, so the call binds nothing, though `lib` and
/// `elsewhere` each export a `helper`.
fn namespace_import(ext: &str) -> Case {
    let main = format!("main.{ext}");
    case(
        &format!("ladder_lexical_tail_{ext}_namespace_import"),
        &[
            (format!("lib.{ext}"), "export function helper() {}\n"),
            (format!("elsewhere.{ext}"), "export function helper() {}\n"),
            (
                main.clone(),
                "import * as helper from \"./lib\";\n\nfunction other() {\n  function helper() {}\n  return helper;\n}\n\nfunction run() {\n  return helper();\n}\n",
            ),
        ],
        &main,
        vec![("run", vec![])],
    )
}

/// As [`named_import`] with no import: nothing binds `helper`. The namesake
/// `elsewhere` exports would bind at Tier 3 if the name tiers ran.
fn no_import(ext: &str) -> Case {
    let main = format!("main.{ext}");
    case(
        &format!("ladder_lexical_tail_{ext}_no_import"),
        &[
            (format!("elsewhere.{ext}"), "export function helper() {}\n"),
            (
                main.clone(),
                "function other() {\n  function helper() {}\n  return helper;\n}\n\nfunction run() {\n  return helper();\n}\n",
            ),
        ],
        &main,
        vec![("run", vec![])],
    )
}

/// The nested `function Widget` in `outer` is out of reach of `make`. The
/// callable import tier misses (a class is no callable), so the constructor
/// fallback runs through the import tiers alone and binds the imported
/// class's constructor at 0.95, never the nested function.
fn class(ext: &str) -> Case {
    let widget = format!("widget.{ext}");
    let main = format!("main.{ext}");
    case(
        &format!("ladder_lexical_tail_{ext}_class"),
        &[
            (widget.clone(), "export class Widget {\n  constructor() {}\n}\n"),
            (
                main.clone(),
                "import { Widget } from \"./widget\";\n\nfunction outer() {\n  function Widget() {}\n}\n\nfunction make() {\n  return new Widget();\n}\n",
            ),
        ],
        &main,
        vec![("make", vec![("constructor", widget, IMPORT_SCOPED)])],
    )
}

/// `c` sits two scopes deep (`a` > `b` > `c`); the only `helper` is declared in
/// `d`, a sibling branch out of reach. The call is `Shadowed` all the way
/// down: the import binds.
fn two_scopes_deep(ext: &str) -> Case {
    let lib = format!("lib.{ext}");
    let main = format!("main.{ext}");
    case(
        &format!("ladder_lexical_tail_{ext}_two_scopes_deep"),
        &[
            (lib.clone(), "export function helper() {}\n"),
            (
                main.clone(),
                "import { helper } from \"./lib\";\n\nfunction a() {\n  function b() {\n    function c() {\n      return helper();\n    }\n  }\n  function d() {\n    function helper() {}\n  }\n}\n",
            ),
        ],
        &main,
        vec![("c", vec![("helper", lib, IMPORT_SCOPED)])],
    )
}

/// Control: the nested `helper` is in reach of `run`, so the lexical verdict
/// is `Bound` at 1.0 and the import is never asked.
fn visible_function(ext: &str) -> Case {
    let main = format!("main.{ext}");
    case(
        &format!("ladder_lexical_tail_{ext}_visible_function"),
        &[
            (format!("lib.{ext}"), "export function helper() {}\n"),
            (
                main.clone(),
                "import { helper } from \"./lib\";\n\nfunction run() {\n  function helper() {}\n  return helper();\n}\n",
            ),
        ],
        &main,
        vec![("run", vec![("helper", main.clone(), SAME_FILE)])],
    )
}

/// Python, `ScopedOnly`. The nested `helper` is in reach of `run`: it binds
/// at 1.0, not the `lib` namesake the name tiers would pick at 0.7.
fn python_visible_nested() -> Case {
    case(
        "ladder_lexical_tail_python_visible_nested",
        &[
            ("lib.py".to_owned(), "def helper():\n    return 2\n"),
            (
                "main.py".to_owned(),
                "def run():\n    def helper():\n        return 1\n\n    return helper()\n",
            ),
        ],
        "main.py",
        vec![("run", vec![("helper", "main.py".to_owned(), SAME_FILE)])],
    )
}

/// Python, `ScopedOnly`. Two nested defs of `helper` in one scope are
/// ambiguous: `Shadowed`. The name tiers skip (`elsewhere`'s namesake stays
/// unbound) and the import-member tier binds `lib`.
fn python_ambiguous_nested_import() -> Case {
    case(
        "ladder_lexical_tail_python_ambiguous_nested_import",
        &[
            ("lib.py".to_owned(), "def helper():\n    return 3\n"),
            ("elsewhere.py".to_owned(), "def helper():\n    return 4\n"),
            (
                "main.py".to_owned(),
                "from lib import helper\n\n\ndef run():\n    def helper():\n        return 1\n\n    def helper():\n        return 2\n\n    return helper()\n",
            ),
        ],
        "main.py",
        vec![("run", vec![("helper", "lib.py".to_owned(), IMPORT_SCOPED)])],
    )
}

/// As [`python_ambiguous_nested_import`], but `pkg/__init__.py` re-exports
/// `helper` from `pkg/impl.py`: the index's import-member tier follows the
/// Python re-export; the overlay's ES named-import tier cannot.
fn python_ambiguous_nested_barrel() -> Case {
    case(
        "ladder_lexical_tail_python_ambiguous_nested_barrel",
        &[
            (
                "pkg/__init__.py".to_owned(),
                "from .impl import helper\n",
            ),
            ("pkg/impl.py".to_owned(), "def helper():\n    return 1\n"),
            ("elsewhere.py".to_owned(), "def helper():\n    return 4\n"),
            (
                "main.py".to_owned(),
                "from pkg import helper\n\n\ndef run():\n    def helper():\n        return 1\n\n    def helper():\n        return 2\n\n    return helper()\n",
            ),
        ],
        "main.py",
        vec![(
            "run",
            vec![("helper", "pkg/impl.py".to_owned(), IMPORT_SCOPED)],
        )],
    )
}

const CARGO: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// Rust, `ScopedOnly`. The nested `fn helper` is in reach of `run`: it binds
/// at 1.0, not `util`'s namesake at 0.7.
fn rust_visible_nested() -> Case {
    case(
        "ladder_lexical_tail_rust_visible_nested",
        &[
            ("Cargo.toml".to_owned(), CARGO),
            ("src/util.rs".to_owned(), "pub fn helper() {}\n"),
            (
                "src/main.rs".to_owned(),
                "fn run() {\n    fn helper() {}\n    helper();\n}\n\nfn main() {\n    run();\n}\n",
            ),
        ],
        "src/main.rs",
        vec![("run", vec![("helper", "src/main.rs".to_owned(), SAME_FILE)])],
    )
}

/// Rust, `ScopedOnly`. Two nested `fn helper` in one scope are ambiguous:
/// `Shadowed`. Parity only: both sides read the same `use` through the same
/// named-import rule.
fn rust_ambiguous_nested() -> Case {
    case(
        "ladder_lexical_tail_rust_ambiguous_nested",
        &[
            ("Cargo.toml".to_owned(), CARGO),
            ("src/util.rs".to_owned(), "pub fn helper() {}\n"),
            (
                "src/main.rs".to_owned(),
                "use crate::util::helper;\n\nfn run() {\n    fn helper() {}\n    fn helper() {}\n    helper();\n}\n\nfn main() {\n    run();\n}\n",
            ),
        ],
        "src/main.rs",
        Vec::new(),
    )
}

/// `name` of a snapshot node line `<kind> <file>:<line> <name> #<uid>`, with its file.
fn name_and_file(node: &str) -> (&str, &str) {
    let mut parts = node.split(' ');
    let (_kind, location, name) = (parts.next(), parts.next(), parts.next());
    let location = location.unwrap_or_default();
    let file = location.rsplit_once(':').map_or(location, |(file, _)| file);
    (name.unwrap_or_default(), file)
}

/// `(caller, callee, callee file, confidence)` of every `Calls` edge.
fn call_edges(snap: &Snapshot) -> Vec<(String, String, String, String)> {
    snap.edge
        .iter()
        .filter_map(|line| {
            let (source, target) = line.strip_prefix("Calls ")?.split_once(" -> ")?;
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
    let (_, source) = case
        .files
        .iter()
        .find(|(path, _)| *path == case.dirty)
        .expect("the dirty file is one of the files");
    let edited = format!("{source}\n");
    let files: Vec<(&str, &str)> = case
        .files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    run(&Scenario {
        name: &case.name,
        owner: "R13 (B8b lexical tail)",
        files: &files,
        edits: &[Edit::Write(&case.dirty, &edited)],
    })
}

/// The overlay's `Calls` lines that differ from the full index.
fn differing_calls(observed: &Observed) -> BTreeSet<String> {
    observed
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("edge ") && line.contains(" Calls "))
        .collect()
}

/// Hold the full index of `case`'s edited tree to `case.expected`, then the
/// overlay's `Calls` edges to the index's.
fn check(case: &Case) {
    let observed = observe_case(case);
    let calls = call_edges(&snapshot(&MergedGraph::new(observed.full(), None)));
    for (caller, callees) in &case.expected {
        let mut actual: Vec<(&str, &str, &str)> = calls
            .iter()
            .filter(|(source, ..)| source == caller)
            .map(|(_, callee, file, confidence)| {
                (callee.as_str(), file.as_str(), confidence.as_str())
            })
            .collect();
        actual.sort();
        let names: Vec<(&str, &str)> = actual.iter().map(|&(n, f, _)| (n, f)).collect();
        let mut want: Vec<(&str, &str)> =
            callees.iter().map(|(n, f, _)| (*n, f.as_str())).collect();
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
    let differing = differing_calls(&observed);
    assert!(
        differing.is_empty(),
        "{}: the overlay's Calls edges differ from the full index: {differing:#?}",
        case.name
    );
}

#[test]
fn test_ladder_lexical_tail_javascript_named_import_binds_import() {
    check(&named_import("js"));
}

#[test]
fn test_ladder_lexical_tail_typescript_named_import_binds_import() {
    check(&named_import("ts"));
}

#[test]
fn test_ladder_lexical_tail_javascript_default_import_binds_default_export() {
    check(&default_import("js"));
}

#[test]
fn test_ladder_lexical_tail_typescript_default_import_binds_default_export() {
    check(&default_import("ts"));
}

#[test]
fn test_ladder_lexical_tail_javascript_namespace_import_binds_nothing() {
    check(&namespace_import("js"));
}

#[test]
fn test_ladder_lexical_tail_typescript_namespace_import_binds_nothing() {
    check(&namespace_import("ts"));
}

#[test]
fn test_ladder_lexical_tail_javascript_no_import_binds_nothing() {
    check(&no_import("js"));
}

#[test]
fn test_ladder_lexical_tail_typescript_no_import_binds_nothing() {
    check(&no_import("ts"));
}

#[test]
fn test_ladder_lexical_tail_javascript_class_binds_constructor() {
    check(&class("js"));
}

#[test]
fn test_ladder_lexical_tail_typescript_class_binds_constructor() {
    check(&class("ts"));
}

#[test]
fn test_ladder_lexical_tail_javascript_two_scopes_deep_binds_import() {
    check(&two_scopes_deep("js"));
}

#[test]
fn test_ladder_lexical_tail_typescript_two_scopes_deep_binds_import() {
    check(&two_scopes_deep("ts"));
}

#[test]
fn test_ladder_lexical_tail_javascript_visible_function_binds_lexical() {
    check(&visible_function("js"));
}

#[test]
fn test_ladder_lexical_tail_typescript_visible_function_binds_lexical() {
    check(&visible_function("ts"));
}

#[test]
fn test_ladder_lexical_tail_python_visible_nested_binds_lexical() {
    check(&python_visible_nested());
}

#[test]
fn test_ladder_lexical_tail_python_ambiguous_nested_binds_import() {
    check(&python_ambiguous_nested_import());
}

/// The one case that moves an overlay edge: it fails on HEAD with one
/// `index-only` `Calls` line (the overlay's ES tier cannot follow the Python
/// package re-export) and passes once the overlay's import tiers are the
/// index's.
#[test]
fn test_ladder_lexical_tail_python_ambiguous_nested_barrel_binds_reexport() {
    check(&python_ambiguous_nested_barrel());
}

#[test]
fn test_ladder_lexical_tail_rust_visible_nested_binds_lexical() {
    check(&rust_visible_nested());
}

#[test]
fn test_ladder_lexical_tail_rust_ambiguous_nested_matches_index() {
    check(&rust_ambiguous_nested());
}
