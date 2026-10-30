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
    Verbatim,
    /// A raw string literal closed by this many quotes.
    Raw(usize),
}

/// What one line of code does to the brace depth and which word bounds it.
#[derive(Default)]
struct LineScan {
    delta: i32,
    /// The lowest depth reached on the line, relative to its start (`<= 0`).
    floor: i32,
    first_is_else: Option<bool>,
    last_is_else: Option<bool>,
}

fn note(out: &mut LineScan, is_else: bool) {
    out.first_is_else.get_or_insert(is_else);
    out.last_is_else = Some(is_else);
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
        match *lex {
            Lex::Block => {
                match line[i..].windows(2).position(|w| w == b"*/") {
                    Some(k) => i += k + 2,
                    None => return out,
                }
                *lex = Lex::Code;
                continue;
            }
            Lex::Verbatim => {
                // A doubled quote is an escaped quote; a lone one closes.
                loop {
                    match (line.get(i), line.get(i + 1)) {
                        (None, _) => return out,
                        (Some(b'"'), Some(b'"')) => i += 2,
                        (Some(b'"'), _) => break,
                        _ => i += 1,
                    }
                }
                i += 1;
                *lex = Lex::Code;
                note(&mut out, false);
                continue;
            }
            Lex::Raw(quotes) => {
                let closer = vec![b'"'; quotes];
                match line[i..].windows(quotes).position(|w| w == closer) {
                    Some(k) => i += k + quotes,
                    None => return out,
                }
                *lex = Lex::Code;
                note(&mut out, false);
                continue;
            }
            Lex::Code => {}
        }
        match line[i] {
            b'/' if line.get(i + 1) == Some(&b'/') => break,
            b'/' if line.get(i + 1) == Some(&b'*') => {
                *lex = Lex::Block;
                i += 2;
                continue;
            }
            b'"' => {
                let verbatim = (i > 0 && line[i - 1] == b'@') || line[..i].ends_with(b"@$");
                let quotes = line[i..].iter().take_while(|&&b| b == b'"').count();
                if verbatim {
                    *lex = Lex::Verbatim;
                    i += 1;
                } else if quotes >= 3 {
                    *lex = Lex::Raw(quotes);
                    i += quotes;
                } else if quotes == 2 {
                    note(&mut out, false);
                    i += 2;
                } else {
                    i = skip_quoted(line, i, b'"');
                    note(&mut out, false);
                }
                continue;
            }
            b'\'' => {
                i = skip_quoted(line, i, b'\'');
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
                note(&mut out, &line[start..i] == b"else");
                continue;
            }
            b if b.is_ascii_whitespace() => {}
            _ => note(&mut out, false),
        }
        i += 1;
    }
    out
}

/// The index after the string or char literal that opens at `open`.
fn skip_quoted(line: &[u8], open: usize, quote: u8) -> usize {
    let mut i = open + 1;
    while i < line.len() {
        match line[i] {
            b'\\' => i += 2,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    line.len()
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
    first_is_else: bool,
    last_is_else: bool,
}

impl Branch {
    fn new(directive: (usize, usize), source_len: usize) -> Self {
        let body_start = (directive.1 + 1).min(source_len);
        Self {
            directive,
            body: (body_start, body_start),
            delta: 0,
            floor: 0,
            first_is_else: false,
            last_is_else: false,
        }
    }

    fn add(&mut self, line: &LineScan, first_line: bool) {
        self.floor = self.floor.min(self.delta + line.floor);
        self.delta += line.delta;
        if first_line {
            self.first_is_else = line.first_is_else == Some(true);
        }
        if let Some(is_else) = line.last_is_else {
            self.last_is_else = is_else;
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
    /// a statement from before the group (`else`) or ends in one, closes a
    /// brace opened before it, or the branches leave different depths.
    fn splits_syntax(&self) -> bool {
        let depth = self.branches[0].delta;
        self.branches
            .iter()
            .any(|b| b.floor < 0 || b.first_is_else || b.last_is_else || b.delta != depth)
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
/// branch's symbol were the only one defined. `None` when no group splits
/// syntax. No byte moves.
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
                    let has_code = scan.first_is_else.is_some();
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
    let mut patched = source.to_vec();
    for group in &split {
        blank_range(&mut patched, group.endif.0, group.endif.1);
        for (k, branch) in group.branches.iter().enumerate() {
            blank_range(&mut patched, branch.directive.0, branch.directive.1);
            if k > 0 {
                blank_range(&mut patched, branch.body.0, branch.body.1);
            }
        }
    }
    Some(patched)
}

#[cfg(test)]
mod tests {
    use super::prepare_source;

    fn patched(src: &str) -> Option<String> {
        prepare_source(src.as_bytes()).map(|b| String::from_utf8(b).expect("ascii"))
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
}
