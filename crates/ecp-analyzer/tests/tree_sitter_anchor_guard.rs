//! Guards against tree-sitter 0.26.11 dropping sibling adjacency when an
//! optional or repeated pattern between two anchors matches zero nodes.

use std::fs;
use std::path::{Path, PathBuf};

fn skip_trivia(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
        } else if bytes[index] == b';' {
            index += 1;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else {
            break;
        }
    }
    index
}

fn skip_string(bytes: &[u8], mut index: usize) -> usize {
    index += 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    index
}

fn skip_balanced_pattern(bytes: &[u8], index: usize) -> Option<usize> {
    let closing = match bytes.get(index)? {
        b'(' => b')',
        b'[' => b']',
        _ => return None,
    };
    let mut stack = vec![closing];
    let mut cursor = index + 1;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => cursor = skip_string(bytes, cursor),
            b';' => cursor = skip_trivia(bytes, cursor),
            b'(' => {
                stack.push(b')');
                cursor += 1;
            }
            b'[' => {
                stack.push(b']');
                cursor += 1;
            }
            byte if stack.last() == Some(&byte) => {
                stack.pop();
                cursor += 1;
                if stack.is_empty() {
                    return Some(cursor);
                }
            }
            _ => cursor += 1,
        }
    }
    None
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn skip_field_prefix(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index;
    while bytes
        .get(cursor)
        .is_some_and(|byte| is_identifier_byte(*byte))
    {
        cursor += 1;
    }
    if cursor == index {
        return index;
    }

    let separator = skip_trivia(bytes, cursor);
    if bytes.get(separator) == Some(&b':') {
        skip_trivia(bytes, separator + 1)
    } else {
        index
    }
}

fn skip_pattern_atom(bytes: &[u8], index: usize) -> Option<usize> {
    let atom = skip_field_prefix(bytes, index);
    match bytes.get(atom)? {
        b'(' | b'[' => skip_balanced_pattern(bytes, atom),
        b'"' => Some(skip_string(bytes, atom)),
        b'_' => Some(atom + 1),
        _ => None,
    }
}

fn skip_capture(bytes: &[u8], index: usize) -> usize {
    let mut cursor = index + 1;
    while bytes
        .get(cursor)
        .is_some_and(|byte| is_identifier_byte(*byte))
    {
        cursor += 1;
    }
    cursor
}

fn skip_zero_width_pattern(bytes: &[u8], index: usize) -> Option<usize> {
    let mut cursor = skip_pattern_atom(bytes, index)?;
    let mut can_match_zero = false;

    loop {
        cursor = skip_trivia(bytes, cursor);
        match bytes.get(cursor) {
            Some(b'*' | b'?') => {
                can_match_zero = true;
                cursor += 1;
            }
            Some(b'+') => cursor += 1,
            Some(b'@') => cursor = skip_capture(bytes, cursor),
            _ => return can_match_zero.then_some(cursor),
        }
    }
}

fn find_double_anchored_zero_width_pattern(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index = skip_string(bytes, index);
                continue;
            }
            b';' => {
                index = skip_trivia(bytes, index);
                continue;
            }
            b'@' => {
                index = skip_capture(bytes, index);
                continue;
            }
            b'.' => {
                let anchor = index;
                let pattern_start = skip_trivia(bytes, index + 1);
                let Some(pattern_end) = skip_zero_width_pattern(bytes, pattern_start) else {
                    index += 1;
                    continue;
                };
                if bytes.get(pattern_end) == Some(&b'.') {
                    return Some(anchor);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn collect_query_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("failed to read query directory entry").path();
        if path.is_dir() {
            collect_query_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "scm") {
            files.push(path);
        }
    }
}

#[test]
fn test_find_double_anchored_zero_width_pattern_star_detects_regression_shape() {
    let query = r#"
        (program
          (lexical_declaration) @a
          .
          (comment)*
          .
          (function_declaration) @b)
    "#;

    assert!(find_double_anchored_zero_width_pattern(query).is_some());
}

#[test]
fn test_find_double_anchored_zero_width_pattern_optional_atoms_detects_variants() {
    let queries = [
        "(program (lexical_declaration) . _? . (function_declaration))",
        "(program (lexical_declaration) . \";\"* . (function_declaration))",
        "(function_declaration name: (identifier) . parameters: (formal_parameters)? . body: (statement_block))",
    ];
    let language = tree_sitter_javascript::LANGUAGE.into();

    for query in queries {
        tree_sitter::Query::new(&language, query).expect("valid JavaScript query");
        assert!(
            find_double_anchored_zero_width_pattern(query).is_some(),
            "guard missed {query}"
        );
    }
}

#[test]
fn test_find_double_anchored_zero_width_pattern_capture_suffix_detects_variant() {
    let query = "(program (lexical_declaration) . (comment) @comments* . (function_declaration))";
    let language = tree_sitter_javascript::LANGUAGE.into();

    tree_sitter::Query::new(&language, query).expect("valid JavaScript query");
    assert!(find_double_anchored_zero_width_pattern(query).is_some());
}

#[test]
fn test_find_double_anchored_zero_width_pattern_string_and_comment_ignores_noise() {
    let queries = [
        r#"
            ; . (comment)* .
            ((string) @value (#eq? @value ". (comment)* ."))
        "#,
        "(program (comment) @foo._? . (function_declaration))",
    ];
    let language = tree_sitter_javascript::LANGUAGE.into();

    for query in queries {
        tree_sitter::Query::new(&language, query).expect("valid JavaScript query");
        assert_eq!(find_double_anchored_zero_width_pattern(query), None);
    }
}

#[test]
fn test_query_sources_double_anchored_zero_width_pattern_absent() {
    let mut files = Vec::new();
    collect_query_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files.sort();

    for path in files {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        if let Some(offset) = find_double_anchored_zero_width_pattern(&source) {
            panic!(
                "{} contains `. Q* .` or `. Q? .` at byte {offset}; tree-sitter 0.26.11 can drop sibling adjacency when Q matches zero",
                path.display()
            );
        }
    }
}
