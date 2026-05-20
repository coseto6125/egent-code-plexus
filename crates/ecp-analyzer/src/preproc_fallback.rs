//! Regex-based `#define NAME` fallback scanner for C/C++ parsers.
//!
//! tree-sitter-c (0.24.x) and tree-sitter-cpp (0.23.x) ERROR-recover
//! aggressively when they hit constructs the LR grammar can't parse —
//! multi-line `\` macro continuations with `##` token-paste, deeply nested
//! templates, `JEMALLOC_ALWAYS_INLINE`-style attribute macros stacked on
//! function declarations. The recovered ERROR nodes preserve the source
//! tokens but **drop the `preproc_def` wrapper**, so a `(preproc_def name:
//! (identifier))` query returns nothing for those regions.
//!
//! Verified on 2026-05-19 against `.sample_repo`: ecp emits 11 of 29
//! macros in `tsd.h`, 137 of 673 in `doctest.h`. The remaining 18 / 536
//! land inside ERROR regions despite being syntactically straightforward
//! `#define NAME body` lines.
//!
//! This module runs a lightweight regex pass over the raw source as a
//! safety net — only emits macros whose name was NOT already captured by
//! the tree-sitter query, so it's a strict augmentation, never a
//! replacement.
//!
//! Scope:
//! - Match `^[ \t]*#[ \t]*define[ \t]+IDENT` (handles `# define` spacing
//!   variants too)
//! - Skip `/* ... */` and `// ...` comments before the match
//! - Multi-line `\` continuation: just capture NAME, ignore body
//! - String / char literals: low-risk to ignore (`#define` inside a literal
//!   is rare enough that the FP cost is acceptable vs the recall gain)
//!
//! Returned `MacroHit`s carry (line_row, col_start, col_end) so the caller
//! can build a span. The caller is responsible for deduplication against
//! tree-sitter-emitted macro names.

use regex::bytes::Regex;
use std::sync::OnceLock;

/// `#define NAME` shape — anchored at line start (after optional indent),
/// permissive spacing around the `#`, captures identifier name.
fn define_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*#[ \t]*define[ \t]+([A-Za-z_][A-Za-z0-9_]*)")
            .expect("define regex compiles")
    })
}

#[derive(Debug, Clone)]
pub struct MacroHit {
    pub name: String,
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
}

/// Scan `source` for `#define NAME` matches, returning the name + position.
///
/// Pre-computes a comment-mask over the source so matches inside `/* */` /
/// `//` blocks are dropped (avoids extracting names from docstring-style
/// `#define` examples embedded in comments).
pub fn scan_define_macros(source: &[u8]) -> Vec<MacroHit> {
    let mask = build_comment_mask(source);
    let mut hits = Vec::new();
    for m in define_regex().captures_iter(source) {
        let full = m.get(0).unwrap();
        // Skip when the `#` itself sits inside a comment range.
        if mask[full.start()] {
            continue;
        }
        let name = m.get(1).unwrap();
        let bytes = &source[name.start()..name.end()];
        let Ok(name_str) = std::str::from_utf8(bytes) else {
            continue;
        };
        let (line, col_start) = byte_to_line_col(source, name.start());
        let col_end = col_start + (name.end() - name.start()) as u32;
        hits.push(MacroHit {
            name: name_str.to_string(),
            line,
            col_start,
            col_end,
        });
    }
    hits
}

/// Bitmap: byte `i` is `true` when inside a `/* */` or `//` comment.
/// Single allocation, O(n), no regex (cheaper than running 3 regexes in
/// parallel and union-ing matches).
fn build_comment_mask(source: &[u8]) -> Vec<bool> {
    let n = source.len();
    let mut mask = vec![false; n];
    let mut i = 0;
    while i < n {
        let b = source[i];
        // Line comment `// ...` → mask through end of line.
        if b == b'/' && i + 1 < n && source[i + 1] == b'/' {
            let start = i;
            i += 2;
            while i < n && source[i] != b'\n' {
                i += 1;
            }
            mask[start..i].fill(true);
            continue;
        }
        // Block comment `/* ... */` → mask through closing `*/`.
        if b == b'/' && i + 1 < n && source[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < n && !(source[i] == b'*' && source[i + 1] == b'/') {
                i += 1;
            }
            // Include the closing `*/` if present.
            if i + 1 < n {
                i += 2;
            }
            mask[start..i.min(n)].fill(true);
            continue;
        }
        // String literal `"..."` → mask interior, honor `\"` escapes.
        if b == b'"' {
            let start = i;
            i += 1;
            while i < n && source[i] != b'"' {
                if source[i] == b'\\' && i + 1 < n {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < n {
                i += 1;
            }
            mask[start..i.min(n)].fill(true);
            continue;
        }
        i += 1;
    }
    mask
}

/// Convert a 0-indexed byte offset into `(line, column)` (both 0-indexed).
/// Walks source from start each time — O(n) per call but only invoked once
/// per macro hit, and macro hits are linear in source size.
fn byte_to_line_col(source: &[u8], byte: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (i, &b) in source.iter().enumerate().take(byte) {
        if b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    (line, (byte - line_start) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_basic_define() {
        let hits = scan_define_macros(b"#define FOO 1\n");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "FOO");
        assert_eq!(hits[0].line, 0);
    }

    #[test]
    fn extracts_indented_and_spaced_define() {
        // `#define`, `# define`, `   #define`, `\t#  define` — all valid.
        let src = b"\t#define A 1\n   # define B 2\n#  define C 3\n";
        let hits = scan_define_macros(src);
        let names: Vec<_> = hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "C"]);
    }

    #[test]
    fn extracts_define_in_preproc_branches() {
        // tree-sitter handles this fine (probe confirmed) but the regex
        // also captures it — the fallback should not introduce duplicates
        // when paired with the tree-sitter pass.
        let src = b"#ifndef X\n#define A 1\n#else\n#define A 2\n#define B 3\n#endif\n";
        let names: Vec<_> = scan_define_macros(src)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, vec!["A", "A", "B"]);
    }

    #[test]
    fn extracts_multi_line_continuation_name_only() {
        // `#define M(args) body \` continuation — we only capture the name.
        let src = b"#define M(x, y) \\\n  foo(x) + \\\n  bar(y)\n#define N 1\n";
        let names: Vec<_> = scan_define_macros(src)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, vec!["M", "N"]);
    }

    #[test]
    fn skips_define_inside_block_comment() {
        // Common in docstring-style examples — must not surface as a
        // macro definition.
        let src = b"/* example:\n#define IGNORED 42\n*/\n#define REAL 1\n";
        let names: Vec<_> = scan_define_macros(src)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, vec!["REAL"]);
    }

    #[test]
    fn skips_define_inside_line_comment() {
        let src = b"// #define COMMENTED 1\n#define REAL 2\n";
        let names: Vec<_> = scan_define_macros(src)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, vec!["REAL"]);
    }

    #[test]
    fn skips_define_inside_string_literal() {
        // `puts("#define X 1");` — string content must not surface a macro.
        let src = b"puts(\"#define X 1\");\n#define Y 2\n";
        let names: Vec<_> = scan_define_macros(src)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, vec!["Y"]);
    }

    #[test]
    fn captures_jemalloc_tsdn_null_shape() {
        // The exact 2-line trigger that ERROR-recovers tree-sitter-cpp
        // when embedded in `.sample_repo/.../tsd.h`. Regex captures it
        // directly from source bytes regardless of grammar state.
        let src = b"struct tsdn_s {\n\ttsd_t tsd;\n};\n#define TSDN_NULL ((tsdn_t *)0)\n";
        let names: Vec<_> = scan_define_macros(src)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, vec!["TSDN_NULL"]);
    }

    #[test]
    fn position_tracks_line_and_column() {
        let src = b"\n\n#define HELLO 1\n";
        let hits = scan_define_macros(src);
        assert_eq!(hits[0].name, "HELLO");
        assert_eq!(hits[0].line, 2);
        // `#define ` (8 chars) before `HELLO`
        assert_eq!(hits[0].col_start, 8);
        assert_eq!(hits[0].col_end, 13);
    }
}
