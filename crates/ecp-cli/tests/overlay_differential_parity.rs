//! Differential parity over the 14 mainstream languages
//! (FU-2026-10-15-61d656576fc8): the `Calls` edges out of a dirty file are
//! the same in the session overlay as in a full reindex of the edited tree,
//! compared by file, kind, name and line, with the confidence exact.
//!
//! Each language gets the same shape: a caller that gains calls to a
//! same-file function, a function declared once elsewhere and a name two
//! files declare; Python, TypeScript and JavaScript also call through a
//! re-exporting module. Three dirty sets run per language: the caller
//! alone; the caller plus a file that gains a second `helper` (the unique
//! name turns ambiguous); the caller with every line below the first moved
//! down one.
//!
//! A pair that differs today is listed in `KNOWN_DIVERGENCES` with the FU
//! that tracks it, and asserted to still differ, so closing it turns the
//! test red until the entry goes. Nothing is skipped.

#[path = "common/parity.rs"]
mod parity;

use ecp_core::analyzer::types::LocalGraph;
use parity::calls_after_edit;
use std::path::Path;

/// `(language, dirty set, FU id)` pairs whose overlay differs from the
/// reindex today.
const KNOWN_DIVERGENCES: &[(&str, &str, &str)] = &[];

struct Fixture {
    language: &'static str,
    files: &'static [(&'static str, &'static str)],
    caller: (&'static str, &'static str),
    /// A file of `files` rewritten to declare a second `helper`.
    namesake: (&'static str, &'static str),
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        language: "TypeScript",
        files: &[
            ("src/def.ts", "export function helper() { return 1; }\nexport function shared() { return 1; }\n"),
            ("src/other.ts", "export function shared() { return 2; }\n"),
            ("src/imp.ts", "export function imported() { return 3; }\n"),
            ("src/index.ts", "export { imported } from './imp';\n"),
            ("src/app.ts", "import { imported } from './index';\nexport function entry() { return 0; }\nfunction local() { return 0; }\n"),
        ],
        caller: ("src/app.ts", "import { imported } from './index';\nexport function entry() {\n  local();\n  helper();\n  shared();\n  imported();\n}\nfunction local() { return 0; }\n"),
        namesake: ("src/other.ts", "export function shared() { return 2; }\nexport function helper() { return 2; }\n"),
    },
    Fixture {
        language: "JavaScript",
        files: &[
            ("src/def.js", "export function helper() { return 1; }\nexport function shared() { return 1; }\n"),
            ("src/other.js", "export function shared() { return 2; }\n"),
            ("src/imp.js", "export function imported() { return 3; }\n"),
            ("src/index.js", "export * from './imp';\n"),
            ("src/app.js", "import { imported } from './index';\nexport function entry() { return 0; }\nfunction local() { return 0; }\n"),
        ],
        caller: ("src/app.js", "import { imported } from './index';\nexport function entry() {\n  local();\n  helper();\n  shared();\n  imported();\n}\nfunction local() { return 0; }\n"),
        namesake: ("src/other.js", "export function shared() { return 2; }\nexport function helper() { return 2; }\n"),
    },
    Fixture {
        language: "Python",
        files: &[
            ("lib/def.py", "def helper():\n    return 1\n\n\ndef shared():\n    return 1\n"),
            ("lib/other.py", "def shared():\n    return 2\n"),
            ("pkg/__init__.py", "from .imp import imported\n"),
            ("pkg/imp.py", "def imported():\n    return 3\n"),
            ("app.py", "from pkg import imported\n\n\ndef entry():\n    return 0\n\n\ndef local():\n    return 0\n"),
        ],
        caller: ("app.py", "from pkg import imported\n\n\ndef entry():\n    local()\n    helper()\n    shared()\n    imported()\n\n\ndef local():\n    return 0\n"),
        namesake: ("lib/other.py", "def shared():\n    return 2\n\n\ndef helper():\n    return 2\n"),
    },
    Fixture {
        language: "Java",
        files: &[
            ("src/main/java/app/Def.java", "package app;\n\npublic class Def {\n    public static int helper() { return 1; }\n    public static int shared() { return 1; }\n}\n"),
            ("src/main/java/app/Other.java", "package app;\n\npublic class Other {\n    public static int shared() { return 2; }\n}\n"),
            ("src/main/java/app/Caller.java", "package app;\n\npublic class Caller {\n    void entry() {\n    }\n\n    void local() {\n    }\n}\n"),
        ],
        caller: ("src/main/java/app/Caller.java", "package app;\n\npublic class Caller {\n    void entry() {\n        local();\n        helper();\n        shared();\n    }\n\n    void local() {\n    }\n}\n"),
        namesake: ("src/main/java/app/Other.java", "package app;\n\npublic class Other {\n    public static int shared() { return 2; }\n    public static int helper() { return 2; }\n}\n"),
    },
    Fixture {
        language: "Kotlin",
        files: &[
            ("src/main/kotlin/app/Def.kt", "package app\n\nfun helper(): Int = 1\n\nfun shared(): Int = 1\n"),
            ("src/main/kotlin/app/Other.kt", "package app\n\nfun shared(): Int = 2\n"),
            ("src/main/kotlin/app/Caller.kt", "package app\n\nfun entry() {\n}\n\nfun local() {\n}\n"),
        ],
        caller: ("src/main/kotlin/app/Caller.kt", "package app\n\nfun entry() {\n    local()\n    helper()\n    shared()\n}\n\nfun local() {\n}\n"),
        namesake: ("src/main/kotlin/app/Other.kt", "package app\n\nfun shared(): Int = 2\n\nfun helper(): Int = 2\n"),
    },
    Fixture {
        language: "C#",
        files: &[
            ("src/Def.cs", "namespace App {\n    public static class Def {\n        public static int Helper() { return 1; }\n        public static int Shared() { return 1; }\n    }\n}\n"),
            ("src/Other.cs", "namespace App {\n    public static class Other {\n        public static int Shared() { return 2; }\n    }\n}\n"),
            ("src/Caller.cs", "namespace App {\n    public class Caller {\n        public void Entry() {\n        }\n\n        void Local() {\n        }\n    }\n}\n"),
        ],
        caller: ("src/Caller.cs", "namespace App {\n    public class Caller {\n        public void Entry() {\n            Local();\n            Helper();\n            Shared();\n        }\n\n        void Local() {\n        }\n    }\n}\n"),
        namesake: ("src/Other.cs", "namespace App {\n    public static class Other {\n        public static int Shared() { return 2; }\n        public static int Helper() { return 2; }\n    }\n}\n"),
    },
    Fixture {
        language: "Go",
        files: &[
            ("pkg/def.go", "package pkg\n\nfunc helper() int { return 1 }\n\nfunc shared() int { return 1 }\n"),
            ("other/other.go", "package other\n\nfunc shared() int { return 2 }\n"),
            ("pkg/caller.go", "package pkg\n\nfunc entry() {\n}\n\nfunc local() {\n}\n"),
        ],
        caller: ("pkg/caller.go", "package pkg\n\nfunc entry() {\n\tlocal()\n\thelper()\n\tshared()\n}\n\nfunc local() {\n}\n"),
        namesake: ("other/other.go", "package other\n\nfunc shared() int { return 2 }\n\nfunc helper() int { return 2 }\n"),
    },
    Fixture {
        language: "Rust",
        files: &[
            ("src/def.rs", "pub fn helper() -> i32 {\n    1\n}\n\npub fn shared() -> i32 {\n    1\n}\n"),
            ("src/other.rs", "pub fn shared() -> i32 {\n    2\n}\n"),
            ("src/caller.rs", "pub fn entry() {\n}\n\nfn local() {\n}\n"),
        ],
        caller: ("src/caller.rs", "pub fn entry() {\n    local();\n    helper();\n    shared();\n}\n\nfn local() {\n}\n"),
        namesake: ("src/other.rs", "pub fn shared() -> i32 {\n    2\n}\n\npub fn helper() -> i32 {\n    2\n}\n"),
    },
    Fixture {
        language: "PHP",
        files: &[
            ("src/Def.php", "<?php\n\nfunction helper() {\n    return 1;\n}\n\nfunction shared() {\n    return 1;\n}\n"),
            ("src/Other.php", "<?php\n\nfunction shared() {\n    return 2;\n}\n"),
            ("src/Caller.php", "<?php\n\nfunction entry() {\n}\n\nfunction local() {\n}\n"),
        ],
        caller: ("src/Caller.php", "<?php\n\nfunction entry() {\n    local();\n    helper();\n    shared();\n}\n\nfunction local() {\n}\n"),
        namesake: ("src/Other.php", "<?php\n\nfunction shared() {\n    return 2;\n}\n\nfunction helper() {\n    return 2;\n}\n"),
    },
    Fixture {
        language: "Ruby",
        files: &[
            ("lib/def.rb", "def helper\n  1\nend\n\ndef shared\n  1\nend\n"),
            ("lib/other.rb", "def shared\n  2\nend\n"),
            ("lib/caller.rb", "def entry\nend\n\ndef local\nend\n"),
        ],
        caller: ("lib/caller.rb", "def entry\n  local()\n  helper()\n  shared()\nend\n\ndef local\nend\n"),
        namesake: ("lib/other.rb", "def shared\n  2\nend\n\ndef helper\n  2\nend\n"),
    },
    Fixture {
        language: "Swift",
        files: &[
            ("Sources/App/Def.swift", "func helper() -> Int {\n    return 1\n}\n\nfunc shared() -> Int {\n    return 1\n}\n"),
            ("Sources/App/Other.swift", "func shared() -> Int {\n    return 2\n}\n"),
            ("Sources/App/Caller.swift", "func entry() {\n}\n\nfunc local() {\n}\n"),
        ],
        caller: ("Sources/App/Caller.swift", "func entry() {\n    local()\n    helper()\n    shared()\n}\n\nfunc local() {\n}\n"),
        namesake: ("Sources/App/Other.swift", "func shared() -> Int {\n    return 2\n}\n\nfunc helper() -> Int {\n    return 2\n}\n"),
    },
    Fixture {
        language: "C",
        files: &[
            ("src/def.c", "int helper(void) {\n    return 1;\n}\n\nint shared(void) {\n    return 1;\n}\n"),
            ("src/other.c", "int shared(void) {\n    return 2;\n}\n"),
            ("src/caller.c", "void entry(void) {\n}\n\nvoid local(void) {\n}\n"),
        ],
        caller: ("src/caller.c", "void entry(void) {\n    local();\n    helper();\n    shared();\n}\n\nvoid local(void) {\n}\n"),
        namesake: ("src/other.c", "int shared(void) {\n    return 2;\n}\n\nint helper(void) {\n    return 2;\n}\n"),
    },
    Fixture {
        language: "C++",
        files: &[
            ("src/def.cpp", "int helper() {\n    return 1;\n}\n\nint shared() {\n    return 1;\n}\n"),
            ("src/other.cpp", "int shared() {\n    return 2;\n}\n"),
            ("src/caller.cpp", "void entry() {\n}\n\nvoid local() {\n}\n"),
        ],
        caller: ("src/caller.cpp", "void entry() {\n    local();\n    helper();\n    shared();\n}\n\nvoid local() {\n}\n"),
        namesake: ("src/other.cpp", "int shared() {\n    return 2;\n}\n\nint helper() {\n    return 2;\n}\n"),
    },
    Fixture {
        language: "Dart",
        files: &[
            ("lib/def.dart", "int helper() {\n  return 1;\n}\n\nint shared() {\n  return 1;\n}\n"),
            ("lib/other.dart", "int shared() {\n  return 2;\n}\n"),
            ("lib/caller.dart", "void entry() {\n}\n\nvoid local() {\n}\n"),
        ],
        caller: ("lib/caller.dart", "void entry() {\n  local();\n  helper();\n  shared();\n}\n\nvoid local() {\n}\n"),
        namesake: ("lib/other.dart", "int shared() {\n  return 2;\n}\n\nint helper() {\n  return 2;\n}\n"),
    },
];

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

#[test]
fn test_overlay_dirty_caller_calls_match_reindex_across_14_languages() {
    assert_eq!(FIXTURES.len(), 14);
    let mut failures = Vec::new();
    for fixture in FIXTURES {
        let (caller_path, caller_source) = fixture.caller;
        let shifted_caller = shifted(caller_source);
        let dirty_sets: [(&str, Vec<(&str, &str)>); 3] = [
            ("caller", vec![fixture.caller]),
            ("caller+namesake", vec![fixture.caller, fixture.namesake]),
            (
                "caller shifted",
                vec![(caller_path, shifted_caller.as_str())],
            ),
        ];
        for (dirty_set, edits) in dirty_sets {
            let parity = calls_after_edit(&parse, fixture.files, &edits, &|file, _| {
                file == caller_path
            });
            let label = format!("{}/{dirty_set}", fixture.language);
            if parity.reindex.is_empty() {
                failures.push(format!(
                    "{label}: the reindex resolved no call, so the comparison proves nothing"
                ));
                continue;
            }
            let known = KNOWN_DIVERGENCES
                .iter()
                .find(|(language, set, _)| *language == fixture.language && *set == dirty_set);
            match (known, parity.overlay == parity.reindex) {
                (None, true) => {}
                (None, false) => failures.push(format!(
                    "{label}: overlay {:#?}\nreindex {:#?}",
                    parity.overlay, parity.reindex
                )),
                (Some((_, _, fu)), true) => failures.push(format!(
                    "{label}: known divergence {fu} is closed; remove it from KNOWN_DIVERGENCES"
                )),
                (Some(_), false) => {}
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
