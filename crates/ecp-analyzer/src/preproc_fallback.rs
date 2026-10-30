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

/// One physical source line, classified by a comment- and literal-aware
/// scan (block comments carry across lines).
struct SrcLine {
    row: u32,
    /// Exclusive; the `\n` is not part of the line.
    end: usize,
    /// The byte after the `#` of a directive that opens on this line.
    directive: Option<usize>,
    /// A trailing `\` on the line before splices this line onto it.
    spliced: bool,
    /// Only whitespace and comments.
    blank: bool,
}

/// The lines of `source`, plus the offset and direction (`true` = `{`) of
/// every brace outside comments, literals and directives.
fn scan_lines(source: &[u8]) -> (Vec<SrcLine>, Vec<(usize, bool)>) {
    let mut lines = Vec::new();
    let mut braces = Vec::new();
    let mut in_comment = false;
    let mut spliced = false;
    let mut prev_in_directive = false;
    let mut start = 0;
    let mut row = 0u32;
    loop {
        let end = source[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(source.len(), |i| start + i);
        let line = &source[start..end];
        let directive = (!spliced && !in_comment)
            .then(|| line.iter().position(|b| !b.is_ascii_whitespace()))
            .flatten()
            .filter(|&i| line[i] == b'#')
            .map(|i| start + i + 1);
        let in_directive = directive.is_some() || (spliced && prev_in_directive);
        let mut blank = true;
        let mut i = 0;
        while i < line.len() {
            let b = line[i];
            if in_comment {
                if b == b'*' && line.get(i + 1) == Some(&b'/') {
                    in_comment = false;
                    i += 1;
                }
                i += 1;
                continue;
            }
            match b {
                b'/' if line.get(i + 1) == Some(&b'/') => break,
                b'/' if line.get(i + 1) == Some(&b'*') => {
                    in_comment = true;
                    i += 1;
                }
                b'"' | b'\'' => {
                    blank = false;
                    i += 1;
                    while i < line.len() && line[i] != b {
                        i += if line[i] == b'\\' { 2 } else { 1 };
                    }
                }
                b'{' | b'}' => {
                    blank = false;
                    if !in_directive {
                        braces.push((start + i, b == b'{'));
                    }
                }
                _ if b.is_ascii_whitespace() => {}
                _ => blank = false,
            }
            i += 1;
        }
        lines.push(SrcLine {
            row,
            end,
            directive,
            spliced,
            blank,
        });
        let text = line.strip_suffix(b"\r").unwrap_or(line);
        spliced = text.ends_with(b"\\");
        prev_in_directive = in_directive;
        if end >= source.len() {
            break;
        }
        start = end + 1;
        row += 1;
    }
    (lines, braces)
}

/// `text` without its `/* */` and `//` comments, trimmed.
fn strip_comments(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        match (text[i], text.get(i + 1).copied()) {
            (b'/', Some(b'/')) => break,
            (b'/', Some(b'*')) => {
                i = text[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(text.len(), |p| i + 2 + p + 2);
                out.push(b' ');
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out.trim_ascii().to_vec()
}

/// The identifier `text` starts with, and the rest after it.
fn split_ident(text: &[u8]) -> Option<(&[u8], &[u8])> {
    let len = text
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        .unwrap_or(text.len());
    (len > 0 && !text[0].is_ascii_digit()).then(|| text.split_at(len))
}

/// The name an `#ifndef X` / `#if !defined(X)` / `#if !defined X` tests,
/// when that test is the whole condition.
fn guarded_name(word: &[u8], rest: &[u8]) -> Option<Vec<u8>> {
    let cond = strip_comments(rest);
    let name = match word {
        b"ifndef" => cond.as_slice(),
        b"if" => {
            let tail = cond.strip_prefix(b"!")?.trim_ascii_start();
            let tail = tail.strip_prefix(b"defined")?.trim_ascii();
            match tail.strip_prefix(b"(") {
                Some(inner) => inner.strip_suffix(b")")?.trim_ascii(),
                None => tail,
            }
        }
        _ => return None,
    };
    let (ident, after) = split_ident(name)?;
    after.is_empty().then(|| ident.to_vec())
}

/// Whether `#define <rest>` defines `name` with no value or `1`, and
/// `name` is spelt like a header guard. `#ifndef BUF_SIZE` / `#define
/// BUF_SIZE 4096` sets a default and `_GNU_SOURCE` a feature: no guards.
fn defines_guard(rest: &[u8], name: &[u8]) -> bool {
    let def = strip_comments(rest);
    let Some((ident, value)) = split_ident(&def) else {
        return false;
    };
    let keep = ident.iter().rposition(|&b| b != b'_').map_or(0, |i| i + 1);
    let trimmed = &ident[..keep];
    let header_like = [
        &b"_H"[..],
        b"_HPP",
        b"_HXX",
        b"_HH",
        b"_GUARD",
        b"_INCLUDED",
    ]
    .iter()
    .any(|suffix| trimmed.ends_with(suffix));
    let value = value.trim_ascii();
    ident == name && header_like && matches!(value, b"" | b"1")
}

/// The word after a directive's `#`, and the text after that word.
fn directive_word(source: &[u8], line: &SrcLine, after_hash: usize) -> (Vec<u8>, usize) {
    let text = &source[after_hash..line.end];
    let lead = text.len() - text.trim_ascii_start().len();
    let word: Vec<u8> = text[lead..]
        .iter()
        .take_while(|b| b.is_ascii_alphabetic())
        .copied()
        .collect();
    let rest = after_hash + lead + word.len();
    (word, rest)
}

/// Rows of the `#define X` lines that open a file-level include guard: an
/// `#ifndef X` / `#if !defined(X)` at conditional depth 0 whose next
/// non-blank line defines `X` with no value or `1`. The decision reads the
/// source text, so it holds however tree-sitter recovers the region; a
/// guard nested in another conditional is no file-level guard.
pub fn include_guard_rows(source: &[u8]) -> Vec<u32> {
    let (lines, _) = scan_lines(source);
    let mut rows = Vec::new();
    let mut depth = 0u32;
    let mut pending: Option<Vec<u8>> = None;
    for (k, line) in lines.iter().enumerate() {
        if line.spliced {
            continue;
        }
        let Some(after_hash) = line.directive else {
            if !line.blank {
                pending = None;
            }
            continue;
        };
        let open = pending.take();
        let continued = lines.get(k + 1).is_some_and(|next| next.spliced);
        let (word, rest) = directive_word(source, line, after_hash);
        let rest = &source[rest..line.end];
        match word.as_slice() {
            b"if" | b"ifdef" | b"ifndef" => {
                if depth == 0 && !continued {
                    pending = guarded_name(&word, rest);
                }
                depth += 1;
            }
            b"endif" => depth = depth.saturating_sub(1),
            b"define" if open.is_some_and(|name| !continued && defines_guard(rest, &name)) => {
                rows.push(line.row);
            }
            _ => {}
        }
    }
    rows
}

/// Brace depth after each counted `{` / `}` of `source`, by byte offset.
/// Only the first branch of each `#if` / `#elif` / `#else` chain counts, as
/// tree-sitter parses it, so branches that each open a brace do not
/// accumulate.
pub fn brace_depths(source: &[u8]) -> Vec<(usize, i32)> {
    let (lines, braces) = scan_lines(source);
    let mut out = Vec::with_capacity(braces.len());
    let mut braces = braces.into_iter().peekable();
    let mut branches: Vec<bool> = Vec::new();
    let mut depth = 0i32;
    for line in &lines {
        if let Some(after_hash) = line.directive {
            let (word, _) = directive_word(source, line, after_hash);
            match word.as_slice() {
                b"if" | b"ifdef" | b"ifndef" => branches.push(false),
                b"elif" | b"else" | b"elifdef" | b"elifndef" => {
                    if let Some(later) = branches.last_mut() {
                        *later = true;
                    }
                }
                b"endif" => {
                    branches.pop();
                }
                _ => {}
            }
        }
        let skipped = branches.iter().any(|&later| later);
        while let Some(&(at, open)) = braces.peek() {
            if at >= line.end {
                break;
            }
            braces.next();
            if !skipped {
                depth += if open { 1 } else { -1 };
                out.push((at, depth));
            }
        }
    }
    out
}

/// The brace depth at byte `offset`, from [`brace_depths`].
pub fn depth_at(depths: &[(usize, i32)], offset: usize) -> i32 {
    let before = depths.partition_point(|&(at, _)| at < offset);
    before.checked_sub(1).map_or(0, |i| depths[i].1)
}

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
        // A `#define` on the line after one ending in `\` is part of that
        // macro's body, not a definition.
        if continues_previous_line(source, full.start()) {
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

/// Whether the line that starts at `line_start` is spliced onto the line
/// before it by a trailing `\` (LF or CRLF).
fn continues_previous_line(source: &[u8], line_start: usize) -> bool {
    let before = &source[..line_start];
    let before = before.strip_suffix(b"\n").unwrap_or(before);
    let before = before.strip_suffix(b"\r").unwrap_or(before);
    before.ends_with(b"\\")
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
