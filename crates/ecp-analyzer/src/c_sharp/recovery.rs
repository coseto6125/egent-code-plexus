//! Source patches C# parses with: conditional compilation that cuts a
//! statement or a brace pair apart. tree-sitter-c-sharp reads each `#if`
//! group as one block of whole statements; where a branch opens or closes a
//! brace the group does not (`#if X` / `if (a)` / `{` / `#else` / `a = b;` /
//! `#endif`), error recovery ends the enclosing class early and its later
//! members take the namespace as owner.

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

/// The directive on `line`; `None` when the line is code.
fn directive(line: &[u8]) -> Option<Directive> {
    let rest = line
        .trim_ascii_start()
        .strip_prefix(b"#")?
        .trim_ascii_start();
    let word_len = rest.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    Some(match &rest[..word_len] {
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

/// Overwrite `[from, to)` with blanks, keeping line breaks so rows stay.
fn blank_range(buf: &mut [u8], from: usize, to: usize) {
    let to = to.min(buf.len());
    for b in &mut buf[from..to] {
        if !matches!(*b, b'\n' | b'\r') {
            *b = b' ';
        }
    }
}

/// `source` with every conditional group that splits syntax (see
/// [`Group::splits_syntax`]) reduced to its first branch: the group's
/// directive lines and every later branch become blanks, as if the first
/// branch's symbol were the only one defined. When the first branches leave
/// the braces unbalanced, only the groups that keep the brace depth are
/// reduced. `None` when no group is reduced. No byte moves.
pub fn prepare_source(source: &[u8]) -> Option<Vec<u8>> {
    if !source.contains(&b'#') {
        return None;
    }
    let mut lex = Lex::Code;
    let mut open: Vec<Group> = Vec::new();
    let mut split: Vec<Group> = Vec::new();
    let mut start = 0;
    while start < source.len() {
        let end = source[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(source.len(), |k| start + k);
        let line = &source[start..end];
        let found = (lex == Lex::Code).then(|| directive(line)).flatten();
        match found {
            Some(Directive::If) => open.push(Group {
                branches: vec![Branch::new((start, end), source.len())],
                endif: (end, end),
                saw_code: false,
            }),
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
                    // branch, the one that stays after patching.
                    if let Some(parent) = open.last_mut() {
                        let kept = &group.branches[0];
                        let outer = parent.current();
                        outer.floor = outer.floor.min(outer.delta + kept.floor);
                        outer.delta += kept.delta;
                        parent.saw_code = true;
                    }
                    if group.splits_syntax() {
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
                }
            }
        }
        start = end + 1;
    }
    if split.is_empty() {
        return None;
    }
    let patched = blank_groups(source, &split);
    if braces_balance(&patched) {
        return Some(patched);
    }
    // The first branches do not fit together (`#if X` opens a brace that an
    // `#if !X` closes). Blanking one half of such a pair breaks a tree that
    // keeping both halves may not, so only the groups no brace depends on go.
    split.retain(Group::keeps_depth);
    (!split.is_empty()).then(|| blank_groups(source, &split))
}

/// `source` with each of `groups` reduced to its first branch.
fn blank_groups(source: &[u8], groups: &[Group]) -> Vec<u8> {
    let mut patched = source.to_vec();
    for group in groups {
        blank_range(&mut patched, group.endif.0, group.endif.1);
        for (k, branch) in group.branches.iter().enumerate() {
            blank_range(&mut patched, branch.directive.0, branch.directive.1);
            if k > 0 {
                blank_range(&mut patched, branch.body.0, branch.body.1);
            }
        }
    }
    patched
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

    /// When the first branches do not pair (`#if X` / `#else` opens,
    /// `#if !X` closes), neither half is blanked.
    #[test]
    fn test_prepare_source_unpaired_first_branches_blank_neither_half() {
        let src = "void M() {\n#if X\n#else\nlock (o) {\n#endif\nBar();\n#if !X\n}\n#endif\n}\n";
        assert_eq!(patched(src), None);
    }

    /// The unpaired-halves fallback still reduces a group no brace depends
    /// on.
    #[test]
    fn test_prepare_source_unpaired_halves_still_reduce_else_group() {
        let src = "void M() {\n#if X\n#else\nlock (o) {\n#endif\nBar();\n#if !X\n}\n#endif\nif (a) { }\n#if Y\nelse { }\n#endif\n}\n";
        let out = patched(src).expect("patched");
        assert!(out.contains("#if X") && out.contains("#if !X"), "{out:?}");
        assert!(
            !out.contains("#if Y") && out.contains("else { }"),
            "{out:?}"
        );
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
