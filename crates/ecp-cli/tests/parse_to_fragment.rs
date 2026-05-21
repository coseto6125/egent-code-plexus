//! Tests for `parse_to_fragment` / `fragments_from_local_graph`.
//!
//! The round-trip path under test:
//!   source bytes → `AnalyzerPipeline::parse_file_raw` → `LocalGraph`
//!   → `fragments_from_local_graph` → `Vec<Fragment>`
//!
//! Byte-span equality with full-reindex is guaranteed because both paths call
//! the same `LanguageProvider::parse_file` under the same pipeline.

use ecp_cli::reanalyze::make_pipeline;
use ecp_cli::session::overlay_writer::{
    fragments_from_local_graph, write_dirty_fragment, Fragment, FragmentInput,
};
use ecp_core::graph::NodeKind;
use ecp_core::session::{DirtyFiles, SessionMeta};
use std::path::Path;

// ── helpers ─────────────────────────────────────────────────────────────────

fn parse_fragments(filename: &str, src: &[u8]) -> Vec<Fragment> {
    let pipeline = make_pipeline();
    let graph = pipeline
        .parse_file_raw(Path::new(filename), src)
        .unwrap_or_else(|e| panic!("parse_file_raw({filename}) failed: {e}"));
    fragments_from_local_graph(&graph)
}

fn make_session_dir(tmp: &std::path::Path, sid: &str) -> std::path::PathBuf {
    let session_dir = tmp.join("sessions").join(sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-05-21T00:00:00Z".into(),
        last_touched: "2026-05-21T00:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    let df = DirtyFiles::empty();
    DirtyFiles::write_atomic(&session_dir.join("dirty_files.json"), &df).unwrap();
    session_dir
}

// ── core behavioural tests ───────────────────────────────────────────────────

#[test]
fn test_python_three_def_file() {
    // Three top-level functions at rows 0, 2, 4.
    let src = b"def foo():\n    pass\ndef bar():\n    pass\ndef baz():\n    pass\n";
    let frags = parse_fragments("module.py", src);

    let names: Vec<&str> = frags.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        frags.len(),
        3,
        "expected 3 fragments, got {}: {:?}",
        frags.len(),
        names
    );

    // Verify every fragment is a Function and has start_row matching its position.
    let expected = [("foo", 0u32), ("bar", 2u32), ("baz", 4u32)];
    for (name, expected_row) in expected {
        let f = frags
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("fragment '{name}' not found in {names:?}"));
        assert_eq!(
            f.kind,
            NodeKind::Function,
            "'{name}' should be Function, got {:?}",
            f.kind
        );
        assert_eq!(
            f.span.0, expected_row,
            "'{name}' start_row should be {expected_row}, got {}",
            f.span.0
        );
        // T0-2 vecs present (empty for now, filled by Phase 3 detectors).
        assert!(f.schema_fields.is_empty());
        assert!(f.event_topics.is_empty());
        assert!(f.tx_scopes.is_empty());
    }
}

#[test]
fn test_empty_file_empty_fragments() {
    // An empty Python file must produce zero fragments without error.
    let frags = parse_fragments("empty.py", b"");
    assert!(frags.is_empty(), "empty file must yield no fragments");
}

#[test]
fn test_syntax_error_returns_partial() {
    // One valid function followed by a broken def should not panic and
    // must surface at least the valid function.
    let src = b"def good():\n    pass\ndef (\n";
    let frags = parse_fragments("partial.py", src);
    let names: Vec<&str> = frags.iter().map(|f| f.name.as_str()).collect();
    assert!(
        frags.iter().any(|f| f.name == "good"),
        "partial parse must include the valid function; got {names:?}"
    );
}

/// Full pipeline path: `write_dirty_fragment` must not set `parse_failed`
/// for a recognised language and must produce a non-empty binary fragment.
#[test]
fn test_write_dirty_fragment_sets_parse_not_failed_for_known_lang() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "t7-1-sid");

    let outcome = write_dirty_fragment(
        &session_dir,
        &FragmentInput {
            rel_path: "src/lib.rs".into(),
            content: b"pub fn hello() {}".to_vec(),
            mtime_ns: 1,
        },
    )
    .unwrap();
    assert!(!outcome.parse_failed);

    // Fragment file must be non-empty (real content, not stub `vec![]`).
    let frag_path = session_dir
        .join("graph_overlay")
        .join(format!("{}.bin", outcome.fragment_id));
    let bytes = std::fs::read(&frag_path).unwrap();
    assert!(
        !bytes.is_empty(),
        "fragment file must contain archived data"
    );
}

// ── 14-lang coverage ─────────────────────────────────────────────────────────

/// Fixture: `(filename, source, expected_symbol_name, expected_kind)`
///
/// One representative symbol per language.  We assert start_row == 0 for all
/// (each fixture puts the target symbol on the first line) so the test is
/// purely structural — it validates that the pipeline routes to the right
/// provider and that `fragments_from_local_graph` surfaces the symbol, not
/// that we've memorised the exact end-column.
struct LangFixture {
    filename: &'static str,
    source: &'static [u8],
    expected_name: &'static str,
    expected_kind: NodeKind,
}

#[test]
fn test_14_lang_fragment_coverage() {
    let fixtures: &[LangFixture] = &[
        LangFixture {
            filename: "a.ts",
            source: b"export function tsFunc(): void {}",
            expected_name: "tsFunc",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.js",
            source: b"function jsFunc() {}",
            expected_name: "jsFunc",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.py",
            source: b"def py_func():\n    pass\n",
            expected_name: "py_func",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "A.java",
            source: b"public class A { public void javaMethod() {} }",
            expected_name: "javaMethod",
            expected_kind: NodeKind::Method,
        },
        LangFixture {
            filename: "A.kt",
            source: b"fun ktFunc() {}",
            expected_name: "ktFunc",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "A.cs",
            source: b"class A { void CsMethod() {} }",
            expected_name: "CsMethod",
            expected_kind: NodeKind::Method,
        },
        LangFixture {
            filename: "a.go",
            source: b"package main\nfunc GoFunc() {}",
            expected_name: "GoFunc",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.rs",
            source: b"pub fn rust_func() {}",
            expected_name: "rust_func",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.php",
            source: b"<?php function phpFunc() {}",
            expected_name: "phpFunc",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.rb",
            source: b"def ruby_method\nend\n",
            expected_name: "ruby_method",
            // Ruby: module-level `def` surfaces as Method (no class context
            // needed — Ruby's method-dispatch model means all defs are methods).
            expected_kind: NodeKind::Method,
        },
        LangFixture {
            filename: "a.swift",
            source: b"func swiftFunc() {}",
            expected_name: "swiftFunc",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.c",
            source: b"void c_func(void) {}",
            expected_name: "c_func",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.cpp",
            source: b"void cpp_func() {}",
            expected_name: "cpp_func",
            expected_kind: NodeKind::Function,
        },
        LangFixture {
            filename: "a.dart",
            source: b"void dartFunc() {}",
            expected_name: "dartFunc",
            expected_kind: NodeKind::Function,
        },
    ];

    let mut failed: Vec<String> = Vec::new();

    for fix in fixtures {
        let frags = parse_fragments(fix.filename, fix.source);
        let hit = frags.iter().find(|f| f.name == fix.expected_name);
        match hit {
            None => {
                let names: Vec<&str> = frags.iter().map(|f| f.name.as_str()).collect();
                failed.push(format!(
                    "{}: symbol '{}' not found; got {names:?}",
                    fix.filename, fix.expected_name
                ));
            }
            Some(f) => {
                if f.kind != fix.expected_kind {
                    failed.push(format!(
                        "{}: '{}' kind {:?} != expected {:?}",
                        fix.filename, fix.expected_name, f.kind, fix.expected_kind
                    ));
                }
                // Span consistency: start must be row 0 (symbol is on line 1).
                // Go fixtures add `package main\n` so GoFunc starts at row 1.
                let expected_row = if fix.filename == "a.go" { 1u32 } else { 0u32 };
                if f.span.0 != expected_row {
                    failed.push(format!(
                        "{}: '{}' start_row {} != {}",
                        fix.filename, fix.expected_name, f.span.0, expected_row
                    ));
                }
                // T0-2 vecs must be present (empty for now).
                if !f.schema_fields.is_empty()
                    || !f.event_topics.is_empty()
                    || !f.tx_scopes.is_empty()
                {
                    failed.push(format!(
                        "{}: T0-2 vecs should be empty at this phase",
                        fix.filename
                    ));
                }
            }
        }
    }

    assert!(
        failed.is_empty(),
        "14-lang coverage failures:\n{}",
        failed.join("\n")
    );
}
