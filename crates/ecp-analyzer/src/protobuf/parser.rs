//! Hand-rolled `.proto` lexer for T4-5 schema-field extraction.
//!
//! Handles proto2 / proto3 field declarations inside top-level `message`
//! blocks.  See `mod.rs` for the full list of acknowledged limitations.

use super::schema_extractors::{
    classify_protobuf_type, PROTOBUF_FIELD_MODIFIERS, PROTOBUF_FRAMEWORK,
};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawSchemaField};
use ecp_core::pool::StringPool;
use std::path::Path;

pub struct ProtobufProvider;

impl ProtobufProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self)
    }
}

impl LanguageProvider for ProtobufProvider {
    fn name(&self) -> &'static str {
        "protobuf"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let text = std::str::from_utf8(source)
            .map_err(|e| anyhow::anyhow!("protobuf: UTF-8 decode error in {:?}: {}", path, e))?;

        let mut pool = StringPool::new();
        let fields = extract_proto_fields(text, &mut pool);
        let schema_fields = (!fields.is_empty()).then(|| fields.into_boxed_slice());

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            schema_fields,
            ..Default::default()
        })
    }
}

/// Line-oriented proto lexer.
///
/// State machine:
/// - `current_message`: name of the enclosing `message { }` block, or `None`
///   when at the top level.
/// - `depth`: brace nesting depth.  A top-level `message` bumps depth to 1;
///   any nested `{` (including nested messages, oneofs, options) bumps it
///   further.  Fields are only emitted when `depth == 1`.
fn extract_proto_fields(text: &str, pool: &mut StringPool) -> Vec<RawSchemaField> {
    let mut out: Vec<RawSchemaField> = Vec::new();
    let mut current_message: Option<String> = None;
    let mut depth: u32 = 0;

    for (line_idx, raw_line) in text.lines().enumerate() {
        let row = line_idx as u32;

        // Strip inline `//` comment and trim whitespace.
        let line = strip_line_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        // ── Brace tracking ──────────────────────────────────────────────────
        // Count braces on this line *before* attempting field extraction so
        // depth is updated even for lines that also contain a field.
        let opens = line.chars().filter(|&c| c == '{').count() as u32;
        let closes = line.chars().filter(|&c| c == '}').count() as u32;

        // ── `message Name {` detection ──────────────────────────────────────
        // Only at depth 0 (top-level) — nested messages are skipped per the
        // v1 limitation documented in mod.rs.
        if depth == 0 {
            if let Some(name) = parse_message_header(line) {
                current_message = Some(name);
                // The `{` on this line is already counted below via `opens`.
            }
        }

        // Update depth AFTER checking for message headers so that
        // `message Foo {` at depth 0 bumps to depth 1 on the same line.
        depth = depth.saturating_add(opens).saturating_sub(closes);

        // After depth update: if we just closed the outermost message block,
        // clear the message context.
        if depth == 0 {
            current_message = None;
        }

        // ── Field extraction — only at depth 1 inside a known message ───────
        let Some(ref owner) = current_message else {
            continue;
        };
        if depth != 1 {
            // depth 0 = outside any message; depth ≥ 2 = nested block (oneof,
            // nested message, options block) — skip in v1.
            continue;
        }

        if let Some((field_name, type_token)) = parse_field_line(line) {
            let type_class = classify_protobuf_type(type_token);
            let span = (row, 0u32, row, line.len() as u32);
            out.push(RawSchemaField {
                name: pool.add(&field_name),
                type_class,
                owner_class: pool.add(owner),
                framework: PROTOBUF_FRAMEWORK,
                span,
            });
        }
    }

    out
}

/// Strip the `//`-prefixed tail of a line (proto single-line comment).
///
/// Does not attempt to handle `//` inside string literals (proto field
/// options with string defaults are extremely uncommon and do not affect
/// schema extraction).
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Parse a `message Name {` header line.
///
/// Returns `Some(name)` when the line starts with `message ` and contains an
/// identifier followed by optional whitespace and `{`.
fn parse_message_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("message")?;
    // Require at least one whitespace between `message` and the name.
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    // Extract the identifier (message name).
    let name_end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }
    let name = &rest[..name_end];
    // The rest should eventually contain `{`; we don't enforce it here
    // because the brace-tracking in the main loop already handles that.
    Some(name.to_string())
}

/// Parse a proto field declaration line, returning `(field_name, type_token)`.
///
/// Expected forms (after comment stripping and trimming):
/// ```text
/// <type> <name> = <number> [<options>] ;
/// <modifier> <type> <name> = <number> [<options>] ;
/// ```
///
/// Returns `None` for any line that doesn't match (enum literals, option
/// lines, `oneof`/`map<K,V>` keywords, etc.).
fn parse_field_line(line: &str) -> Option<(String, &str)> {
    // Must end with `;` (after trimming) to be a field declaration.
    let line = line.strip_suffix(';').map(str::trim_end).unwrap_or(line);

    // Tokenise: split on whitespace, then strip the `= <number>` tail and any
    // option bracket tail `[...]`.
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }

    // Consume optional leading modifier.
    let (type_token, rest_tokens) = if PROTOBUF_FIELD_MODIFIERS.contains(&tokens[0]) {
        if tokens.len() < 4 {
            return None;
        }
        (tokens[1], &tokens[2..])
    } else {
        (tokens[0], &tokens[1..])
    };

    // Reject keywords that start non-field constructs.
    match type_token {
        "message" | "enum" | "oneof" | "option" | "reserved" | "extensions" | "import"
        | "syntax" | "package" | "service" | "rpc" | "returns" => return None,
        _ => {}
    }

    // Reject `map<K,V>` type token — map fields are a single token containing `<`.
    if type_token.starts_with("map<") || type_token == "map" {
        return None;
    }

    // rest_tokens[0] should be the field name, rest_tokens[1] should be `=`.
    let field_name = rest_tokens.first()?;
    let eq_token = rest_tokens.get(1)?;
    if *eq_token != "=" {
        return None;
    }

    // Validate field name: must be a proto identifier (alphanumeric + `_`).
    if field_name.is_empty() || !field_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    // Reject if type_token looks like a number (proto reserved range / enum val).
    if type_token
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
    {
        return None;
    }

    Some((field_name.to_string(), type_token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comment_basic() {
        assert_eq!(
            strip_line_comment("string name = 1; // comment"),
            "string name = 1; "
        );
        assert_eq!(strip_line_comment("// full line"), "");
        assert_eq!(strip_line_comment("no comment"), "no comment");
    }

    #[test]
    fn message_header_parses() {
        assert_eq!(
            parse_message_header("message User {"),
            Some("User".to_string())
        );
        assert_eq!(
            parse_message_header("message  SpacedName{"),
            Some("SpacedName".to_string())
        );
        assert_eq!(parse_message_header("enum Foo {"), None);
        assert_eq!(parse_message_header("messageUser {"), None);
    }

    #[test]
    fn field_line_simple() {
        let (name, ty) = parse_field_line("string email = 1;").unwrap();
        assert_eq!(name, "email");
        assert_eq!(ty, "string");
    }

    #[test]
    fn field_line_with_modifier() {
        let (name, ty) = parse_field_line("repeated int32 ids = 2;").unwrap();
        assert_eq!(name, "ids");
        assert_eq!(ty, "int32");
    }

    #[test]
    fn field_line_rejects_keywords() {
        assert!(parse_field_line("option java_package = \"com.example\";").is_none());
        assert!(parse_field_line("oneof payload {").is_none());
    }
}
