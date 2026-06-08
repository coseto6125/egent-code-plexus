//! E2E: the orphan query (NOT EXISTS incoming Calls) finds uncalled functions
//! across all 14 mainstream languages. One test, sequential, fresh tempdir per
//! language (avoids parallel cold-index I/O storms on WSL2).

use serde_json::Value;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Initialise a minimal git repo, write `src/<name>.<ext>` with the given
/// source, commit, and run `ecp admin index`.
fn index_repo(repo: &std::path::Path, ext: &str, src: &str) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join(format!("src/fixture.{ext}")), src).unwrap();

    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_json(repo: &std::path::Path, args: &[&str]) -> Value {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return valid JSON: {err}\nstdout={stdout}"))
}

// (lang_label, file_extension, source_code)
// Each fixture: callee is called by caller; orphan is defined but never called.
// Java/C# use PascalCase method names to match their convention.
// Java/C# use a cross-class static call: intra-class bare-method-call Calls
// edges are a known parser blind spot (FU-2026-05-29-001), out of scope here.
// EXISTS is what's under test.
const CASES: &[(&str, &str, &str)] = &[
    (
        "typescript",
        "ts",
        "function callee(){}\nfunction caller(){callee();}\nfunction orphan(){}",
    ),
    (
        "javascript",
        "js",
        "function callee(){}\nfunction caller(){callee();}\nfunction orphan(){}",
    ),
    (
        "python",
        "py",
        "def callee():\n    pass\ndef caller():\n    callee()\ndef orphan():\n    pass",
    ),
    (
        "java",
        "java",
        "class B{ static void callee(){} }\nclass A{ void caller(){ B.callee(); } void orphan(){} }",
    ),
    (
        "kotlin",
        "kt",
        "fun callee(){}\nfun caller(){callee()}\nfun orphan(){}",
    ),
    (
        "csharp",
        "cs",
        "class B{ public static void Callee(){} }\nclass A{ void Caller(){ B.Callee(); } void Orphan(){} }",
    ),
    (
        "go",
        "go",
        "package m\nfunc callee(){}\nfunc caller(){callee()}\nfunc orphan(){}",
    ),
    (
        "rust",
        "rs",
        "fn callee(){}\nfn caller(){callee();}\nfn orphan(){}",
    ),
    (
        "php",
        "php",
        "<?php\nfunction callee(){}\nfunction caller(){callee();}\nfunction orphan(){}",
    ),
    (
        "ruby",
        "rb",
        "def callee\nend\ndef caller\n  callee()\nend\ndef orphan\nend",
    ),
    (
        "swift",
        "swift",
        "func callee(){}\nfunc caller(){callee()}\nfunc orphan(){}",
    ),
    (
        "c",
        "c",
        "void callee(){}\nvoid caller(){callee();}\nvoid orphan(){}",
    ),
    (
        "cpp",
        "cpp",
        "void callee(){}\nvoid caller(){callee();}\nvoid orphan(){}",
    ),
    (
        "dart",
        "dart",
        "void callee(){}\nvoid caller(){callee();}\nvoid orphan(){}",
    ),
];

#[test]
fn orphan_query_finds_uncalled_function_across_14_languages() {
    for (lang, ext, src) in CASES {
        let tmp = tempfile::tempdir().unwrap();
        index_repo(tmp.path(), ext, src);

        let out = run_json(
            tmp.path(),
            &[
                "cypher",
                "MATCH (f) WHERE NOT EXISTS((n)-[:Calls]->(f)) RETURN f.name",
                "--format",
                "json",
            ],
        );

        // Single-column RETURN produces rows as a flat array of scalars:
        // {"rows": ["caller", "orphan", "fixture.ts"]}.
        // Multi-column produces rows as an array of arrays.
        let names: Vec<String> = out["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{lang}: expected rows array, got {out}"))
            .iter()
            .filter_map(|r| {
                // flat scalar row
                r.as_str()
                    .map(String::from)
                    // nested array row (multi-column) — take first element
                    .or_else(|| r.get(0).and_then(|v| v.as_str()).map(String::from))
            })
            .collect();

        assert!(
            names.iter().any(|n| n == "orphan" || n == "Orphan"),
            "{lang}: expected 'orphan'/'Orphan' in {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "callee" || n == "Callee"),
            "{lang}: 'callee'/'Callee' has a caller — must NOT appear as orphan; got {names:?}"
        );
    }
}
