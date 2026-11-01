//! Source patches C# parses with: conditional compilation that cuts a
//! statement or a brace pair apart. tree-sitter-c-sharp reads each `#if`
//! group as one block of whole statements; where a branch opens or closes a
//! brace the group does not (`#if X` / `if (a)` / `{` / `#else` / `a = b;` /
//! `#endif`), error recovery ends the enclosing class early and its later
//! members take the namespace as owner.

use crate::driver::blank_range;

/// A line-scan lexer state that outlives the line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lex {
    Code,
    Block,
    /// A verbatim string; `holes` when it is interpolated (`$@"` / `@$"`).
    Verbatim {
        holes: bool,
    },
    /// A raw string literal closed by this many quotes.
    Raw(usize),
}

/// What one line of code does to the brace depth and which word bounds it.
#[derive(Default)]
struct LineScan {
    delta: i32,
    /// The lowest depth reached on the line, relative to its start (`<= 0`).
    floor: i32,
    /// Whether the first / last token is a word that continues a statement
    /// (`else`, `catch`, `finally`); `None` when the line has no code.
    first_continues: Option<bool>,
    last_continues: Option<bool>,
}

fn note(out: &mut LineScan, continues: bool) {
    out.first_continues.get_or_insert(continues);
    out.last_continues = Some(continues);
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Counts the braces on `line` that sit in code, not in a string, a char
/// literal or a comment, and moves `lex` across the line.
fn scan_line(line: &[u8], lex: &mut Lex) -> LineScan {
    let mut out = LineScan::default();
    let mut i = 0;
    while i < line.len() {
        let end = match *lex {
            Lex::Code => None,
            Lex::Block => line[i..]
                .windows(2)
                .position(|w| w == b"*/")
                .map(|k| i + k + 2),
            Lex::Verbatim { holes } => skip_text(line, i, true, holes),
            Lex::Raw(quotes) => skip_raw(line, i, quotes),
        };
        if *lex != Lex::Code {
            let Some(k) = end else { return out };
            if *lex != Lex::Block {
                note(&mut out, false);
            }
            i = k;
            *lex = Lex::Code;
            continue;
        }
        match line[i] {
            b'/' if line.get(i + 1) == Some(&b'/') => break,
            b'/' if line.get(i + 1) == Some(&b'*') => {
                *lex = Lex::Block;
                i += 2;
                continue;
            }
            b'"' => {
                note(&mut out, false);
                match skip_string(line, i) {
                    Ok(k) => i = k,
                    Err(open) => {
                        *lex = open;
                        return out;
                    }
                }
                continue;
            }
            b'\'' => {
                i = skip_char(line, i);
                note(&mut out, false);
                continue;
            }
            b'{' => {
                out.delta += 1;
                note(&mut out, false);
            }
            b'}' => {
                out.delta -= 1;
                out.floor = out.floor.min(out.delta);
                note(&mut out, false);
            }
            b if is_word_byte(b) => {
                let start = i;
                while i < line.len() && is_word_byte(line[i]) {
                    i += 1;
                }
                note(
                    &mut out,
                    matches!(&line[start..i], b"else" | b"catch" | b"finally"),
                );
                continue;
            }
            b if b.is_ascii_whitespace() => {}
            _ => note(&mut out, false),
        }
        i += 1;
    }
    out
}

/// The index after the char literal that opens at `open`.
fn skip_char(line: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    while i < line.len() {
        match line[i] {
            b'\\' => i += 2,
            b'\'' => return i + 1,
            _ => i += 1,
        }
    }
    line.len()
}

/// The index after the string literal whose first quote is at `open`, read
/// with the `$` / `@` prefix before it. `Err` holds the state the line ends
/// in when the literal runs past it.
fn skip_string(line: &[u8], open: usize) -> Result<usize, Lex> {
    let (mut verbatim, mut holes) = (false, false);
    for &b in line[..open]
        .iter()
        .rev()
        .take_while(|&&b| b == b'$' || b == b'@')
    {
        verbatim |= b == b'@';
        holes |= b == b'$';
    }
    let quotes = line[open..].iter().take_while(|&&b| b == b'"').count();
    if !verbatim && quotes >= 3 {
        // A raw string's holes need no reading: no code in a hole holds the
        // closing quote run.
        return skip_raw(line, open + quotes, quotes).ok_or(Lex::Raw(quotes));
    }
    skip_text(line, open + 1, verbatim, holes).ok_or(if verbatim {
        Lex::Verbatim { holes }
    } else {
        Lex::Code
    })
}

/// The index after the closing quote of the regular or verbatim string text
/// that starts at `i`; `None` when the line ends first.
fn skip_text(line: &[u8], mut i: usize, verbatim: bool, holes: bool) -> Option<usize> {
    while i < line.len() {
        match line[i] {
            b'\\' if !verbatim => i += 2,
            b'"' if verbatim && line.get(i + 1) == Some(&b'"') => i += 2,
            b'"' => return Some(i + 1),
            b'{' if holes && line.get(i + 1) == Some(&b'{') => i += 2,
            b'{' if holes => i = skip_hole(line, i + 1)?,
            _ => i += 1,
        }
    }
    None
}

/// The index after the `}` that closes the interpolation hole whose code
/// starts at `i`; `None` when the line ends first.
fn skip_hole(line: &[u8], mut i: usize) -> Option<usize> {
    let mut depth = 0u32;
    while i < line.len() {
        match line[i] {
            b'"' => {
                i = skip_string(line, i).ok()?;
                continue;
            }
            b'\'' => {
                i = skip_char(line, i);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth > 0 => depth -= 1,
            b'}' => return Some(i + 1),
            b':' if line.get(i + 1) == Some(&b':') => i += 1,
            // A top-level `:` starts the format clause: text up to the brace.
            b':' if depth == 0 => {
                return line[i..].iter().position(|&b| b == b'}').map(|k| i + k + 1)
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index after the run of `quotes` quotes that closes a raw string whose
/// text goes on at `i`; `None` when the line ends first.
fn skip_raw(line: &[u8], i: usize, quotes: usize) -> Option<usize> {
    line[i..]
        .windows(quotes)
        .position(|w| w.iter().all(|&b| b == b'"'))
        .map(|k| i + k + quotes)
}

enum Directive {
    If,
    /// `#elif` or `#else`.
    Branch,
    Endif,
    /// `#region`, `#pragma`, `#define`, ...
    Other,
}

/// The directive word of `line` and the text after it; `None` when the line
/// is code.
fn directive_word(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = line
        .trim_ascii_start()
        .strip_prefix(b"#")?
        .trim_ascii_start();
    let word_len = rest.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    Some(rest.split_at(word_len))
}

/// The directive on `line`; `None` when the line is code.
fn directive(line: &[u8]) -> Option<Directive> {
    Some(match directive_word(line)?.0 {
        b"if" => Directive::If,
        b"elif" | b"else" => Directive::Branch,
        b"endif" => Directive::Endif,
        _ => Directive::Other,
    })
}

/// One branch of a conditional group: its directive line and the code up to
/// the next directive of the group.
struct Branch {
    directive: (usize, usize),
    body: (usize, usize),
    delta: i32,
    /// The lowest brace depth the body reaches, relative to its start.
    floor: i32,
    first_continues: bool,
    last_continues: bool,
}

impl Branch {
    fn new(directive: (usize, usize), source_len: usize) -> Self {
        let body_start = (directive.1 + 1).min(source_len);
        Self {
            directive,
            body: (body_start, body_start),
            delta: 0,
            floor: 0,
            first_continues: false,
            last_continues: false,
        }
    }

    fn add(&mut self, line: &LineScan, first_line: bool) {
        self.floor = self.floor.min(self.delta + line.floor);
        self.delta += line.delta;
        if first_line {
            self.first_continues = line.first_continues == Some(true);
        }
        if let Some(continues) = line.last_continues {
            self.last_continues = continues;
        }
    }
}

struct Group {
    branches: Vec<Branch>,
    endif: (usize, usize),
    /// Whether the innermost branch has seen a line of code.
    saw_code: bool,
    /// Whether the group sits in a branch of another group.
    nested: bool,
}

impl Group {
    fn current(&mut self) -> &mut Branch {
        self.branches.last_mut().expect("a group has a branch")
    }

    /// A group a parser cannot read as whole statements: a branch continues
    /// a statement from before the group (`else`, `catch`, `finally`) or ends
    /// in one, closes a brace opened before it, or leaves a brace open. Equal
    /// depths do not make a group whole: two branches that each open the same
    /// method body read as two half methods.
    fn splits_syntax(&self) -> bool {
        self.branches
            .iter()
            .any(|b| b.floor < 0 || b.delta != 0 || b.first_continues || b.last_continues)
    }

    /// Whether every branch leaves the brace depth as it found it, so keeping
    /// any one of them cannot unbalance the braces around the group.
    fn keeps_depth(&self) -> bool {
        self.branches.iter().all(|b| b.floor == 0 && b.delta == 0)
    }
}

/// The most settings of the tested symbols [`prepare_source`] tries.
const MAX_BRANCH_CHOICES: usize = 256;

/// One run of the source in order, as the brace walk of a setting reads it:
/// code outside every group as `(delta, floor)`, or the reduced group at
/// that index of the split groups.
enum Piece {
    Code(i32, i32),
    Group(usize),
}

/// `source` with every conditional group that splits syntax (see
/// [`Group::splits_syntax`]) reduced to the branch one setting of its
/// symbol selects: the group's directive lines and every other branch become
/// blanks. A group that tests one symbol (`#if X`, `#if !X`, an optional
/// `#else`) follows the value of that symbol, so groups on the same symbol
/// never mix branches no compilation keeps together; a group with a compound
/// condition or an `#elif` keeps its first branch. The first setting reads
/// each symbol as its first group's first branch does; the first setting
/// whose kept branches balance the braces stays. When none balances, only the
/// groups that keep the brace depth are reduced, each to its first branch.
/// `None` when no group is reduced. No byte moves.
pub fn prepare_source(source: &[u8]) -> Option<Vec<u8>> {
    if !source.contains(&b'#') {
        return None;
    }
    let mut lex = Lex::Code;
    let mut open: Vec<Group> = Vec::new();
    let mut split: Vec<Group> = Vec::new();
    let mut pieces: Vec<Piece> = Vec::new();
    let (mut delta, mut floor) = (0, 0);
    // Whether the pieces give every setting's brace walk: a split group in
    // another group moves the depth its parent recorded, and an unterminated
    // group's code is in no piece.
    let mut exact = true;
    let mut start = 0;
    while start < source.len() {
        let end = source[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(source.len(), |k| start + k);
        let line = &source[start..end];
        let found = (lex == Lex::Code).then(|| directive(line)).flatten();
        match found {
            Some(Directive::If) => {
                if open.is_empty() {
                    pieces.push(Piece::Code(delta, floor));
                    (delta, floor) = (0, 0);
                }
                open.push(Group {
                    branches: vec![Branch::new((start, end), source.len())],
                    endif: (end, end),
                    saw_code: false,
                    nested: !open.is_empty(),
                });
            }
            Some(Directive::Branch) => {
                if let Some(group) = open.last_mut() {
                    group.current().body.1 = start;
                    group.branches.push(Branch::new((start, end), source.len()));
                    group.saw_code = false;
                }
            }
            Some(Directive::Endif) => {
                if let Some(mut group) = open.pop() {
                    group.current().body.1 = start;
                    group.endif = (start, end);
                    // The enclosing branch sees the inner group as its first
                    // branch.
                    if let Some(parent) = open.last_mut() {
                        let kept = &group.branches[0];
                        let outer = parent.current();
                        outer.floor = outer.floor.min(outer.delta + kept.floor);
                        outer.delta += kept.delta;
                        parent.saw_code = true;
                    }
                    // A whole group outside every other leaves the depth as
                    // it found it in every branch: no piece.
                    if group.splits_syntax() {
                        if group.nested {
                            exact = false;
                        } else {
                            pieces.push(Piece::Group(split.len()));
                        }
                        split.push(group);
                    }
                }
            }
            Some(Directive::Other) => {}
            None => {
                let scan = scan_line(line, &mut lex);
                if let Some(group) = open.last_mut() {
                    let has_code = scan.first_continues.is_some();
                    let first_line = has_code && !group.saw_code;
                    group.saw_code |= has_code;
                    group.current().add(&scan, first_line);
                } else {
                    floor = floor.min(delta + scan.floor);
                    delta += scan.delta;
                }
            }
        }
        start = end + 1;
    }
    if split.is_empty() {
        return None;
    }
    exact &= open.is_empty();
    pieces.push(Piece::Code(delta, floor));
    consistent_branches(source, &split, exact.then_some(pieces.as_slice())).or_else(|| {
        split.iter().any(Group::keeps_depth).then(|| {
            let mut patched = source.to_vec();
            for group in split.iter().filter(|g| g.keeps_depth()) {
                reduce_group(&mut patched, group, Some(0));
            }
            patched
        })
    })
}

/// The symbol a group tests and the value of it that selects the group's
/// first branch: `(X, true)` for `#if X`, `(X, false)` for `#if !X`, either
/// with an optional `#else`. `None` for a compound condition, a literal
/// (`#if false`) or an `#elif`.
fn condition<'s>(source: &'s [u8], group: &Group) -> Option<(&'s [u8], bool)> {
    let [first, rest @ ..] = group.branches.as_slice() else {
        return None;
    };
    match rest {
        [] => {}
        [alt] if directive_word(&source[alt.directive.0..alt.directive.1])?.0 == b"else" => {}
        _ => return None,
    }
    let (_, mut expr) = directive_word(&source[first.directive.0..first.directive.1])?;
    if let Some(comment) = expr.windows(2).position(|w| w == b"//") {
        expr = &expr[..comment];
    }
    let mut value = true;
    loop {
        expr = expr.trim_ascii();
        if let Some(operand) = expr.strip_prefix(b"!") {
            value = !value;
            expr = operand;
        } else if let Some(inner) = expr.strip_prefix(b"(").and_then(|e| e.strip_suffix(b")")) {
            expr = inner;
        } else {
            break;
        }
    }
    let symbol = !expr.is_empty()
        && !expr[0].is_ascii_digit()
        && expr.iter().all(|&b| is_word_byte(b))
        && expr != b"true"
        && expr != b"false";
    symbol.then_some((expr, value))
}

/// `source` reduced to the first setting of the tested symbols whose kept
/// branches balance the braces (see [`prepare_source`]). `pieces`, when
/// given, holds every setting's brace walk, so only the winning setting is
/// blanked; without it each setting is blanked and scanned. `None` when no
/// setting balances, or when more than [`MAX_BRANCH_CHOICES`] settings move
/// the depth.
fn consistent_branches(
    source: &[u8],
    split: &[Group],
    pieces: Option<&[Piece]>,
) -> Option<Vec<u8>> {
    // Each symbol with the value its first group's first branch reads, and
    // its bit in a setting when a group that moves the depth tests it. A
    // symbol only depth-keeping groups test keeps that first value.
    let mut symbols: Vec<(&[u8], bool, Option<u32>)> = Vec::new();
    let mut moving = 0u32;
    let tests: Vec<Option<(usize, bool)>> = split
        .iter()
        .map(|group| {
            let (symbol, first) = condition(source, group)?;
            let at = symbols
                .iter()
                .position(|(s, _, _)| *s == symbol)
                .unwrap_or_else(|| {
                    symbols.push((symbol, first, None));
                    symbols.len() - 1
                });
            if !group.keeps_depth() && symbols[at].2.is_none() {
                symbols[at].2 = Some(moving);
                moving += 1;
            }
            Some((at, first))
        })
        .collect();
    let settings = 1usize
        .checked_shl(moving)
        .filter(|&n| n <= MAX_BRANCH_CHOICES)?;
    let mut picks: Vec<Option<usize>> = vec![Some(0); split.len()];
    let set_picks = |setting: usize, picks: &mut [Option<usize>]| {
        for ((pick, test), group) in picks.iter_mut().zip(&tests).zip(split) {
            let Some((at, first)) = *test else { continue };
            let (_, default, bit) = symbols[at];
            let value = default ^ bit.is_some_and(|bit| (setting >> bit) & 1 == 1);
            *pick = if value == first {
                Some(0)
            } else {
                (group.branches.len() > 1).then_some(1)
            };
        }
    };
    let blank = |patched: &mut [u8], picks: &[Option<usize>]| {
        for (group, &kept) in split.iter().zip(picks) {
            reduce_group(patched, group, kept);
        }
    };
    if let Some(pieces) = pieces {
        (0..settings).find(|&setting| {
            set_picks(setting, &mut picks);
            walk_balances(pieces, split, &picks)
        })?;
        let mut patched = source.to_vec();
        blank(&mut patched[..], &picks);
        return Some(patched);
    }
    let mut patched = source.to_vec();
    (0..settings).find_map(|setting| {
        set_picks(setting, &mut picks);
        patched.copy_from_slice(source);
        blank(&mut patched[..], &picks);
        braces_balance(&patched).then(|| std::mem::take(&mut patched))
    })
}

/// Whether the pieces, each group read as its kept branch, never close more
/// braces than they opened and end balanced: [`braces_balance`] of the
/// patched source, from the depths the scan recorded.
fn walk_balances(pieces: &[Piece], split: &[Group], picks: &[Option<usize>]) -> bool {
    let mut depth = 0;
    for piece in pieces {
        let (delta, floor) = match *piece {
            Piece::Code(delta, floor) => (delta, floor),
            Piece::Group(at) => picks[at].map_or((0, 0), |kept| {
                let branch = &split[at].branches[kept];
                (branch.delta, branch.floor)
            }),
        };
        if depth + floor < 0 {
            return false;
        }
        depth += delta;
    }
    depth == 0
}

/// Blanks the directive lines of `group` and every branch body but `kept`
/// (every body when `None`: no branch is selected).
fn reduce_group(patched: &mut [u8], group: &Group, kept: Option<usize>) {
    blank_range(patched, group.endif.0, group.endif.1);
    for (k, branch) in group.branches.iter().enumerate() {
        blank_range(patched, branch.directive.0, branch.directive.1);
        if Some(k) != kept {
            blank_range(patched, branch.body.0, branch.body.1);
        }
    }
}

/// Whether the code braces of `source` never close more than they opened
/// and end balanced. Directive lines are not code.
fn braces_balance(source: &[u8]) -> bool {
    let mut lex = Lex::Code;
    let mut depth = 0;
    for line in source.split(|&b| b == b'\n') {
        if lex == Lex::Code && directive(line).is_some() {
            continue;
        }
        let scan = scan_line(line, &mut lex);
        if depth + scan.floor < 0 {
            return false;
        }
        depth += scan.delta;
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::{prepare_source, scan_line, Lex};

    fn patched(src: &str) -> Option<String> {
        prepare_source(src.as_bytes()).map(|b| String::from_utf8(b).expect("ascii"))
    }

    fn delta(line: &str) -> i32 {
        let mut lex = Lex::Code;
        let scan = scan_line(line.as_bytes(), &mut lex);
        assert!(lex == Lex::Code, "{line:?} leaves a literal open");
        scan.delta
    }

    fn trimmed(out: &str) -> Vec<&str> {
        out.lines().map(str::trim).collect()
    }

    #[test]
    fn test_prepare_source_without_directive_is_none() {
        assert_eq!(patched("class C { }\n"), None);
        assert_eq!(patched(""), None);
    }

    #[test]
    fn test_prepare_source_balanced_groups_are_none() {
        assert_eq!(
            patched("#if X\nvoid A() { }\n#else\nvoid B() { }\n#endif\n"),
            None
        );
        assert_eq!(patched("#region R\nclass C { }\n#endregion\n"), None);
    }

    #[test]
    fn test_prepare_source_brace_count_ignores_strings_and_comments() {
        let src = "#if X\nFoo(\"{\"); // {\n#else\nFoo(@\"}\"); /* { */\n#endif\n";
        assert_eq!(patched(src), None);
        let multi = "#if X\nvar s = @\"\n{\n\";\n#else\nvar t = 1;\n#endif\n";
        assert_eq!(patched(multi), None);
    }

    #[test]
    fn test_prepare_source_uneven_branches_keep_first_branch_and_line_breaks() {
        let src = "#if X\nif (a)\n{\n#else\nAlt();\n#endif\nOne();\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(out.len(), src.len());
        assert_eq!(
            out.lines().map(str::trim).collect::<Vec<_>>(),
            ["", "if (a)", "{", "", "", "", "One();", "}"]
        );
    }

    #[test]
    fn test_prepare_source_branch_that_continues_else_loses_only_directives() {
        let src = "if (a) { }\n#if X\nelse if (b) { }\n#endif\nelse { }\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            out.lines().map(str::trim).collect::<Vec<_>>(),
            ["if (a) { }", "", "else if (b) { }", "", "else { }"]
        );
    }

    #[test]
    fn test_prepare_source_nested_uneven_group_leaves_balanced_outer_group_alone() {
        let src = "#if A\nvoid M() {\n#if B\nif (a)\n{\n#else\nAlt();\n#endif\nOne();\n}\n}\n#else\nvoid N() { }\n#endif\n";
        let out = patched(src).expect("patched");
        assert!(out.starts_with("#if A\n"), "{out:?}");
        assert!(out.ends_with("void N() { }\n#endif\n"), "{out:?}");
        assert!(!out.contains("#if B") && !out.contains("Alt();"), "{out:?}");
    }

    #[test]
    fn test_prepare_source_crlf_keeps_carriage_returns() {
        let src = "#if X\r\nif (a)\r\n{\r\n#else\r\nAlt();\r\n#endif\r\nOne();\r\n}\r\n";
        let out = patched(src).expect("patched");
        assert_eq!(out.matches("\r\n").count(), src.matches("\r\n").count());
        assert_eq!(out.len(), src.len());
    }
    /// A brace inside an interpolation hole's nested string is text: only
    /// the trailing code brace counts.
    #[test]
    fn test_scan_line_string_in_interpolation_hole_hides_its_brace() {
        assert_eq!(delta(r#"M($"{F("}")}"); {"#), 1);
        assert_eq!(delta(r#"M($"{F('}')}"); {"#), 1);
        assert_eq!(delta(r#"M($"{F($"{G("}")}")}"); {"#), 1);
    }

    /// `{{` / `}}` in an interpolated string are text, not a hole.
    #[test]
    fn test_scan_line_interpolation_escapes_are_text() {
        assert_eq!(delta(r#"x = $"{{"; }"#), -1);
        assert_eq!(delta(r#"x = $"}}"; {"#), 1);
        assert_eq!(delta(r#"x = $"{{{a}}}"; {"#), 1);
    }

    /// Both orders of the verbatim interpolated prefix read holes.
    #[test]
    fn test_scan_line_verbatim_interpolated_prefixes_read_holes() {
        assert_eq!(delta(r#"M($@"{F("}")}"); {"#), 1);
        assert_eq!(delta(r#"M(@$"{F("}")}"); {"#), 1);
        assert_eq!(delta(r#"M($@"a""{F("}")}""b"); {"#), 1);
    }

    /// A top-level `:` in a hole starts the format clause, text up to the
    /// closing brace; `::` and a `:` inside parentheses are code.
    #[test]
    fn test_scan_line_hole_colon_rules_find_the_closing_brace() {
        assert_eq!(delta(r#"x = $"{n:(#}"; {"#), 1);
        assert_eq!(delta(r#"x = $"{global::N.F("}")}"; {"#), 1);
        assert_eq!(delta(r#"x = $"{(a ? "}" : "{")}"; {"#), 1);
    }

    /// A verbatim interpolated string that spans lines keeps reading holes
    /// on the next line.
    #[test]
    fn test_scan_line_multiline_verbatim_interpolated_string_reads_holes() {
        let mut lex = Lex::Code;
        assert_eq!(scan_line(br#"x = $@""#, &mut lex).delta, 0);
        assert!(lex == Lex::Verbatim { holes: true });
        assert_eq!(scan_line(br#"{F("}")}"; {"#, &mut lex).delta, 1);
        assert!(lex == Lex::Code);
    }

    /// A raw string, plain or interpolated, ends at its quote run whatever
    /// braces it holds.
    #[test]
    fn test_scan_line_raw_strings_hide_their_braces() {
        assert_eq!(delta(r#"x = $"""{F("}")}"""; {"#), 1);
        assert_eq!(delta(r#"x = $$"""{ {{a}} }"""; {"#), 1);
        assert_eq!(delta(r#"x = """{"""; {"#), 1);
    }

    /// Plain literals keep their rules: an empty string, an escaped quote.
    /// An unterminated literal or hole ends at the line end.
    #[test]
    fn test_scan_line_plain_and_unterminated_literals() {
        assert_eq!(delta(r#"x = ""; {"#), 1);
        assert_eq!(delta(r#"x = "\"{"; {"#), 1);
        assert_eq!(delta(r#"x = $"{F("#), 0);
        assert_eq!(delta(r#"x = $"{"#), 0);
        assert_eq!(delta("x = $\"{F(\"}\")}\";\r"), 0);
        assert_eq!(delta("   "), 0);
        assert_eq!(delta(""), 0);
    }

    /// `catch` and `finally` continue a statement from before, as `else`
    /// does; a longer word that starts with one does not.
    #[test]
    fn test_scan_line_catch_and_finally_continue_a_statement() {
        let first = |line: &str| scan_line(line.as_bytes(), &mut Lex::Code).first_continues;
        assert_eq!(first("catch (IOException) { Baz(); }"), Some(true));
        assert_eq!(first("finally { }"), Some(true));
        assert_eq!(first("else"), Some(true));
        assert_eq!(first("try { }"), Some(false));
        assert_eq!(first("catchAll();"), Some(false));
        assert_eq!(first("   "), None);
    }

    /// The brief's case: an interpolated string with a brace in a hole's
    /// string leaves both branches whole, so no group is reduced.
    #[test]
    fn test_prepare_source_interpolated_hole_brace_keeps_both_branches() {
        let src = "class C {\n#if X\n    string A() => $\"{F(\"}\")}\";\n#else\n    void B() {}\n#endif\n    string F(string s) => s;\n}\n";
        assert_eq!(patched(src), None);
    }

    /// A group of `catch` clauses continues the `try` before it: it reads as
    /// its first branch.
    #[test]
    fn test_prepare_source_catch_group_keeps_first_branch() {
        let src = "try { Bar(); }\n#if X\ncatch (IOException) { Baz(); }\n#else\ncatch (Exception) { Qux(); }\n#endif\nfinally { }\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            trimmed(&out),
            [
                "try { Bar(); }",
                "",
                "catch (IOException) { Baz(); }",
                "",
                "",
                "",
                "finally { }"
            ]
        );
        let lone = "try { }\ncatch { }\n#if X\nfinally { Baz(); }\n#endif\n";
        let out = patched(lone).expect("patched");
        assert_eq!(
            trimmed(&out),
            ["try { }", "catch { }", "", "finally { Baz(); }", ""]
        );
    }

    /// Branches that each open the same body are not whole statements, even
    /// at equal depth: the group reads as its first branch.
    #[test]
    fn test_prepare_source_equal_open_branches_keep_first_branch() {
        let src = "#if X\nvoid F(int a) {\n#else\nvoid F(string a) {\n#endif\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(trimmed(&out), ["", "void F(int a) {", "", "", "", "}"]);
        let three = "#if A\nvoid F(int a) {\n#elif B\nvoid F(long a) {\n#else\nvoid F(string a) {\n#endif\n}\n";
        let out = patched(three).expect("patched");
        assert_eq!(
            trimmed(&out),
            ["", "void F(int a) {", "", "", "", "", "", "}"]
        );
    }

    /// CRLF line ends: the equal-branch group is reduced and every line
    /// break stays.
    #[test]
    fn test_prepare_source_equal_open_branches_crlf_keeps_line_breaks() {
        let src = "#if X\r\nvoid F(int a) {\r\n#else\r\nvoid F(string a) {\r\n#endif\r\n}\r\n";
        let out = patched(src).expect("patched");
        assert_eq!(out.len(), src.len());
        assert_eq!(out.matches("\r\n").count(), src.matches("\r\n").count());
        assert!(!out.contains("string a"), "{out:?}");
    }

    /// A one-branch group that opens a brace and a later one that closes it
    /// are reduced together: both lose only their directive lines.
    #[test]
    fn test_prepare_source_open_and_close_groups_are_reduced_together() {
        let src = "void M() {\n#if X\nlock (o) {\n#endif\nBar();\n#if X\n}\n#endif\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            trimmed(&out),
            [
                "void M() {",
                "",
                "lock (o) {",
                "",
                "Bar();",
                "",
                "}",
                "",
                "}"
            ]
        );
    }

    /// An inverted pair: `#if X` / `#else` opens a brace in its second
    /// branch, `#if !X` closes it. The first setting (X defined) keeps
    /// neither brace, which balances, so it stays.
    #[test]
    fn test_prepare_source_inverted_pair_reads_both_groups_with_one_setting() {
        let src = "void M() {\n#if X\n#else\nlock (o) {\n#endif\nBar();\n#if !X\n}\n#endif\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(out.len(), src.len());
        assert_eq!(
            trimmed(&out),
            ["void M() {", "", "", "", "", "Bar();", "", "", "", "}"]
        );
    }

    /// X defined keeps `lock (o) {` with `Two();`, which leaves a brace
    /// open; X undefined keeps `lock (p) {` with the `}` of `#if !X`. Before
    /// the per-symbol choice the first branches (`lock (o) {` with `}`)
    /// balanced and stayed, a pair no setting of X compiles.
    #[test]
    fn test_prepare_source_inverted_pair_takes_the_second_setting_when_the_first_unbalances() {
        let src = "void M() {\n#if X\nlock (o) {\n#else\nlock (p) {\n#endif\nBar();\n#if !X\n}\n#else\nTwo();\n#endif\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            trimmed(&out),
            [
                "void M() {",
                "",
                "",
                "",
                "lock (p) {",
                "",
                "Bar();",
                "",
                "}",
                "",
                "",
                "",
                "}"
            ]
        );
    }

    /// The same pair with the symbol spelled `(ÜBER) // on` and `! ÜBER`:
    /// both groups test one symbol, so they take the second setting too.
    #[test]
    fn test_prepare_source_parenthesized_commented_non_ascii_symbol_is_one_symbol() {
        let src = "void M() {\n#if (ÜBER) // on\nlock (o) {\n#else\nlock (p) {\n#endif\nBar();\n# if ! ÜBER\n}\n#else\nTwo();\n#endif\n}\n";
        let out = patched(src).expect("patched");
        assert!(
            out.contains("lock (p) {") && !out.contains("lock (o)") && !out.contains("Two"),
            "{out:?}"
        );
    }

    /// CRLF line ends: the second setting is kept and every line break
    /// stays.
    #[test]
    fn test_prepare_source_inverted_pair_crlf_keeps_line_breaks() {
        let src = "void M() {\r\n#if X\r\nlock (o) {\r\n#else\r\nlock (p) {\r\n#endif\r\nBar();\r\n#if !X\r\n}\r\n#else\r\nTwo();\r\n#endif\r\n}\r\n";
        let out = patched(src).expect("patched");
        assert_eq!(out.len(), src.len());
        assert_eq!(out.matches("\r\n").count(), src.matches("\r\n").count());
        assert!(
            out.contains("lock (p) {") && !out.contains("lock (o)") && !out.contains('#'),
            "{out:?}"
        );
    }

    /// The brief's first counterexample. `#if X` / `#else` opens `A` or
    /// `B`; a later `#if X` / `#else` opens a block only for X undefined,
    /// and `#if !X` closes it. X defined balances: `A` keeps `Common()` and
    /// no `OnlyB()`. The search by group index kept `A` with the `#else`
    /// block, an `A` calling `OnlyB` that no setting compiles.
    #[test]
    fn test_prepare_source_groups_on_one_symbol_take_the_same_setting() {
        let src = "class C {\n#if X\nvoid A() {\n#else\nvoid B() {\n#endif\n#if X\n#else\n{ OnlyB();\n#endif\nCommon();\n#if !X\n}\n#endif\n}\nvoid OnlyB() {}\nvoid Common() {}\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            trimmed(&out),
            [
                "class C {",
                "",
                "void A() {",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "Common();",
                "",
                "",
                "",
                "}",
                "void OnlyB() {}",
                "void Common() {}",
                "}"
            ]
        );
    }

    /// The brief's second counterexample: two independent inverted pairs on
    /// X and Y. Both defined balances: `A` closes at the `#if X` `}` and
    /// `Mid` stays a member. The search by group index kept `A` open with
    /// `A3()` and closed it at the `#if !Y` `}`, so `A` swallowed `Mid`.
    #[test]
    fn test_prepare_source_two_symbol_pairs_keep_the_unconditional_member_outside() {
        let src = "class C {\n#if X\nvoid A() {\n#else\nvoid A2() { }\n#endif\n#if X\n}\n#else\nvoid A3() { }\n#endif\nvoid Mid() { }\n#if Y\nvoid B2() { }\n#else\nvoid B() {\n#endif\n#if !Y\n}\n#else\nvoid B3() { }\n#endif\nvoid Tail() { }\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            trimmed(&out),
            [
                "class C {",
                "",
                "void A() {",
                "",
                "",
                "",
                "",
                "}",
                "",
                "",
                "",
                "void Mid() { }",
                "",
                "void B2() { }",
                "",
                "",
                "",
                "",
                "",
                "",
                "void B3() { }",
                "",
                "void Tail() { }",
                "}"
            ]
        );
    }

    /// A split group inside a whole `#if Z` group follows the same symbol as
    /// a group outside it: X undefined keeps `B` and the block. The outer
    /// group keeps its directives.
    #[test]
    fn test_prepare_source_nested_split_group_follows_the_outer_symbol_setting() {
        let src = "class C {\n#if X\nvoid A() {\n#else\nvoid B() {\n#endif\nCommon();\n#if Z\n#if X\nDone();\n#else\n{ OnlyB();\n#endif\n#endif\n}\n}\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            trimmed(&out),
            [
                "class C {",
                "",
                "",
                "",
                "void B() {",
                "",
                "Common();",
                "#if Z",
                "",
                "",
                "",
                "{ OnlyB();",
                "",
                "#endif",
                "}",
                "}",
                "}"
            ]
        );
    }

    /// A lone `#if X` that opens a brace nothing closes reads as X
    /// undefined: the group's body goes with its directives.
    #[test]
    fn test_prepare_source_lone_opener_reads_its_symbol_undefined() {
        let src = "void M() {\n#if X\nlock (o) {\n#endif\nBar();\n}\n";
        let out = patched(src).expect("patched");
        assert_eq!(trimmed(&out), ["void M() {", "", "", "", "Bar();", "}"]);
    }

    /// When no setting balances the braces (both branches open a brace
    /// nothing closes), neither branch is blanked.
    #[test]
    fn test_prepare_source_no_balancing_setting_blanks_neither_branch() {
        let src = "void M() {\n#if X\nlock (a) {\n#else\nlock (b) {\n#endif\nBar();\n}\n";
        assert_eq!(patched(src), None);
    }

    /// A compound condition names no one symbol: each group keeps its first
    /// branch, which leaves the pair unbalanced, so neither group is
    /// reduced. The search by group index paired the `#else` of the first
    /// with the second.
    #[test]
    fn test_prepare_source_compound_condition_pair_falls_back_to_neutral() {
        let src = "void M() {\n#if X && Y\n#else\nlock (o) {\n#endif\nBar();\n#if !(X && Y)\n}\n#endif\n}\n";
        assert_eq!(patched(src), None);
    }

    /// The inverted pair is reduced with one setting, and a group no brace
    /// depends on is reduced as well.
    #[test]
    fn test_prepare_source_inverted_pair_still_reduces_else_group() {
        let src = "void M() {\n#if X\n#else\nlock (o) {\n#endif\nBar();\n#if !X\n}\n#endif\nif (a) { }\n#if Y\nelse { }\n#endif\n}\n";
        let out = patched(src).expect("patched");
        assert!(!out.contains('#'), "{out:?}");
        assert!(
            !out.contains("lock (o) {") && out.contains("else { }"),
            "{out:?}"
        );
    }

    /// Past the search bound (nine symbols that move the depth) no setting
    /// is tried: only the groups no brace depends on are reduced.
    #[test]
    fn test_prepare_source_too_many_moving_groups_fall_back_to_neutral_groups() {
        let mut src = String::from("void M() {\n");
        for k in 0..9 {
            src.push_str(&format!(
                "#if X{k}\n#else\nlock (o) {{\n#endif\nBar();\n#if !X{k}\n}}\n#endif\n"
            ));
        }
        src.push_str("if (a) { }\n#if Y\nelse { }\n#endif\n}\n");
        let out = patched(&src).expect("patched");
        assert!(out.contains("#if X8") && out.contains("#if !X8"), "{out:?}");
        assert!(
            !out.contains("#if Y") && out.contains("else { }"),
            "{out:?}"
        );
    }

    /// `#endif` at the end of the file with no line break closes its group.
    /// An `#if` left open at the end keeps its directive, and the groups
    /// before it are still reduced.
    #[test]
    fn test_prepare_source_directive_at_eof_without_newline() {
        let src = "void M() {\n#if X\nlock (o) {\n#endif\nBar();\n}\n#if X\n}\n#endif";
        let out = patched(src).expect("patched");
        assert_eq!(out.len(), src.len());
        assert_eq!(
            trimmed(&out),
            [
                "void M() {",
                "",
                "lock (o) {",
                "",
                "Bar();",
                "}",
                "",
                "}",
                ""
            ]
        );
        let open = "void M() {\n#if X\nlock (o) {\n#endif\nBar();\n}\n#if X\n}\n#endif\n#if Y";
        let out = patched(open).expect("patched");
        assert!(out.ends_with("#if Y") && !out.contains("#if X"), "{out:?}");
    }

    /// A nested group that opens a body in each branch is reduced; the
    /// balanced outer group keeps its directives.
    #[test]
    fn test_prepare_source_nested_equal_open_group_leaves_outer_group_alone() {
        let src = "#if A\n#if B\nvoid F(int a) {\n#else\nvoid F(string a) {\n#endif\n}\n#endif\n";
        let out = patched(src).expect("patched");
        assert_eq!(
            trimmed(&out),
            ["#if A", "", "void F(int a) {", "", "", "", "}", "#endif"]
        );
    }

    /// Empty or whitespace-only branches and an unterminated group are no
    /// reason to patch.
    #[test]
    fn test_prepare_source_empty_and_unterminated_groups_are_none() {
        assert_eq!(patched("#if X\n#else\n#endif\n"), None);
        assert_eq!(patched("#if X\n   \n#else\n\t\n#endif\n"), None);
        assert_eq!(patched("#if X\nvoid F() {\n"), None);
    }
}
