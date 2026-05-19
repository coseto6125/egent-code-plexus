//! Regression suite for the `#define NAME` regex fallback (PR #152
//! follow-up). tree-sitter-c (0.24.x) and tree-sitter-cpp (0.23.x)
//! ERROR-recover aggressively when they hit constructs the LR grammar
//! can't parse — multi-line `\` continuations + `##` token-paste,
//! `JEMALLOC_ALWAYS_INLINE`-style attribute macros stacked on function
//! declarations, deeply nested templates. The recovered ERROR nodes
//! preserve source tokens but drop the `preproc_def` wrapper, so the
//! tree-sitter query returns nothing for those regions.
//!
//! The fallback runs after the tree-sitter pass and emits Macro nodes
//! for any `#define NAME` not already captured. This file pins both
//! synthetic minimal cases (cheap unit checks of the dedup contract)
//! and real-file cases (the 3 macros that motivated the work).

use cgn_analyzer::c::parser::CProvider;
use cgn_analyzer::cpp::parser::CppProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::graph::NodeKind;
use std::path::{Path, PathBuf};

fn sample_repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .join(".sample_repo")
        .join(relative)
}

fn cpp_macros(src: &[u8]) -> Vec<String> {
    CppProvider::new()
        .unwrap()
        .parse_file(Path::new("t.cpp"), src)
        .unwrap()
        .nodes
        .into_iter()
        .filter(|n| n.kind == NodeKind::Macro)
        .map(|n| n.name)
        .collect()
}

fn c_macros(src: &[u8]) -> Vec<String> {
    CProvider::new()
        .unwrap()
        .parse_file(Path::new("t.c"), src)
        .unwrap()
        .nodes
        .into_iter()
        .filter(|n| n.kind == NodeKind::Macro)
        .map(|n| n.name)
        .collect()
}

#[test]
fn fallback_does_not_double_emit_when_tree_sitter_already_captured() {
    // tree-sitter captures `FOO` via the `preproc_def` query; the
    // fallback's dedup logic must prevent a second emission for the
    // same name.
    let src = b"#define FOO 1\n";
    let cpp = cpp_macros(src);
    let c = c_macros(src);
    assert_eq!(cpp.iter().filter(|n| *n == "FOO").count(), 1);
    assert_eq!(c.iter().filter(|n| *n == "FOO").count(), 1);
}

#[test]
fn fallback_does_not_emit_macros_inside_comments() {
    // The fallback's comment-mask must block `#define` examples that
    // appear inside docstring blocks (common in libraries that document
    // macro usage in `/** */` doxygen blocks).
    let src = b"/**\n * Example:\n * #define INSIDE_COMMENT 1\n */\n#define REAL 2\n";
    let cpp = cpp_macros(src);
    assert!(!cpp.iter().any(|n| n == "INSIDE_COMMENT"));
    assert!(cpp.iter().any(|n| n == "REAL"));
}

#[test]
fn jemalloc_tsdn_null_recovered_in_real_file() {
    // tsd.h ERROR-recovers around the `MALLOC_TEST_TSD` multi-line `\`
    // macro at line 43, wiping `preproc_def` for everything that follows
    // including `TSDN_NULL` at line 280. The fallback restores it.
    let bytes = std::fs::read(sample_repo_path(
        "C/deps/jemalloc/include/jemalloc/internal/tsd.h",
    ))
    .expect("sample_repo tsd.h missing — run scripts/parity bootstrap");
    let macros: Vec<String> = cpp_macros(&bytes);
    assert!(
        macros.iter().any(|n| n == "TSDN_NULL"),
        "TSDN_NULL must be emitted via fallback after tree-sitter ERROR recovery"
    );
}

#[test]
fn doctest_cmp_ge_recovered_in_real_file() {
    // doctest.h has `#define DOCTEST_CMP_GE` twice (lines 1487, 1494)
    // inside `#ifndef ... #else ... #endif` branches. The fallback
    // captures both occurrences from the raw source.
    let bytes = std::fs::read(sample_repo_path("Cpp/tests/thirdparty/doctest/doctest.h"))
    .expect("sample_repo doctest.h missing — run scripts/parity bootstrap");
    let macros: Vec<String> = cpp_macros(&bytes);
    let count = macros.iter().filter(|n| *n == "DOCTEST_CMP_GE").count();
    assert!(
        count >= 1,
        "DOCTEST_CMP_GE must surface at least once (found {count})"
    );
}

#[test]
fn jemalloc_ro_mutex_ctl_gen_recovered_in_real_file() {
    // ctl.c has the multi-line `\` continuation `#define RO_MUTEX_CTL_GEN`
    // at line 3581 deep inside a 3596-line file. tree-sitter-c
    // ERROR-recovers and drops the preproc_def wrapper; the regex
    // fallback walks raw bytes and captures the name regardless of
    // grammar state.
    let bytes = std::fs::read(sample_repo_path("C/deps/jemalloc/src/ctl.c"))
        .expect("sample_repo ctl.c missing — run scripts/parity bootstrap");
    let macros: Vec<String> = c_macros(&bytes);
    assert!(
        macros.iter().any(|n| n == "RO_MUTEX_CTL_GEN"),
        "RO_MUTEX_CTL_GEN must surface via fallback"
    );
}
