//! Syntax-pattern matcher used by `ecp pattern`.
//!
//! The graph answers "who calls X"; it holds declarations only, so a
//! statement-shaped question ("which `try` swallows its exception") has no
//! node to match against. This module fills that gap: it runs an ast-grep
//! pattern over the tree-sitter grammars ecp already vendors, so the caller
//! keeps one parser stack instead of two.
//!
//! Dispatch is by file extension, mirroring [`crate::identifier_finder`].
//! An unsupported extension returns `None` and the caller skips the file.
//!
//! ## Metavariable escaping
//!
//! ast-grep writes metavariables as `$NAME` / `$$$NAME`, but most grammars
//! reject `$` inside an identifier, so the pattern fails to parse and every
//! metavariable degrades into two unrelated nodes. The match then returns
//! zero hits and reports no error. Two hooks together avoid that:
//! `pre_process_pattern` swaps `$` for a character the grammar accepts, and
//! `expando_char` tells the metavariable extractor what to look for. A
//! language whose identifiers already admit `$` (JavaScript, TypeScript)
//! keeps `$` and skips the swap.
//!
//! The stand-in per language matches ast-grep's own table, so a pattern that
//! works in the `ast-grep` CLI works here. `LINEAR_B` covers C / C++, whose
//! identifiers admit no Latin-1 letter.

use ast_grep_core::matcher::PatternBuilder;
/// Re-exported so callers report a parse failure without depending on ast-grep.
pub use ast_grep_core::matcher::PatternError;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use ast_grep_core::{Language, Pattern};

/// Re-exported so callers compile patterns without depending on ast-grep directly.
pub use ast_grep_core::Pattern as CompiledPattern;
use ecp_core::analyzer::types::IdentifierRange;
use std::borrow::Cow;

/// Identifier-safe stand-ins for `$`, taken from ast-grep's language table.
const MICRO: char = 'µ';
const LINEAR_B: char = '\u{10000}';

/// One tree-sitter grammar plus the metavariable character its identifiers
/// accept. Cloned per file; `TSLanguage` clones are cheap handle copies.
#[derive(Clone)]
pub struct PatternLang {
    ts: TSLanguage,
    expando: char,
}

impl Language for PatternLang {
    fn expando_char(&self) -> char {
        self.expando
    }

    /// Swap the `$` of every metavariable sigil for [`Self::expando_char`],
    /// and leave every other `$` alone. A pattern searching for a literal
    /// `$` (a shell variable, a `"$100"` string) must survive unchanged, so
    /// only `$A` / `$$A` / `$$$A` and the anonymous `$$$` are rewritten.
    /// This mirrors `ast_grep_language::pre_process_pattern`.
    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        if self.expando == '$' || !query.contains('$') {
            return Cow::Borrowed(query);
        }
        let mut out = String::with_capacity(query.len());
        let mut dollars = 0usize;
        for c in query.chars() {
            if c == '$' {
                dollars += 1;
                continue;
            }
            // `$A` names a metavariable; `$$$` alone is the anonymous multi.
            let sigil = if c.is_ascii_uppercase() || c == '_' || dollars == 3 {
                self.expando
            } else {
                '$'
            };
            out.extend(std::iter::repeat_n(sigil, dollars));
            dollars = 0;
            out.push(c);
        }
        let sigil = if dollars == 3 { self.expando } else { '$' };
        out.extend(std::iter::repeat_n(sigil, dollars));
        Cow::Owned(out)
    }

    fn kind_to_id(&self, kind: &str) -> u16 {
        self.ts.id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.ts.field_id_for_name(field).map(|f| f.get())
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

impl LanguageExt for PatternLang {
    fn get_ts_language(&self) -> TSLanguage {
        self.ts.clone()
    }
}

/// Grammar for `path`'s extension, or `None` when patterns are unsupported
/// there. The set is narrower than `identifier_finder`'s: a language joins
/// only once a test in this module proves a metavariable pattern matches on
/// it, because a wrong `expando` yields zero hits rather than an error.
pub fn lang_for_path(path: &str) -> Option<PatternLang> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase())?;
    let (ts, expando): (TSLanguage, char) = match ext.as_str() {
        // Identifiers reject `$`; the stand-in column follows ast-grep.
        "py" | "pyi" => (tree_sitter_python::LANGUAGE.into(), MICRO),
        "rs" => (tree_sitter_rust::LANGUAGE.into(), MICRO),
        "go" => (tree_sitter_go::LANGUAGE.into(), MICRO),
        "rb" => (tree_sitter_ruby::LANGUAGE.into(), MICRO),
        "kt" | "kts" => (tree_sitter_kotlin::LANGUAGE.into(), MICRO),
        "cs" => (tree_sitter_c_sharp::LANGUAGE.into(), MICRO),
        // `LANGUAGE_PHP_ONLY`, not the HTML-mixing `LANGUAGE_PHP` that
        // `identifier_finder` uses: a bare pattern carries no `<?php` opener,
        // so the mixing grammar reads it as HTML text and matches nothing.
        "php" => (tree_sitter_php::LANGUAGE_PHP_ONLY.into(), MICRO),
        "swift" => (tree_sitter_swift::LANGUAGE.into(), MICRO),
        "c" => (tree_sitter_c::LANGUAGE.into(), LINEAR_B),
        // `.h` routes to C++ for the reason `identifier_finder` documents.
        "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" | "h" => {
            (tree_sitter_cpp::LANGUAGE.into(), LINEAR_B)
        }
        // Identifiers already admit `$`, so the sigil passes through.
        "java" => (tree_sitter_java::LANGUAGE.into(), '$'),
        "ts" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), '$'),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), '$'),
        "js" | "jsx" | "mjs" | "cjs" => (tree_sitter_javascript::LANGUAGE.into(), '$'),
        // Dart is absent on purpose: tree-sitter-dart parses no bare call
        // expression, so `f($X)` matches nothing and reports no error. The
        // `ast-grep` CLI behaves the same. Add it once a pattern shape holds.
        _ => return None,
    };
    Some(PatternLang { ts, expando })
}

/// Byte ranges in `source` matching `pattern`, or `None` when `path`'s
/// language has no pattern support. Compile `pattern` once per language with
/// [`compile`] when scanning many files.
pub fn find_pattern_occurrences(
    path: &str,
    source: &[u8],
    pattern: &str,
) -> Option<Result<Vec<IdentifierRange>, PatternError>> {
    let lang = lang_for_path(path)?;
    Some(compile(pattern, &lang).map(|pat| match_source(source, &lang, &pat)))
}

/// Compile `pattern` against `lang`. Compilation parses the pattern, so hoist
/// it out of a per-file loop.
pub fn compile(pattern: &str, lang: &PatternLang) -> Result<Pattern, PatternError> {
    Pattern::try_new(pattern, lang.clone())
}

/// Run an already-compiled `pattern` over one file's bytes. Invalid UTF-8
/// yields no matches; the grammars here all assume UTF-8 source.
pub fn match_source(source: &[u8], lang: &PatternLang, pattern: &Pattern) -> Vec<IdentifierRange> {
    let Ok(text) = std::str::from_utf8(source) else {
        return Vec::new();
    };
    let root = lang.ast_grep(text);
    root.root()
        .find_all(pattern)
        .map(|m| {
            let range = m.range();
            let pos = m.start_pos();
            IdentifierRange {
                start_byte: range.start,
                end_byte: range.end,
                row: pos.line(),
                col: pos.column(&m),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(path: &str, source: &str, pattern: &str) -> Vec<IdentifierRange> {
        find_pattern_occurrences(path, source.as_bytes(), pattern)
            .expect("language should be supported")
            .expect("pattern should compile")
    }

    #[test]
    fn find_pattern_python_multi_metavar_matches_block() {
        let src = "try:\n    risky()\nexcept ValueError:\n    pass\n";
        assert_eq!(
            hits("a.py", src, "try:\n    $$$BODY\nexcept $$$E:\n    pass").len(),
            1
        );
    }

    #[test]
    fn find_pattern_python_single_metavar_binds_argument() {
        let src = "x = json.loads(raw)\ny = json.loads(other)\n";
        assert_eq!(hits("a.py", src, "json.loads($X)").len(), 2);
    }

    #[test]
    fn find_pattern_rust_metavar_matches_call() {
        let src = "fn main() { let a = foo(1); let b = foo(2); }";
        assert_eq!(hits("a.rs", src, "foo($X)").len(), 2);
    }

    #[test]
    fn find_pattern_go_metavar_matches_call() {
        let src = "package m\nfunc main() { print(1); print(2) }\n";
        assert_eq!(hits("a.go", src, "print($X)").len(), 2);
    }

    #[test]
    fn find_pattern_ruby_metavar_matches_call() {
        assert_eq!(hits("a.rb", "puts 1\nputs 2\n", "puts $X").len(), 2);
    }

    #[test]
    fn find_pattern_csharp_metavar_matches_call() {
        let src = "class A { void M() { F(1); F(2); } }";
        assert_eq!(hits("A.cs", src, "F($X)").len(), 2);
    }

    #[test]
    fn find_pattern_php_metavar_matches_call() {
        assert_eq!(hits("a.php", "<?php f(1); f(2);", "f($X)").len(), 2);
    }

    #[test]
    fn find_pattern_kotlin_metavar_matches_call() {
        assert_eq!(hits("a.kt", "fun m() { f(1); f(2) }", "f($X)").len(), 2);
    }

    #[test]
    fn find_pattern_swift_metavar_matches_call() {
        assert_eq!(hits("a.swift", "f(1)\nf(2)\n", "f($X)").len(), 2);
    }

    #[test]
    fn find_pattern_c_statement_pattern_matches_call() {
        // C parses a bare `f($X)` as a declaration; the trailing `;` is what
        // makes the pattern an expression statement.
        let src = "int main() { f(1); f(2); }";
        assert_eq!(hits("a.c", src, "f($X);").len(), 2);
    }

    #[test]
    fn find_pattern_cpp_statement_pattern_matches_call() {
        let src = "int main() { f(1); f(2); }";
        assert_eq!(hits("a.cpp", src, "f($X);").len(), 2);
    }

    #[test]
    fn find_pattern_dart_is_unsupported() {
        assert!(lang_for_path("a.dart").is_none());
    }

    #[test]
    fn find_pattern_literal_dollar_survives_escaping() {
        // `$1` is not a metavariable, so the pattern must keep its `$`.
        let src = "a = \"$100\"\nb = \"other\"\n";
        assert_eq!(hits("a.py", src, "\"$100\"").len(), 1);
    }

    #[test]
    fn find_pattern_java_metavar_matches_call() {
        let src = "class A { void m() { f(1); f(2); } }";
        assert_eq!(hits("A.java", src, "f($X)").len(), 2);
    }

    #[test]
    fn find_pattern_typescript_keeps_dollar_metavar() {
        let src = "const a = f(1);\nconst b = f(2);\n";
        assert_eq!(hits("a.ts", src, "f($X)").len(), 2);
    }

    #[test]
    fn find_pattern_javascript_metavar_matches_call() {
        let src = "f(1);\nf(2);\n";
        assert_eq!(hits("a.js", src, "f($X)").len(), 2);
    }

    #[test]
    fn find_pattern_literal_pattern_needs_no_escaping() {
        let src = "def a():\n    pass\ndef b():\n    pass\n";
        assert_eq!(hits("a.py", src, "pass").len(), 2);
    }

    #[test]
    fn find_pattern_reports_byte_range_and_row() {
        let src = "x = 1\ny = foo(2)\n";
        let found = hits("a.py", src, "foo($X)");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].row, 1);
        assert_eq!(&src[found[0].start_byte..found[0].end_byte], "foo(2)");
    }

    #[test]
    fn find_pattern_unsupported_extension_returns_none() {
        assert!(find_pattern_occurrences("README.md", b"# hi\n", "hi").is_none());
    }

    #[test]
    fn find_pattern_no_extension_returns_none() {
        assert!(find_pattern_occurrences("Makefile", b"all:\n", "all").is_none());
    }

    #[test]
    fn find_pattern_invalid_pattern_reports_error() {
        // Two sibling statements cannot form one pattern node.
        assert!(find_pattern_occurrences("a.py", b"pass\n", "a = 1\nb = 2")
            .expect("python is supported")
            .is_err());
    }

    #[test]
    fn find_pattern_invalid_utf8_yields_no_matches() {
        let found = find_pattern_occurrences("a.py", &[0xff, 0xfe], "pass")
            .expect("python is supported")
            .expect("pattern compiles");
        assert!(found.is_empty());
    }
}
