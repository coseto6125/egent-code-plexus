//! Hand-rolled `.proto` lexer for T4-5 schema-field extraction.
//!
//! Handles proto2 / proto3 field declarations inside top-level `message`
//! blocks.  See `mod.rs` for the full list of acknowledged limitations.

use super::schema_extractors::{
    classify_protobuf_type, PROTOBUF_FIELD_MODIFIERS, PROTOBUF_FRAMEWORK,
};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawRoute, RawSchemaField};
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

        let fields = extract_proto_fields(text);
        let schema_fields = (!fields.is_empty()).then(|| fields.into_boxed_slice());

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            schema_fields,
            routes: extract_proto_services(text),
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
fn extract_proto_fields(text: &str) -> Vec<RawSchemaField> {
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
                name: field_name.into_boxed_str(),
                type_class,
                owner_class: Box::from(owner.as_str()),
                framework: PROTOBUF_FRAMEWORK,
                span,
            });
        }
    }

    out
}

/// Line-oriented `service { rpc … }` extractor — gRPC service contracts.
///
/// Emits one [`RawRoute`] per `rpc` method so the graph builder finalizes it
/// into a `NodeKind::Route` (same node kind as an HTTP endpoint — an rpc IS a
/// service endpoint). Reusing `Route` lets gRPC services flow through the
/// existing route/contract tooling (`ecp routes`, `ecp contracts`) with no
/// schema change, closing the graph-completeness gap where a `service` block
/// was previously invisible (only `message` fields were captured).
///
/// `method` is the literal `"GRPC"`; `path` follows the gRPC HTTP/2 wire
/// convention `/<package.>Service/Method`, so a `Fetches`-style consumer edge
/// or cross-repo contract match keys on the same string a gRPC stub call uses.
///
/// Mirrors [`extract_proto_fields`]' state machine: top-level `package`
/// sets the path prefix, a depth-0 `service Name {` opens a service context,
/// and `rpc` lines are read only at `depth == 1` inside that service.
fn extract_proto_services(text: &str) -> Vec<RawRoute> {
    let mut out: Vec<RawRoute> = Vec::new();
    let mut package: Option<String> = None;
    let mut current_service: Option<String> = None;
    let mut depth: u32 = 0;

    for (line_idx, raw_line) in text.lines().enumerate() {
        let row = line_idx as u32;
        let line = strip_line_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        // `package foo.bar;` is only meaningful at the top level (depth 0).
        if depth == 0 && current_service.is_none() {
            if let Some(pkg) = parse_package_line(line) {
                package = Some(pkg);
            }
        }

        let opens = line.chars().filter(|&c| c == '{').count() as u32;
        let closes = line.chars().filter(|&c| c == '}').count() as u32;

        if depth == 0 {
            if let Some(name) = parse_service_header(line) {
                current_service = Some(name);
            }
        }

        depth = depth.saturating_add(opens).saturating_sub(closes);

        if depth == 0 {
            current_service = None;
        }

        // rpc methods live at depth 1 inside a known service.
        let Some(ref service) = current_service else {
            continue;
        };
        if depth != 1 {
            continue;
        }

        if let Some(method_name) = parse_rpc_line(line) {
            let path = match &package {
                Some(pkg) => format!("/{pkg}.{service}/{method_name}"),
                None => format!("/{service}/{method_name}"),
            };
            out.push(RawRoute {
                method: "GRPC".to_string(),
                path,
                handler: None,
                span: (row, 0u32, row, line.len() as u32),
            });
        }
    }

    out
}

/// Parse a top-level `package foo.bar;` line, returning the dotted package name.
fn parse_package_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("package")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let name = rest.trim().strip_suffix(';')?.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    Some(name.to_string())
}

/// Parse a `service Name {` header, returning the service name.
fn parse_service_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("service")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    let name_end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }
    Some(rest[..name_end].to_string())
}

/// Parse an `rpc Method(Req) returns (Resp);` line, returning the method name.
///
/// Tolerates `stream` modifiers and arbitrary whitespace; the request/response
/// message types are not captured (the rpc node carries the method identity —
/// the message shapes are already separate `message` schema-field nodes).
fn parse_rpc_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("rpc")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    // Method name runs up to `(` or whitespace.
    let name_end = rest
        .find(|c: char| c == '(' || c.is_whitespace())
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }
    let name = &rest[..name_end];
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_string())
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

    #[test]
    fn service_header_parses() {
        assert_eq!(
            parse_service_header("service Greeter {"),
            Some("Greeter".to_string())
        );
        assert_eq!(parse_service_header("message User {"), None);
        assert_eq!(parse_service_header("serviceGreeter {"), None);
    }

    #[test]
    fn package_line_parses() {
        assert_eq!(
            parse_package_line("package routeguide.v1;"),
            Some("routeguide.v1".to_string())
        );
        assert_eq!(parse_package_line("package;"), None);
        assert_eq!(parse_package_line("packagefoo;"), None);
    }

    #[test]
    fn rpc_line_parses() {
        assert_eq!(
            parse_rpc_line("rpc SayHello(HelloRequest) returns (HelloReply);"),
            Some("SayHello".to_string())
        );
        assert_eq!(
            parse_rpc_line("rpc ListFeatures(Rectangle) returns (stream Feature) {}"),
            Some("ListFeatures".to_string())
        );
        assert_eq!(parse_rpc_line("string name = 1;"), None);
        assert_eq!(parse_rpc_line("rpcFoo()"), None);
    }

    #[test]
    fn service_with_package_emits_grpc_route() {
        let proto = "\
package helloworld;

service Greeter {
  rpc SayHello (HelloRequest) returns (HelloReply);
  rpc SayHelloAgain (HelloRequest) returns (HelloReply);
}
";
        let routes = extract_proto_services(proto);
        assert_eq!(routes.len(), 2);
        assert!(routes.iter().all(|r| r.method == "GRPC"));
        assert_eq!(routes[0].path, "/helloworld.Greeter/SayHello");
        assert_eq!(routes[1].path, "/helloworld.Greeter/SayHelloAgain");
    }

    #[test]
    fn service_without_package_omits_prefix() {
        let proto = "service Echo {\n  rpc Ping(Req) returns (Resp);\n}\n";
        let routes = extract_proto_services(proto);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/Echo/Ping");
    }

    #[test]
    fn streaming_rpc_captured() {
        let proto = "\
package route_guide;
service RouteGuide {
  rpc RecordRoute(stream Point) returns (RouteSummary) {}
  rpc RouteChat(stream RouteNote) returns (stream RouteNote) {}
}
";
        let routes = extract_proto_services(proto);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].path, "/route_guide.RouteGuide/RecordRoute");
        assert_eq!(routes[1].path, "/route_guide.RouteGuide/RouteChat");
    }

    #[test]
    fn message_only_proto_emits_no_routes() {
        let proto = "\
package m;
message User {
  string name = 1;
  rpc not_a_real_rpc = 2;
}
";
        // A `message`-only file (even one with an `rpc`-looking field name) must
        // not produce any gRPC route — `rpc` is only meaningful inside `service`.
        assert!(extract_proto_services(proto).is_empty());
    }

    #[test]
    fn multiple_services_in_one_file() {
        let proto = "\
package api;
service A {
  rpc One(X) returns (Y);
}
service B {
  rpc Two(X) returns (Y);
}
";
        let routes = extract_proto_services(proto);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].path, "/api.A/One");
        assert_eq!(routes[1].path, "/api.B/Two");
    }
}
