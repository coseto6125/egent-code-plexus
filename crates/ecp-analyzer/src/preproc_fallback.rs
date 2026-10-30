//! Source-text preprocessor facts for the C/C++ parsers: facts that hold
//! however tree-sitter recovers the region around them.
//!
//! One lexer pass ([`scan_lines`]) classifies each line (directive, spliced
//! macro body, blank) and records every brace outside comments, literals
//! and directives. [`Preproc`] derives three facts from it, each on first
//! use:
//!
//! - **Rows** ([`PreprocRows`]): file-level include guards and the rows
//!   that belong to a directive.
//! - **Brace depth**: depth 0 is file scope. Only the first branch of each
//!   `#if` chain counts, and namespace / `extern "C"` bodies add no depth.
//! - **`#define` names** ([`MacroHit`]), the fallback for macros the query
//!   misses: tree-sitter-c (0.24.x) and tree-sitter-cpp (0.23.x)
//!   ERROR-recover aggressively on multi-line `\` macros with `##`, deeply
//!   nested templates and attribute macros stacked on declarations, and the
//!   recovered ERROR nodes **drop the `preproc_def` wrapper**. Verified on
//!   2026-05-19 against `.sample_repo`: ecp emitted 11 of 29 macros in
//!   `tsd.h`, 137 of 673 in `doctest.h`. The caller emits only names the
//!   query did not capture, so the fallback is a strict augmentation.

/// One physical source line, classified by a lexer that knows comments,
/// string, char and raw-string literals and digit separators (block
/// comments and raw strings carry across lines).
#[derive(Debug)]
struct SrcLine {
    row: u32,
    start: usize,
    /// Exclusive; the `\n` is not part of the line.
    end: usize,
    /// The byte after the `#` of a directive that opens on this line.
    directive: Option<usize>,
    /// A trailing `\` on the line before splices this line onto it.
    spliced: bool,
    /// The line belongs to a directive: it opens one, or a `\` splices it
    /// onto one (the body lines of a multi-line `#define`).
    in_directive: bool,
    /// Only whitespace and comments.
    blank: bool,
}

/// A `{` or `}` outside comments, literals and directives.
#[derive(Debug)]
struct Brace {
    at: usize,
    open: bool,
    /// The byte after the last `;` / `{` / `}` before this brace: where the
    /// code that heads the brace starts.
    head_from: usize,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The end of the preprocessing number that starts at the digit `i`:
/// `1'000`, `0xFF'FF`, `1.5e+3`. Its `'` separators open no char literal.
fn pp_number_end(line: &[u8], mut i: usize) -> usize {
    i += 1;
    while let Some(&b) = line.get(i) {
        let sign = matches!(b, b'+' | b'-') && matches!(line[i - 1], b'e' | b'E' | b'p' | b'P');
        let separator = b == b'\'' && line.get(i + 1).is_some_and(|&n| is_ident_byte(n));
        if !(is_ident_byte(b) || b == b'.' || sign || separator) {
            break;
        }
        i += 1;
    }
    i
}

/// For the `"` at `quote` that opens a raw string (`R"x(`, with an `L`,
/// `u`, `U` or `u8` prefix allowed), the offset after its `(` and the
/// `)x"` that closes it.
fn raw_string_open(line: &[u8], quote: usize) -> Option<(usize, Vec<u8>)> {
    let prefix = line[..quote]
        .iter()
        .rposition(|&b| !is_ident_byte(b))
        .map_or(0, |i| i + 1);
    if !matches!(&line[prefix..quote], b"R" | b"LR" | b"uR" | b"UR" | b"u8R") {
        return None;
    }
    let open = quote + 1 + line[quote + 1..].iter().take(17).position(|&b| b == b'(')?;
    let delimiter = &line[quote + 1..open];
    if delimiter
        .iter()
        .any(|&b| b.is_ascii_whitespace() || matches!(b, b')' | b'\\' | b'"'))
    {
        return None;
    }
    let mut close = Vec::with_capacity(delimiter.len() + 2);
    close.push(b')');
    close.extend_from_slice(delimiter);
    close.push(b'"');
    Some((open + 1, close))
}

/// The lines of `source` and its braces.
fn scan_lines(source: &[u8]) -> (Vec<SrcLine>, Vec<Brace>) {
    let mut lines = Vec::new();
    let mut braces = Vec::new();
    let mut in_comment = false;
    let mut raw_close: Option<Vec<u8>> = None;
    let mut spliced = false;
    let mut prev_in_directive = false;
    let mut head_from = 0;
    let mut start = 0;
    let mut row = 0u32;
    loop {
        let end = source[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(source.len(), |i| start + i);
        let line = &source[start..end];
        let directive = (!spliced && !in_comment && raw_close.is_none())
            .then(|| line.iter().position(|b| !b.is_ascii_whitespace()))
            .flatten()
            .filter(|&i| line[i] == b'#')
            .map(|i| start + i + 1);
        let in_directive = directive.is_some() || (spliced && prev_in_directive);
        let mut blank = true;
        let mut i = 0;
        if let Some(close) = &raw_close {
            blank = false;
            match find(line, close) {
                Some(p) => {
                    i = p + close.len();
                    raw_close = None;
                }
                None => i = line.len(),
            }
        }
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
            if let Some((body, close)) = (b == b'"').then(|| raw_string_open(line, i)).flatten() {
                blank = false;
                match find(&line[body..], &close) {
                    Some(p) => i = body + p + close.len(),
                    None => {
                        raw_close = Some(close);
                        i = line.len();
                    }
                }
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
                b'0'..=b'9' if i == 0 || !is_ident_byte(line[i - 1]) => {
                    blank = false;
                    i = pp_number_end(line, i);
                    continue;
                }
                b'{' | b'}' | b';' => {
                    blank = false;
                    if !in_directive {
                        if b != b';' {
                            braces.push(Brace {
                                at: start + i,
                                open: b == b'{',
                                head_from,
                            });
                        }
                        head_from = start + i + 1;
                    }
                }
                _ if b.is_ascii_whitespace() => {}
                _ => blank = false,
            }
            i += 1;
        }
        lines.push(SrcLine {
            row,
            start,
            end,
            directive,
            spliced,
            in_directive,
            blank,
        });
        let text = line.strip_suffix(b"\r").unwrap_or(line);
        // A raw string keeps its `\` line ends verbatim.
        spliced = raw_close.is_none() && text.ends_with(b"\\");
        prev_in_directive = in_directive;
        if end >= source.len() {
            break;
        }
        start = end + 1;
        row += 1;
    }
    (lines, braces)
}

/// `text` without its `/* */` and `//` comments, trimmed. A `//` comment
/// ends at the line end, so `text` may hold several lines.
fn strip_comments(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        match (text[i], text.get(i + 1).copied()) {
            (b'/', Some(b'/')) => {
                i = text[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(text.len(), |p| i + p);
            }
            (b'/', Some(b'*')) => {
                i = find(&text[i + 2..], b"*/").map_or(text.len(), |p| i + 2 + p + 2);
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
        .position(|&b| !is_ident_byte(b))
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
            let tail = tail.strip_prefix(b"defined")?;
            if tail.first().is_some_and(|&b| is_ident_byte(b)) {
                return None;
            }
            let tail = tail.trim_ascii();
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

/// When `#define <rest>` defines `name` with no value or `1` and `name` is
/// spelt like a header guard, whether the value is `1`. `#ifndef BUF_SIZE`
/// / `#define BUF_SIZE 4096` sets a default and `_GNU_SOURCE` a feature:
/// no guards.
fn defines_guard(rest: &[u8], name: &[u8]) -> Option<bool> {
    let def = strip_comments(rest);
    let (ident, value) = split_ident(&def)?;
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
    (ident == name && header_like && matches!(value, b"" | b"1")).then_some(value == b"1")
}

/// The word after a directive's `#`, and the offset of the text after it.
fn directive_word<'s>(source: &'s [u8], line: &SrcLine, after_hash: usize) -> (&'s [u8], usize) {
    let text = &source[after_hash..line.end];
    let lead = text.len() - text.trim_ascii_start().len();
    let len = text[lead..]
        .iter()
        .take_while(|b| b.is_ascii_alphabetic())
        .count();
    let from = after_hash + lead;
    (&source[from..from + len], from + len)
}

/// Preprocessor rows of one source.
#[derive(Debug, Default)]
pub struct PreprocRows {
    /// Rows of the `#define X` lines that open a file-level include guard,
    /// ascending: an `#ifndef X` / `#if !defined(X)` at conditional depth 0
    /// whose next non-blank line defines `X` with no value or `1`. A guard
    /// nested in another conditional is no file-level guard, and neither is
    /// a `#define X 1` whose group holds nothing else: it sets a default
    /// (`#ifndef HAVE_STDINT_H` / `#define HAVE_STDINT_H 1` / `#endif`).
    guards: Vec<u32>,
    /// Rows that belong to a directive, the spliced body lines of a
    /// multi-line `#define` included, ascending. No declaration starts on
    /// such a row, whatever tree-sitter makes of a macro body.
    directives: Vec<u32>,
}

impl PreprocRows {
    /// The 0-based rows of the include guards' `#define` lines.
    pub fn guards(&self) -> &[u32] {
        &self.guards
    }

    /// Whether the 0-based `row` holds an include guard's `#define`.
    pub fn is_guard_row(&self, row: usize) -> bool {
        u32::try_from(row).is_ok_and(|row| self.guards.binary_search(&row).is_ok())
    }

    /// Whether the 0-based `row` belongs to a directive.
    pub fn in_directive(&self, row: usize) -> bool {
        u32::try_from(row).is_ok_and(|row| self.directives.binary_search(&row).is_ok())
    }
}

fn preproc_rows(source: &[u8], lines: &[SrcLine]) -> PreprocRows {
    let mut guards = Vec::new();
    let directives = lines
        .iter()
        .filter(|line| line.in_directive)
        .map(|line| line.row)
        .collect();
    let mut depth = 0u32;
    let mut pending: Option<Vec<u8>> = None;
    // The last guard defines its name as `1` and its group has held nothing
    // else yet.
    let mut unconfirmed = false;
    for (k, line) in lines.iter().enumerate() {
        if line.spliced {
            continue;
        }
        let Some(after_hash) = line.directive else {
            if !line.blank {
                pending = None;
                unconfirmed = false;
            }
            continue;
        };
        let open = pending.take();
        let continued = lines.get(k + 1).is_some_and(|next| next.spliced);
        let (word, rest) = directive_word(source, line, after_hash);
        let rest = &source[rest..line.end];
        let mut guard = None;
        match word {
            b"if" | b"ifdef" | b"ifndef" => {
                if depth == 0 && !continued {
                    pending = guarded_name(word, rest);
                }
                depth += 1;
            }
            b"endif" => {
                depth = depth.saturating_sub(1);
                if depth == 0 && unconfirmed {
                    guards.pop();
                }
            }
            b"define" if !continued => {
                guard = open.and_then(|name| defines_guard(rest, &name));
                if guard.is_some() {
                    guards.push(line.row);
                }
            }
            _ => {}
        }
        unconfirmed = guard == Some(true);
    }
    PreprocRows { guards, directives }
}

/// Whether `text` ends with the whole word `word`.
fn ends_with_word(text: &[u8], word: &[u8]) -> bool {
    text.ends_with(word)
        && text[..text.len() - word.len()]
            .last()
            .is_none_or(|&b| !is_ident_byte(b))
}

/// Whether `text` holds the whole word `word`.
fn has_word(text: &[u8], word: &[u8]) -> bool {
    text.windows(word.len()).enumerate().any(|(i, w)| {
        w == word
            && (i == 0 || !is_ident_byte(text[i - 1]))
            && text.get(i + word.len()).is_none_or(|&b| !is_ident_byte(b))
    })
}

/// Whether the `{` at `open`, headed by the code from `from`, opens a
/// namespace or `extern "C"` body, whose declarations stay at file scope.
/// With comments and directive lines left out, the head holds the word
/// `namespace` (`namespace a::b`, `inline namespace v1`, `namespace std
/// _GLIBCXX_VISIBILITY(default)`) or ends with `extern "C"`.
fn opens_file_scope_block(source: &[u8], from: usize, open: usize) -> bool {
    let span = &source[from..open];
    if find(span, b"namespace").is_none() && find(span, b"extern").is_none() {
        return false;
    }
    let mut code = Vec::with_capacity(span.len());
    for line in span.split(|&b| b == b'\n') {
        if !line.trim_ascii_start().starts_with(b"#") {
            code.extend_from_slice(line);
            code.push(b'\n');
        }
    }
    let head = strip_comments(&code);
    if let Some(rest) = head
        .strip_suffix(b"\"C\"")
        .or_else(|| head.strip_suffix(b"\"C++\""))
    {
        return ends_with_word(rest.trim_ascii_end(), b"extern");
    }
    has_word(&head, b"namespace")
}

/// Brace depth after each counted brace, by byte offset. Only the first
/// branch of each `#if` / `#elif` / `#else` chain counts, as tree-sitter
/// parses it, so branches that each open a brace do not accumulate.
/// Namespace and `extern "C"` bodies add no depth: depth 0 is file scope.
fn brace_depths(source: &[u8], lines: &[SrcLine], braces: &[Brace]) -> Vec<(usize, i32)> {
    let mut out = Vec::with_capacity(braces.len());
    let mut braces = braces.iter().peekable();
    let mut branches: Vec<bool> = Vec::new();
    // Per open brace: whether it adds depth.
    let mut open_braces: Vec<bool> = Vec::new();
    let mut depth = 0i32;
    for line in lines {
        if let Some(after_hash) = line.directive {
            match directive_word(source, line, after_hash).0 {
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
        while let Some(brace) = braces.next_if(|brace| brace.at < line.end) {
            if skipped {
                continue;
            }
            if brace.open {
                let counts = !opens_file_scope_block(source, brace.head_from, brace.at);
                open_braces.push(counts);
                depth += i32::from(counts);
            } else if let Some(counts) = open_braces.pop() {
                // An unmatched `}` leaves the depth as it is: one brace
                // the scan missed must not push the rest of the file below
                // file scope.
                depth -= i32::from(counts);
            }
            out.push((brace.at, depth));
        }
    }
    out
}

/// The brace depth at byte `offset`, from [`Preproc::depths`].
pub fn depth_at(depths: &[(usize, i32)], offset: usize) -> i32 {
    let before = depths.partition_point(|&(at, _)| at < offset);
    before.checked_sub(1).map_or(0, |i| depths[i].1)
}

#[derive(Debug, Clone)]
pub struct MacroHit {
    pub name: String,
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
}

/// The `#define NAME` lines of `source` (`# define`, indented and spaced
/// forms included). A `#define` inside a comment, a literal or a spliced
/// macro body defines nothing.
fn define_hits(source: &[u8], lines: &[SrcLine]) -> Vec<MacroHit> {
    lines
        .iter()
        .filter_map(|line| {
            let (word, rest) = directive_word(source, line, line.directive?);
            if word != b"define" {
                return None;
            }
            let lead = source[rest..line.end]
                .iter()
                .take_while(|&&b| b == b' ' || b == b'\t')
                .count();
            if lead == 0 {
                return None;
            }
            let (name, _) = split_ident(&source[rest + lead..line.end])?;
            let col_start = (rest + lead - line.start) as u32;
            Some(MacroHit {
                name: String::from_utf8_lossy(name).into_owned(),
                line: line.row,
                col_start,
                col_end: col_start + name.len() as u32,
            })
        })
        .collect()
}

/// Each line of `source` as `(start, end, word)`: `word` is the word after
/// the `#` when a real directive opens on the line, never one inside a
/// comment, a literal or a spliced line.
pub(crate) fn directive_lines(source: &[u8]) -> Vec<(usize, usize, Option<&[u8]>)> {
    scan_lines(source)
        .0
        .iter()
        .map(|line| {
            let word = line
                .directive
                .map(|after_hash| directive_word(source, line, after_hash).0);
            (line.start, line.end, word)
        })
        .collect()
}

/// The text facts of one C/C++ source, each read on first use and kept for
/// the rest of its parse.
#[derive(Debug, Default)]
pub struct Preproc {
    scan: Option<(Vec<SrcLine>, Vec<Brace>)>,
    rows: Option<PreprocRows>,
    depths: Option<Vec<(usize, i32)>>,
    macros: Option<Vec<MacroHit>>,
}

impl Preproc {
    pub fn rows(&mut self, source: &[u8]) -> &PreprocRows {
        let Self { scan, rows, .. } = self;
        rows.get_or_insert_with(|| {
            let (lines, _) = scan.get_or_insert_with(|| scan_lines(source));
            preproc_rows(source, lines)
        })
    }

    pub fn depths(&mut self, source: &[u8]) -> &[(usize, i32)] {
        let Self { scan, depths, .. } = self;
        depths.get_or_insert_with(|| {
            let (lines, braces) = scan.get_or_insert_with(|| scan_lines(source));
            brace_depths(source, lines, braces)
        })
    }

    /// The brace depth at byte `offset`; 0 is file scope.
    pub fn depth_at(&mut self, source: &[u8], offset: usize) -> i32 {
        depth_at(self.depths(source), offset)
    }

    pub fn macros(&mut self, source: &[u8]) -> &[MacroHit] {
        let Self { scan, macros, .. } = self;
        macros.get_or_insert_with(|| {
            let (lines, _) = scan.get_or_insert_with(|| scan_lines(source));
            define_hits(source, lines)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_define_macros(source: &[u8]) -> Vec<MacroHit> {
        Preproc::default().macros(source).to_vec()
    }

    fn depth_after(source: &[u8], marker: &str) -> i32 {
        let at = find(source, marker.as_bytes()).expect("marker in source");
        Preproc::default().depth_at(source, at)
    }

    /// A `'` digit separator opens no char literal: the braces after it on
    /// the line still count, and the depth comes back to file scope.
    #[test]
    fn test_depth_at_digit_separator_keeps_braces_counted() {
        let src = b"void f(int n) {\n  if (n > 1'000) {\n    n++;\n  }\n}\nint after;\n";
        assert_eq!(depth_after(src, "n++"), 2);
        assert_eq!(depth_after(src, "int after"), 0);
        let hex = b"void g() { int m = 0xFF'FF; }\nint after;\n";
        assert_eq!(depth_after(hex, "int after"), 0);
    }

    /// A char literal still hides a brace: `'{'` adds no depth.
    #[test]
    fn test_depth_at_char_literal_brace_adds_no_depth() {
        let src = b"void f() { char c = '{'; char d = u8'}'; }\nint after;\n";
        assert_eq!(depth_after(src, "int after"), 0);
    }

    /// Braces and quotes inside a multi-line raw string are text, whatever
    /// its delimiter; a `\` line end inside it splices nothing.
    #[test]
    fn test_depth_at_multi_line_raw_string_braces_are_text() {
        let src = b"const char *k = R\"({\n  \"a\": 1}\n{)\";\nint after_raw;\n";
        assert_eq!(depth_after(src, "int after_raw"), 0);
        let delimited = b"auto j = u8R\"x(\n}} )\" \\\n)x\";\nint after;\n";
        assert_eq!(depth_after(delimited, "int after"), 0);
        let rows = Preproc::default()
            .rows(b"auto s = R\"(\n#define X 1\n)\";\n")
            .directives
            .clone();
        assert!(
            rows.is_empty(),
            "a `#` line inside a raw string is no directive: {rows:?}"
        );
    }

    /// An unmatched `}` leaves the rest of the file at file scope.
    #[test]
    fn test_depth_at_unmatched_close_brace_stays_at_file_scope() {
        let src = b"}\nint after;\nvoid f() {\n  int local;\n}\n";
        assert_eq!(depth_after(src, "int after"), 0);
        assert_eq!(depth_after(src, "int local"), 1);
    }

    /// Namespace heads with a macro, an attribute or a nested `inline`
    /// part, and heads whose comments hold a `;`, add no depth; a function
    /// body still does.
    #[test]
    fn test_depth_at_namespace_heads_add_no_depth() {
        for head in [
            "namespace std _GLIBCXX_VISIBILITY(default)\n",
            "namespace [[deprecated]] foo ",
            "namespace foo::inline bar ",
            "namespace n /* ; */ ",
            "namespace n // }\n",
            "inline namespace v1 ",
            "namespace ",
            "extern \"C\" ",
        ] {
            let src = format!("{head}{{\nint inside;\n}}\n");
            assert_eq!(depth_after(src.as_bytes(), "int inside"), 0, "{head:?}");
        }
        let src = b"void namespaced() {\nint inside;\n}\n";
        assert_eq!(depth_after(src, "int inside"), 1);
        let src = b"struct namespace_t {\nint inside;\n};\n";
        assert_eq!(depth_after(src, "int inside"), 1);
    }

    /// `#if !defined X` needs the whole word `defined`.
    #[test]
    fn test_guarded_name_defined_needs_word_boundary() {
        assert_eq!(
            guarded_name(b"if", b" !defined(FOO_H)"),
            Some(b"FOO_H".to_vec())
        );
        assert_eq!(
            guarded_name(b"if", b" !defined FOO_H"),
            Some(b"FOO_H".to_vec())
        );
        assert_eq!(guarded_name(b"if", b" !definedFOO_H"), None);
        assert_eq!(guarded_name(b"if", b" !defined_FOO_H"), None);
    }

    /// A `#define X 1` whose group holds nothing else sets a default, not
    /// a guard; a guard with a body, or with no value, stays a guard.
    #[test]
    fn test_preproc_rows_one_valued_define_alone_in_group_is_no_guard() {
        let src = b"#pragma once\n#ifndef HAVE_STDINT_H\n#define HAVE_STDINT_H 1\n#endif\n#ifdef HAVE_STDINT_H\nint y;\n#endif\n";
        assert!(Preproc::default().rows(src).guards().is_empty());
        let src = b"#ifndef A_H\n#define A_H 1\nint x;\n#endif\n";
        assert_eq!(Preproc::default().rows(src).guards(), [1]);
        let src = b"#ifndef A_H\n#define A_H\n#endif\n";
        assert_eq!(Preproc::default().rows(src).guards(), [1]);
        let src = b"#ifndef A_H\r\n#define A_H 1\r\n#include <x.h>\r\n#endif\r\n";
        assert_eq!(Preproc::default().rows(src).guards(), [1]);
    }

    /// A `'"'` char literal opens no string that hides a later `#define`.
    #[test]
    fn test_define_hits_after_quote_char_literal_are_found() {
        let src = b"static const char kQuote = '\"';\n#define ATTR_PRINTF(i, c) x\nconst char *m = \"done\";\n";
        let names: Vec<_> = scan_define_macros(src)
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, ["ATTR_PRINTF"]);
    }

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
