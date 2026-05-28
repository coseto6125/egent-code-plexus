use crate::cypher::error::CypherError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Match,
    Optional,
    Where,
    Return,
    Distinct,
    With,
    As,
    OrderBy,
    Asc,
    Desc,
    Skip,
    Limit,
    Union,
    All,
    And,
    Or,
    Not,
    In,
    StartsWith,
    EndsWith,
    Contains,

    // Literals
    True,
    False,
    Null,
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),

    // Symbols
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    DotDot,
    Colon,
    Pipe,
    Star,
    Dash,
    Arrow,
    RevArrow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    RegexMatch,
}

/// Attempt to consume one whitespace-separated word at `*i` that matches
/// `expected` (case-insensitive). Advances `*i` past the word on success;
/// restores `*i` to its entry value on mismatch.
fn try_consume_word(input: &str, bytes: &[u8], i: &mut usize, expected: &str) -> bool {
    let save = *i;
    while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
        *i += 1;
    }
    let bs = *i;
    while *i < bytes.len() && bytes[*i].is_ascii_alphabetic() {
        *i += 1;
    }
    if input[bs..*i].eq_ignore_ascii_case(expected) {
        true
    } else {
        *i = save;
        false
    }
}

pub fn tokenize(input: &str) -> Result<Vec<Token>, CypherError> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Multi-char symbols first
        if c == b'-' && bytes.get(i + 1) == Some(&b'>') {
            out.push(Token::Arrow);
            i += 2;
            continue;
        }
        if c == b'<' && bytes.get(i + 1) == Some(&b'-') {
            out.push(Token::RevArrow);
            i += 2;
            continue;
        }
        if c == b'<' && bytes.get(i + 1) == Some(&b'=') {
            out.push(Token::Le);
            i += 2;
            continue;
        }
        if c == b'>' && bytes.get(i + 1) == Some(&b'=') {
            out.push(Token::Ge);
            i += 2;
            continue;
        }
        if c == b'<' && bytes.get(i + 1) == Some(&b'>') {
            out.push(Token::Ne);
            i += 2;
            continue;
        }
        if c == b'=' && bytes.get(i + 1) == Some(&b'~') {
            out.push(Token::RegexMatch);
            i += 2;
            continue;
        }
        if c == b'.' && bytes.get(i + 1) == Some(&b'.') {
            out.push(Token::DotDot);
            i += 2;
            continue;
        }

        match c {
            b'(' => {
                out.push(Token::LParen);
                i += 1;
                continue;
            }
            b')' => {
                out.push(Token::RParen);
                i += 1;
                continue;
            }
            b'[' => {
                out.push(Token::LBracket);
                i += 1;
                continue;
            }
            b']' => {
                out.push(Token::RBracket);
                i += 1;
                continue;
            }
            b'{' => {
                out.push(Token::LBrace);
                i += 1;
                continue;
            }
            b'}' => {
                out.push(Token::RBrace);
                i += 1;
                continue;
            }
            b',' => {
                out.push(Token::Comma);
                i += 1;
                continue;
            }
            b'.' => {
                out.push(Token::Dot);
                i += 1;
                continue;
            }
            b':' => {
                out.push(Token::Colon);
                i += 1;
                continue;
            }
            b'|' => {
                out.push(Token::Pipe);
                i += 1;
                continue;
            }
            b'*' => {
                out.push(Token::Star);
                i += 1;
                continue;
            }
            b'-' => {
                out.push(Token::Dash);
                i += 1;
                continue;
            }
            b'=' => {
                out.push(Token::Eq);
                i += 1;
                continue;
            }
            b'<' => {
                out.push(Token::Lt);
                i += 1;
                continue;
            }
            b'>' => {
                out.push(Token::Gt);
                i += 1;
                continue;
            }
            _ => {}
        }

        // String literal
        if c == b'\'' || c == b'"' {
            let quote = c;
            let start = i;
            i += 1;
            let mut s = String::new();
            loop {
                if i >= bytes.len() {
                    return Err(CypherError::Lex {
                        offset: start,
                        msg: "unterminated string".into(),
                    });
                }
                let b = bytes[i];
                if b == b'\\' && i + 1 < bytes.len() {
                    s.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                if b == quote {
                    i += 1;
                    break;
                }
                s.push(b as char);
                i += 1;
            }
            out.push(Token::Str(s));
            continue;
        }

        // Backtick-quoted identifier — Neo4j standard for ident names that
        // contain non-ident chars (dot, space, hyphen) or shadow reserved
        // words. `foo` is the literal identifier `foo`; the doubled-backtick
        // sequence ``` `` ``` inside escapes one backtick (matching Neo4j).
        // Emitted as Token::Ident so every parser site that already accepts
        // an identifier (alias slot, prop name, label name, function name)
        // transparently picks up the quoted form — no parser change.
        if c == b'`' {
            let start = i;
            i += 1;
            let mut s = String::new();
            loop {
                if i >= bytes.len() {
                    return Err(CypherError::Lex {
                        offset: start,
                        msg: "unterminated backtick identifier".into(),
                    });
                }
                if bytes[i] == b'`' {
                    // `` inside backticks → escaped single backtick (Neo4j).
                    if i + 1 < bytes.len() && bytes[i + 1] == b'`' {
                        s.push('`');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                s.push(bytes[i] as char);
                i += 1;
            }
            if s.is_empty() {
                return Err(CypherError::Lex {
                    offset: start,
                    msg: "empty backtick identifier".into(),
                });
            }
            out.push(Token::Ident(s));
            continue;
        }

        // Number
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let mut is_float = false;
            if i < bytes.len()
                && bytes[i] == b'.'
                && bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit())
            {
                is_float = true;
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let s = &input[start..i];
            let tok = if is_float {
                Token::Float(s.parse().map_err(|_| CypherError::Lex {
                    offset: start,
                    msg: format!("bad float {s}"),
                })?)
            } else {
                Token::Int(s.parse().map_err(|_| CypherError::Lex {
                    offset: start,
                    msg: format!("bad int {s}"),
                })?)
            };
            out.push(tok);
            continue;
        }

        // Identifier / keyword
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let s = &input[start..i];
            let tok = match s.to_ascii_uppercase().as_str() {
                "MATCH" => Token::Match,
                "OPTIONAL" => Token::Optional,
                "WHERE" => Token::Where,
                "RETURN" => Token::Return,
                "DISTINCT" => Token::Distinct,
                "WITH" => Token::With,
                "AS" => Token::As,
                "ORDER" => {
                    if try_consume_word(input, bytes, &mut i, "BY") {
                        Token::OrderBy
                    } else {
                        Token::Ident(s.into())
                    }
                }
                "ASC" => Token::Asc,
                "DESC" => Token::Desc,
                "SKIP" => Token::Skip,
                "LIMIT" => Token::Limit,
                "UNION" => Token::Union,
                "ALL" => Token::All,
                "AND" => Token::And,
                "OR" => Token::Or,
                "NOT" => Token::Not,
                "IN" => Token::In,
                "STARTS" => {
                    if try_consume_word(input, bytes, &mut i, "WITH") {
                        Token::StartsWith
                    } else {
                        Token::Ident(s.into())
                    }
                }
                "ENDS" => {
                    if try_consume_word(input, bytes, &mut i, "WITH") {
                        Token::EndsWith
                    } else {
                        Token::Ident(s.into())
                    }
                }
                "CONTAINS" => Token::Contains,
                "TRUE" => Token::True,
                "FALSE" => Token::False,
                "NULL" => Token::Null,
                _ => Token::Ident(s.into()),
            };
            out.push(tok);
            continue;
        }

        return Err(CypherError::Lex {
            offset: i,
            msg: format!("unexpected byte {:?}", c as char),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Vec<Token> {
        tokenize(s).expect("lex ok")
    }

    #[test]
    fn keywords_case_insensitive() {
        for k in ["MATCH", "match", "Match"] {
            assert_eq!(lex(k), vec![Token::Match]);
        }
    }

    #[test]
    fn punctuation() {
        assert_eq!(
            lex("()[]{},.:"),
            vec![
                Token::LParen,
                Token::RParen,
                Token::LBracket,
                Token::RBracket,
                Token::LBrace,
                Token::RBrace,
                Token::Comma,
                Token::Dot,
                Token::Colon
            ]
        );
    }

    #[test]
    fn arrows_and_pipe() {
        assert_eq!(lex("->"), vec![Token::Arrow]);
        assert_eq!(lex("<-"), vec![Token::RevArrow]);
        assert_eq!(lex("-"), vec![Token::Dash]);
        assert_eq!(lex("|"), vec![Token::Pipe]);
        assert_eq!(lex("*"), vec![Token::Star]);
        assert_eq!(lex(".."), vec![Token::DotDot]);
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(
            lex("= <> < <= > >= =~"),
            vec![
                Token::Eq,
                Token::Ne,
                Token::Lt,
                Token::Le,
                Token::Gt,
                Token::Ge,
                Token::RegexMatch
            ]
        );
    }

    #[test]
    fn string_literal_single_and_double() {
        assert_eq!(lex("'hello'"), vec![Token::Str("hello".into())]);
        assert_eq!(lex(r#""hi""#), vec![Token::Str("hi".into())]);
        assert_eq!(lex(r"'it\'s'"), vec![Token::Str("it's".into())]);
    }

    #[test]
    fn numbers_int_and_float() {
        assert_eq!(lex("42"), vec![Token::Int(42)]);
        assert_eq!(lex("1.5"), vec![Token::Float(1.5)]);
        assert_eq!(lex("-7"), vec![Token::Dash, Token::Int(7)]);
    }

    #[test]
    fn identifiers_and_kinds() {
        assert_eq!(
            lex("foo Function r"),
            vec![
                Token::Ident("foo".into()),
                Token::Ident("Function".into()),
                Token::Ident("r".into())
            ]
        );
    }

    #[test]
    fn whitespace_skipped() {
        assert_eq!(
            lex("MATCH    (a)"),
            vec![
                Token::Match,
                Token::LParen,
                Token::Ident("a".into()),
                Token::RParen
            ]
        );
    }

    #[test]
    fn lex_error_unterminated_string() {
        let err = tokenize("'unclosed").unwrap_err();
        match err {
            CypherError::Lex { .. } => {}
            e => panic!("expected Lex, got {e:?}"),
        }
    }
}
