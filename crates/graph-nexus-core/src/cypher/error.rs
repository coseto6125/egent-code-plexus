use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum CypherError {
    Lex {
        offset: usize,
        msg: String,
    },
    Parse {
        offset: usize,
        expected: String,
        found: String,
    },
    Semantic {
        msg: String,
    },
    Exec {
        msg: String,
    },
}

impl fmt::Display for CypherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lex { offset, msg } => write!(f, "lex error at byte {offset}: {msg}"),
            Self::Parse {
                offset,
                expected,
                found,
            } => write!(
                f,
                "parse error at byte {offset}: expected {expected}, found {found}"
            ),
            Self::Semantic { msg } => write!(f, "semantic error: {msg}"),
            Self::Exec { msg } => write!(f, "execution error: {msg}"),
        }
    }
}

impl std::error::Error for CypherError {}
