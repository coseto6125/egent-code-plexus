//! Recovery for C++ source that tree-sitter-cpp cannot parse.
//!
//! Two shapes drop class members' owners on real headers (nlohmann/json):
//!
//! - An attribute-like macro line before a declaration keyword
//!   (`NLOHMANN_BASIC_JSON_TPL_DECLARATION` / `class basic_json {`). The
//!   grammar reads the macro as a type and the class as a function body, so
//!   the class node and its members' owner vanish.
//!   [`blank_decl_prefix_macros`] overwrites such a macro name with blanks
//!   before the parse. No byte moves, so every span stays valid.
//! - Preprocessor conditionals inside a declaration (`#if X` in a
//!   constructor's initializer list) and access-specifier macros
//!   (`JSON_PRIVATE_UNLESS_TESTED:`). No source patch fixes every such
//!   case, so [`stamp_lexical_owners`] matches braces lexically, counting
//!   only the first branch of each conditional, and gives each member whose
//!   owner the tree lost the class whose body holds it.

use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use rustc_hash::FxHashMap;

/// Keywords an attribute-like macro line can precede.
const DECL_KEYWORDS: &[&[u8]] = &[
    b"class",
    b"struct",
    b"union",
    b"friend",
    b"template",
    b"enum",
];

/// A class-head lookahead stops after this many bytes; a real head is short.
const MAX_CLASS_HEAD: usize = 4096;

fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

fn ident_end(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && is_ident_byte(s[i]) {
        i += 1;
    }
    i
}

/// `FOO`, `JSON_API`: an all-caps name, which is never a keyword and in
/// practice never a type or variable at the head of a declaration.
fn is_macro_name(tok: &[u8]) -> bool {
    tok.len() >= 2
        && !tok[0].is_ascii_digit()
        && tok
            .iter()
            .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && tok.iter().any(u8::is_ascii_uppercase)
}

fn find(s: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    s.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

fn line_end(s: &[u8], from: usize) -> usize {
    s[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(s.len(), |p| from + p)
}

/// Skip whitespace and comments.
fn skip_trivia(s: &[u8], mut i: usize) -> usize {
    loop {
        while i < s.len() && s[i].is_ascii_whitespace() {
            i += 1;
        }
        if s[i..].starts_with(b"//") {
            i = line_end(s, i);
        } else if s[i..].starts_with(b"/*") {
            i = find(s, i + 2, b"*/").map_or(s.len(), |p| p + 2);
        } else {
            return i;
        }
    }
}

/// Skip whitespace, comments and whole preprocessor lines.
fn skip_trivia_and_directives(s: &[u8], mut i: usize) -> usize {
    loop {
        i = skip_trivia(s, i);
        if s.get(i) != Some(&b'#') {
            return i;
        }
        i = skip_directive(s, i);
    }
}

/// The index of the `\n` that ends the directive at `i`, past `\` line
/// continuations and block comments.
fn skip_directive(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        match s[i] {
            b'\n' => {
                let before = s[..i].strip_suffix(b"\r").unwrap_or(&s[..i]);
                if !before.ends_with(b"\\") {
                    return i;
                }
                i += 1;
            }
            b'/' if s.get(i + 1) == Some(&b'*') => {
                i = find(s, i + 2, b"*/").map_or(s.len(), |p| p + 2);
            }
            b'/' if s.get(i + 1) == Some(&b'/') => return line_end(s, i),
            _ => i += 1,
        }
    }
    s.len()
}

/// Whether the first word after `i`, past comments and other macro names,
/// is one of [`DECL_KEYWORDS`]. `memo` holds the answer at each position a
/// previous walk started from or passed a macro name at: a run of macro
/// lines shares one walk, so the scan of a file stays linear.
fn next_word_is_decl_keyword(s: &[u8], mut i: usize, memo: &mut FxHashMap<usize, bool>) -> bool {
    let mut passed: Vec<usize> = Vec::new();
    let answer = loop {
        if let Some(&known) = memo.get(&i) {
            break known;
        }
        passed.push(i);
        i = skip_trivia(s, i);
        let j = ident_end(s, i);
        if j == i {
            break false;
        }
        let word = &s[i..j];
        if !is_macro_name(word) {
            break DECL_KEYWORDS.contains(&word);
        }
        i = j;
    };
    memo.extend(passed.into_iter().map(|at| (at, answer)));
    answer
}

/// `source` with every macro name that opens a line and precedes a
/// declaration keyword overwritten by blanks; `None` when there is none.
/// The `#define` line itself and its continuation lines never qualify.
pub fn blank_decl_prefix_macros(source: &[u8]) -> Option<Vec<u8>> {
    let mut patched: Option<Vec<u8>> = None;
    let mut memo: FxHashMap<usize, bool> = FxHashMap::default();
    let mut line_start = 0;
    while line_start < source.len() {
        let end = line_end(source, line_start);
        let before = &source[..line_start.saturating_sub(1)];
        let continued = before
            .strip_suffix(b"\r")
            .unwrap_or(before)
            .ends_with(b"\\");
        let i = line_start
            + source[line_start..end]
                .iter()
                .take_while(|&&b| matches!(b, b' ' | b'\t'))
                .count();
        let j = ident_end(source, i);
        let qualifies = !continued
            && j > i
            && is_macro_name(&source[i..j])
            && source
                .get(j)
                .is_some_and(|&b| b.is_ascii_whitespace() || b == b'/')
            && next_word_is_decl_keyword(source, j, &mut memo);
        if qualifies {
            patched.get_or_insert_with(|| source.to_vec())[i..j].fill(b' ');
        }
        line_start = end + 1;
    }
    patched
}

/// The word after the `#` of the directive on the line `[start, end)`
/// (`if`, `ifdef`, `else`, ...); `None` when the line is no directive.
fn directive_word(s: &[u8], start: usize, end: usize) -> Option<&[u8]> {
    let blank = |from: usize| {
        from + s[from..end]
            .iter()
            .take_while(|&&b| matches!(b, b' ' | b'\t'))
            .count()
    };
    let hash = blank(start);
    if s.get(hash) != Some(&b'#') || hash >= end {
        return None;
    }
    let word = blank(hash + 1);
    Some(&s[word..ident_end(s, word).min(end)])
}

/// The last non-whitespace byte before `i`.
fn prev_non_space(s: &[u8], i: usize) -> Option<u8> {
    s[..i]
        .iter()
        .rev()
        .copied()
        .find(|b| !b.is_ascii_whitespace())
}

fn continues_previous_line(s: &[u8], line_start: usize) -> bool {
    let before = &s[..line_start.saturating_sub(1)];
    line_start > 0
        && before
            .strip_suffix(b"\r")
            .unwrap_or(before)
            .ends_with(b"\\")
}

/// Overwrite `[from, to)` with blanks, keeping line breaks so rows stay.
fn blank_range(buf: &mut [u8], from: usize, to: usize) {
    for b in &mut buf[from..to.min(buf.len())] {
        if !matches!(*b, b'\n' | b'\r') {
            *b = b' ';
        }
    }
}

const COND_OPEN: &[&[u8]] = &[b"if", b"ifdef", b"ifndef"];
const COND_NEXT: &[&[u8]] = &[b"else", b"elif", b"elifdef", b"elifndef"];

/// `source` with each conditional that opens a constructor's initializer
/// list (`Foo()` / `#if X` / `: a(1)` / `#else` / `: a(2)` / `#endif`)
/// reduced to its first branch: the directive lines and every later branch
/// become blanks. tree-sitter-cpp reads two `:` lists as the end of the
/// class, so the next method became a function spanning the class. `None`
/// when no conditional has that shape. No byte moves.
pub fn blank_init_list_conditionals(source: &[u8]) -> Option<Vec<u8>> {
    let mut patched: Option<Vec<u8>> = None;
    let mut line_start = 0;
    while line_start < source.len() {
        let end = line_end(source, line_start);
        let opens = directive_word(source, line_start, end).is_some_and(|w| COND_OPEN.contains(&w))
            && !continues_previous_line(source, line_start);
        if !opens {
            line_start = end + 1;
            continue;
        }
        let open_end = skip_directive(source, line_start);
        let first = skip_trivia(source, open_end);
        let opens_init_list = prev_non_space(source, line_start) == Some(b')')
            && source.get(first) == Some(&b':')
            && source.get(first + 1) != Some(&b':');
        if !opens_init_list {
            line_start = end + 1;
            continue;
        }
        let buf = patched.get_or_insert_with(|| source.to_vec());
        blank_range(buf, line_start, open_end);
        let (mut depth, mut later) = (0u32, false);
        let mut i = open_end + 1;
        while i < source.len() {
            let e = line_end(source, i);
            let Some(word) = directive_word(source, i, e) else {
                if later {
                    blank_range(buf, i, e);
                }
                i = e + 1;
                continue;
            };
            let directive_end = skip_directive(source, i);
            let closes = word == b"endif" && depth == 0;
            if COND_OPEN.contains(&word) {
                depth += 1;
            } else if word == b"endif" && depth > 0 {
                depth -= 1;
            } else if depth == 0 && COND_NEXT.contains(&word) {
                later = true;
            }
            if later || closes {
                blank_range(buf, i, directive_end);
            }
            i = directive_end + 1;
            if closes {
                break;
            }
        }
        line_start = i;
    }
    patched
}

/// The source patches C++ parses with, applied in order; `None` when none
/// applies. Each only overwrites bytes with blanks.
pub fn prepare_source(source: &[u8]) -> Option<Vec<u8>> {
    let macros = blank_decl_prefix_macros(source);
    blank_init_list_conditionals(macros.as_deref().unwrap_or(source)).or(macros)
}

/// The type name a base clause or qualifier names, generic arguments and a
/// leading `::` cut: `Proto<CharType>` → `Proto`, `::ns::Base<T>` →
/// `ns::Base`. The qualifier stays: the resolver's qualified tier matches it
/// against the owner (`detail::foo` → `foo` owned by `detail`). `None` for a
/// dependent name (`Base<T>::template rebind<U>`), `decltype(..)`, or text
/// that is not a name.
pub fn reduce_type_name(text: &str) -> Option<String> {
    let mut outer = String::with_capacity(text.len());
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => outer.push(c),
            _ => {}
        }
    }
    let dependent = outer
        .split(|c: char| c.is_whitespace() || c == ':')
        .any(|word| matches!(word, "template" | "typename"));
    if dependent {
        return None;
    }
    let name: String = outer.chars().filter(|c| !c.is_whitespace()).collect();
    let name = name.trim_start_matches("::");
    let valid = !name.is_empty()
        && name
            .split("::")
            .all(|seg| !seg.is_empty() && seg.bytes().all(is_ident_byte));
    valid.then(|| name.to_string())
}

/// One brace pair; `class` indexes [`LexicalScan::classes`] when the pair is
/// a class, struct or union body.
struct Region {
    open: usize,
    close: usize,
    parent: Option<usize>,
    class: Option<usize>,
}

struct LexicalClass {
    name: String,
    kind: NodeKind,
    head: usize,
    bases: Vec<String>,
}

struct LexicalScan {
    regions: Vec<Region>,
    classes: Vec<LexicalClass>,
}

/// One open `#if` group: `first_active` is false for `#if 0`, whose first
/// branch is dead code; `branch` counts the `#elif` / `#else` lines seen.
struct CondGroup {
    first_active: bool,
    branch: u32,
}

impl CondGroup {
    fn active(&self) -> bool {
        if self.first_active {
            self.branch == 0
        } else {
            self.branch == 1
        }
    }
}

fn skip_quoted(s: &[u8], from: usize, quote: u8) -> usize {
    let mut i = from + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 2,
            b'\n' => return i,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    s.len()
}

/// `R"delim( .. )delim"`, `from` at the opening quote.
fn skip_raw_string(s: &[u8], from: usize) -> usize {
    let Some(open) = s[from..]
        .iter()
        .take(18)
        .position(|&b| b == b'(')
        .map(|p| from + p)
    else {
        return from + 1;
    };
    let mut close = Vec::with_capacity(open - from + 1);
    close.push(b')');
    close.extend_from_slice(&s[from + 1..open]);
    close.push(b'"');
    find(s, open + 1, &close).map_or(s.len(), |p| p + close.len())
}

/// Skip a balanced `open` .. `close` run starting at `i` (at `open`).
fn skip_balanced(s: &[u8], mut i: usize, open: u8, close: u8, limit: usize) -> Option<usize> {
    let mut depth = 0u32;
    while i < limit {
        match s[i] {
            b if b == open => depth += 1,
            b if b == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// The bases of a base clause that starts after its `:` at `i`, and the
/// index of the body's `{`. `None` when a `;` or `}` comes first: then the
/// `:` was not a base clause.
fn base_clause(s: &[u8], mut i: usize, limit: usize) -> Option<(Vec<String>, usize)> {
    let mut bases = Vec::new();
    let mut depth = 0u32;
    let mut start = i;
    let push_base = |text: &[u8], bases: &mut Vec<String>| {
        let text = String::from_utf8_lossy(text);
        let mut words = text.trim();
        while let Some(rest) = ["public", "protected", "private", "virtual"]
            .iter()
            .find_map(|kw| {
                words
                    .strip_prefix(kw)
                    .filter(|r| r.starts_with(char::is_whitespace))
            })
        {
            words = rest.trim_start();
        }
        bases.extend(reduce_type_name(words));
    };
    while i < limit {
        match s[i] {
            b'/' if s.get(i + 1) == Some(&b'/') => {
                i = line_end(s, i);
                continue;
            }
            b'/' if s.get(i + 1) == Some(&b'*') => {
                i = find(s, i + 2, b"*/").map_or(s.len(), |p| p + 2);
                continue;
            }
            b'<' | b'(' => depth += 1,
            b'>' | b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                push_base(&s[start..i], &mut bases);
                start = i + 1;
            }
            b'{' if depth == 0 => {
                push_base(&s[start..i], &mut bases);
                return Some((bases, i));
            }
            b';' | b'}' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// The name, body `{` index and bases of the class head that follows a
/// `class` / `struct` / `union` keyword ending at `i`. The name is `None`
/// for an anonymous type. `None` when the keyword starts no definition
/// (`friend class X;`, `template<class T>`, `struct X* p`).
fn class_head(s: &[u8], mut i: usize) -> Option<(Option<String>, usize, Vec<String>)> {
    let limit = s.len().min(i + MAX_CLASS_HEAD);
    let mut name: Option<&[u8]> = None;
    loop {
        i = skip_trivia_and_directives(s, i);
        if i >= limit {
            return None;
        }
        match s[i] {
            b'{' => {
                let name = name.map(|n| String::from_utf8_lossy(n).into_owned());
                return Some((name, i, Vec::new()));
            }
            b':' if s.get(i + 1) == Some(&b':') => i += 2,
            b':' => {
                let (bases, open) = base_clause(s, i + 1, limit)?;
                let name = name.map(|n| String::from_utf8_lossy(n).into_owned());
                return Some((name, open, bases));
            }
            b'[' => i = skip_balanced(s, i, b'[', b']', limit)?,
            // Template specialization arguments: `struct hash<X> {`.
            b'<' if name.is_some() => i = skip_balanced(s, i, b'<', b'>', limit)?,
            b if is_ident_byte(b) && !b.is_ascii_digit() => {
                let j = ident_end(s, i);
                let after = skip_trivia(s, j);
                if s.get(after) == Some(&b'(') {
                    // `alignas(16)`, `__declspec(dllexport)`.
                    i = skip_balanced(s, after, b'(', b')', limit)?;
                } else {
                    let word = &s[i..j];
                    match name {
                        _ if word == b"final" => {}
                        None => name = Some(word),
                        // `class JSON_API Foo` / `class Foo FINAL_MACRO`.
                        Some(_) if is_macro_name(word) => {}
                        Some(prev) if is_macro_name(prev) => name = Some(word),
                        // `struct Foo foo{1};` declares a variable.
                        Some(_) => return None,
                    }
                    i = j;
                }
            }
            _ => return None,
        }
    }
}

/// Match braces over `s`, skipping comments, string and character literals,
/// preprocessor lines and every `#if` branch but the first live one.
fn scan(s: &[u8]) -> LexicalScan {
    let mut regions: Vec<Region> = Vec::new();
    let mut classes: Vec<LexicalClass> = Vec::new();
    let mut open_regions: Vec<usize> = Vec::new();
    let mut conds: Vec<CondGroup> = Vec::new();
    // The `{` that opens the body of the class head scanned last.
    let mut pending_body: Option<(usize, usize)> = None;
    let mut last_word: &[u8] = b"";
    let mut at_line_start = true;
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'\n' {
            at_line_start = true;
            i += 1;
            continue;
        }
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'#' && at_line_start {
            let k = skip_trivia(s, i + 1);
            let j = ident_end(s, k);
            match &s[k..j] {
                b"if" | b"ifdef" | b"ifndef" => {
                    let arg = skip_trivia(s, j);
                    let dead = &s[k..j] == b"if"
                        && s.get(arg) == Some(&b'0')
                        && !s.get(arg + 1).is_some_and(|&c| is_ident_byte(c));
                    conds.push(CondGroup {
                        first_active: !dead,
                        branch: 0,
                    });
                }
                b"elif" | b"elifdef" | b"elifndef" | b"else" => {
                    if let Some(group) = conds.last_mut() {
                        group.branch += 1;
                    }
                }
                b"endif" => {
                    conds.pop();
                }
                _ => {}
            }
            i = skip_directive(s, i);
            continue;
        }
        at_line_start = false;
        if s[i..].starts_with(b"//") {
            i = line_end(s, i);
            continue;
        }
        if s[i..].starts_with(b"/*") {
            i = find(s, i + 2, b"*/").map_or(s.len(), |p| p + 2);
            continue;
        }
        let active = conds.iter().all(CondGroup::active);
        match b {
            b'"' => i = skip_quoted(s, i, b'"'),
            b'\'' => i = skip_quoted(s, i, b'\''),
            b'0'..=b'9' => {
                // A number, digit separators (`1'000`) and exponents included.
                i += 1;
                while i < s.len() && (is_ident_byte(s[i]) || matches!(s[i], b'\'' | b'.')) {
                    i += 1;
                }
            }
            b'{' if active => {
                let class = pending_body
                    .take()
                    .filter(|&(open, _)| open == i)
                    .map(|(_, class)| class);
                regions.push(Region {
                    open: i,
                    close: s.len(),
                    parent: open_regions.last().copied(),
                    class,
                });
                open_regions.push(regions.len() - 1);
                i += 1;
            }
            b'}' if active => {
                if let Some(r) = open_regions.pop() {
                    regions[r].close = i;
                }
                i += 1;
            }
            _ if is_ident_byte(b) => {
                let j = ident_end(s, i);
                let word = &s[i..j];
                let raw_prefix = matches!(word, b"R" | b"LR" | b"uR" | b"UR" | b"u8R");
                if raw_prefix && s.get(j) == Some(&b'"') {
                    i = skip_raw_string(s, j);
                    continue;
                }
                if active && last_word != b"enum" {
                    let kind = match word {
                        b"class" => Some(NodeKind::Class),
                        b"struct" | b"union" => Some(NodeKind::Struct),
                        _ => None,
                    };
                    if let Some((name, open, bases)) = kind.and_then(|_| class_head(s, j)) {
                        if let (Some(kind), Some(name)) = (kind, name) {
                            classes.push(LexicalClass {
                                name,
                                kind,
                                head: i,
                                bases,
                            });
                            pending_body = Some((open, classes.len() - 1));
                        }
                    }
                }
                if active {
                    last_word = word;
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    LexicalScan { regions, classes }
}

fn line_starts(s: &[u8]) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            s.iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i + 1),
        )
        .collect()
}

fn byte_of(starts: &[usize], row: u32, col: u32) -> usize {
    starts
        .get(row as usize)
        .map_or(usize::MAX, |&l| l + col as usize)
}

fn row_col(starts: &[usize], byte: usize) -> (u32, u32) {
    let row = starts.partition_point(|&l| l <= byte).saturating_sub(1);
    (row as u32, (byte - starts[row]) as u32)
}

impl LexicalScan {
    /// The innermost brace pair strictly around `pos`.
    fn innermost(&self, pos: usize) -> Option<&Region> {
        let mut cur = self
            .regions
            .partition_point(|r| r.open < pos)
            .checked_sub(1);
        while let Some(r) = cur {
            let region = &self.regions[r];
            if pos < region.close {
                return Some(region);
            }
            cur = region.parent;
        }
        None
    }
}

/// Whether the word that ends just before `pos` is `word`.
fn preceded_by_word(s: &[u8], pos: usize, word: &[u8]) -> bool {
    let end = s[..pos.min(s.len())]
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |p| p + 1);
    s[..end].ends_with(word) && (end == word.len() || !is_ident_byte(s[end - word.len() - 1]))
}

/// Give each member the tree left without an owner the class whose body
/// holds it, and add the class itself when the tree lost it. A `Function`
/// directly in a class body becomes a `Method`; a `friend` stays a free
/// function. A class the tree cut short (an `#if` inside its body) gets its
/// lexical end. Returns the `(span, name)` of every class added, for
/// receiver typing.
///
/// Called only for a tree with errors: on a clean tree the span-based
/// stamping already gives every member its owner.
pub fn stamp_lexical_owners(
    source: &[u8],
    nodes: &mut Vec<RawNode>,
    is_exported: bool,
) -> Vec<((u32, u32, u32, u32), String)> {
    let scan = scan(source);
    if scan.classes.is_empty() {
        return Vec::new();
    }
    let starts = line_starts(source);
    let mut added = Vec::new();
    for region in &scan.regions {
        let Some(class) = region.class.map(|c| &scan.classes[c]) else {
            continue;
        };
        let (open_row, open_col) = row_col(&starts, region.open);
        let (head_row, head_col) = row_col(&starts, class.head);
        let (end_row, end_col) = row_col(&starts, region.close.min(source.len().saturating_sub(1)));
        let end_col = end_col + 1;
        let tree_node = nodes.iter_mut().find(|n| {
            matches!(n.kind, NodeKind::Class | NodeKind::Struct)
                && n.name == class.name
                && (n.span.0, n.span.1) <= (open_row, open_col)
                && (open_row, open_col) <= (n.span.2, n.span.3)
        });
        match tree_node {
            Some(node) => {
                if (node.span.2, node.span.3) < (end_row, end_col) {
                    node.span.2 = end_row;
                    node.span.3 = end_col;
                }
            }
            None => {
                let span = (head_row, head_col, end_row, end_col);
                let body_end = (region.close + 1).min(source.len());
                nodes.push(RawNode {
                    decorators: Vec::new(),
                    is_exported,
                    heritage: class.bases.clone(),
                    type_annotation: None,
                    name: class.name.clone(),
                    kind: class.kind,
                    span,
                    calls: Vec::new(),
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: ecp_core::uid::xxh3_64_bytes(&source[class.head..body_end]),
                });
                added.push((span, class.name.clone()));
            }
        }
    }
    for node in nodes.iter_mut() {
        if node.owner_class.is_some()
            || !matches!(
                node.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor | NodeKind::Property
            )
        {
            continue;
        }
        let pos = byte_of(&starts, node.span.0, node.span.1);
        // A lambda in a member initializer is no member.
        if node.name.starts_with("<anonymous")
            || pos >= source.len()
            || preceded_by_word(source, pos, b"friend")
            || source[pos..].starts_with(b"friend")
        {
            continue;
        }
        let Some(class) = scan
            .innermost(pos)
            .and_then(|r| r.class)
            .map(|c| &scan.classes[c])
        else {
            continue;
        };
        if node.kind == NodeKind::Function {
            node.kind = NodeKind::Method;
        }
        node.owner_class = Some(class.name.clone());
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blank_init_list_conditionals_keeps_first_branch_only() {
        let src = b"  Foo()\n#if A\n    : a(1)\n#else\n    : a(2)\n#endif\n  {}\n";
        let patched = blank_init_list_conditionals(src).expect("init-list conditional");
        assert_eq!(patched.len(), src.len());
        assert_eq!(
            patched,
            b"  Foo()\n     \n    : a(1)\n     \n          \n      \n  {}\n"
        );
    }

    #[test]
    fn test_blank_init_list_conditionals_other_conditionals_unpatched() {
        // A conditional after the first initializer, a base clause, and a
        // scope operator: none opens an initializer list.
        for src in [
            &b"  Foo()\n    : a(1)\n#if A\n    , b(2)\n#endif\n  {}\n"[..],
            b"class Foo\n#if A\n  : public Base\n#endif\n{};\n",
            b"  f(x)\n#if A\n  ::g();\n#endif\n",
        ] {
            assert_eq!(blank_init_list_conditionals(src), None, "{src:?}");
        }
    }

    #[test]
    fn test_blank_decl_prefix_macros_macro_before_class_blanks_macro() {
        let patched = blank_decl_prefix_macros(b"JSON_API\nclass X {};\n").unwrap();
        assert_eq!(patched, b"        \nclass X {};\n");
    }

    /// 50k macro lines: each line's lookahead walks the rest of the run,
    /// which a walk per line made quadratic.
    #[test]
    fn test_blank_decl_prefix_macros_long_macro_run_blanks_every_line() {
        let run = "FOO_BAR\n".repeat(50_000);
        let class = format!("{run}class X {{}};\n");
        let patched = blank_decl_prefix_macros(class.as_bytes()).unwrap();
        assert!(!patched.windows(7).any(|w| w == b"FOO_BAR"));
        assert_eq!(patched.len(), class.len());
        let other = format!("{run}int x;\n");
        assert_eq!(blank_decl_prefix_macros(other.as_bytes()), None);
    }
}
