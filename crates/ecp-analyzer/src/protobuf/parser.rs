//! Hand-rolled `.proto` lexer for T4-5 schema-field extraction.
//!
//! Handles proto2 / proto3 field declarations inside top-level `message`
//! blocks.  See `mod.rs` for the full list of acknowledged limitations.

use super::schema_extractors::{
    classify_protobuf_type, PROTOBUF_FIELD_MODIFIERS, PROTOBUF_FRAMEWORK,
};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode, RawRoute, RawSchemaField};
use ecp_core::graph::NodeKind;
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

        let (fields, messages) = extract_proto_fields(text);
        let schema_fields = (!fields.is_empty()).then(|| fields.into_boxed_slice());

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes: messages,
            schema_fields,
            routes: extract_proto_services(text),
            ..Default::default()
        })
    }
}

/// One logical proto statement: the source row it starts on, plus the cleaned
/// text. Cleaning removes comments (`//` and `/* … */`) and neutralizes string
/// literals to a placeholder, then splits the raw source at `{`, `}`, and `;`
/// boundaries so every brace/terminator is its own statement. Downstream
/// parsing then never has to count braces inside strings/comments or cope with
/// multiple statements sharing a line (`message Foo { string a = 1; }`).
struct Statement {
    row: u32,
    text: String,
}

/// Lex `text` into [`Statement`]s, the load-bearing robustness primitive.
///
/// A single char-level pass tracks three lexical states — inside a `"…"`
/// string, inside a `//` line comment, inside a `/* … */` block comment — so a
/// brace or `//` inside a string (e.g. `google.api.http = { post: "/v1/{x}" }`)
/// is NOT mistaken for structure. String contents collapse to a single `"`
/// placeholder (their bytes never carry structural meaning here). The cleaned
/// stream is split at `{`, `}`, `;` so each is emitted as its own one-char
/// statement, and the run of identifier/whitespace text between delimiters is
/// emitted as a statement carrying its starting row.
fn lex_statements(text: &str) -> Vec<Statement> {
    let mut out: Vec<Statement> = Vec::new();
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut row: u32 = 0;
    let mut buf = String::new();
    let mut buf_row: u32 = 0;
    let mut in_block_comment = false;

    let flush = |buf: &mut String, buf_row: u32, out: &mut Vec<Statement>| {
        if !buf.trim().is_empty() {
            out.push(Statement {
                row: buf_row,
                text: buf.trim().to_string(),
            });
        }
        buf.clear();
    };

    while i < n {
        let b = bytes[i];
        if b == b'\n' {
            row += 1;
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // Comment starts.
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            // Line comment — skip to end of line.
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            in_block_comment = true;
            i += 2;
            continue;
        }
        // String literal — collapse to a placeholder, skip its bytes (incl.
        // escapes) so inner `{`, `}`, `//` never reach the structure logic.
        if b == b'"' || b == b'\'' {
            let quote = b;
            if buf.trim().is_empty() {
                buf_row = row;
            }
            buf.push('"');
            i += 1;
            while i < n && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                } else {
                    if bytes[i] == b'\n' {
                        row += 1;
                    }
                    i += 1;
                }
            }
            i += 1; // closing quote (or EOF)
            continue;
        }
        // Structural delimiters — flush the pending run, emit the delimiter.
        if b == b'{' || b == b'}' || b == b';' {
            flush(&mut buf, buf_row, &mut out);
            out.push(Statement {
                row,
                text: (b as char).to_string(),
            });
            i += 1;
            continue;
        }
        // Ordinary content byte.
        if buf.trim().is_empty() && b != b' ' && b != b'\t' && b != b'\r' {
            buf_row = row;
        }
        buf.push(b as char);
        i += 1;
    }
    flush(&mut buf, buf_row, &mut out);
    out
}

/// Build the [`RawSchemaField`]s and owner [`RawNode`] (`NodeKind::Struct`) for
/// every top-level `message`.
///
/// The Struct node is load-bearing: `schema_field_mirrors` resolves each
/// `RawSchemaField.owner_class` against the SymbolTable to attach a
/// `HasProperty` edge, and silently drops fields whose owner isn't a known
/// node. `Struct` (not `Class`) because a proto message is a value-type
/// aggregate with no inheritance / vtable — LLMs must not pattern-match OO
/// conventions.
///
/// Runs over [`lex_statements`], so braces inside strings/comments and
/// single-line bodies are handled correctly. `block_depth` is the brace nesting
/// relative to the top-level message: a field is attributed to the message at
/// ANY depth ≥ 1 (so `oneof` / nested option blocks still contribute fields to
/// the enclosing message). Only messages that own ≥1 field get a Struct node;
/// the pending message is also flushed at EOF so a truncated file doesn't drop
/// the owner node (which would re-orphan its fields downstream).
/// The open top-level `message` while walking statements: from its header
/// until the matching `}`. `start_row` anchors the Struct node span, extended
/// to the closing brace's row at flush.
struct PendingMsg {
    name: String,
    start_row: u32,
    has_field: bool,
}

fn extract_proto_fields(text: &str) -> (Vec<RawSchemaField>, Vec<RawNode>) {
    let mut out: Vec<RawSchemaField> = Vec::new();
    let mut messages: Vec<RawNode> = Vec::new();
    let mut pending: Option<PendingMsg> = None;
    // Whether the top-level message's body `{` has been seen (the header and
    // its opening brace are separate statements).
    let mut body_open = false;
    // Block-kind stack for blocks nested INSIDE the message body. `true` ⟺ a
    // nested `message` block, whose fields belong to that nested message
    // (skipped in v1). `false` ⟺ a transparent block (`oneof`, option struct,
    // `map` entry) whose fields ARE the enclosing message's (proto3 `oneof` is
    // a mutual-exclusion grouping of the message's own fields). A field counts
    // only when no nested-message frame is open.
    let mut block_stack: Vec<bool> = Vec::new();
    // Whether the immediately-preceding statement was a nested `message` header
    // (so the next `{` opens a nested-message frame).
    let mut next_block_is_message = false;

    for stmt in lex_statements(text) {
        let line = stmt.text.as_str();
        let row = stmt.row;

        match line {
            "{" if pending.is_some() => {
                if body_open {
                    block_stack.push(next_block_is_message);
                } else {
                    body_open = true; // the message body's own opening brace
                }
                next_block_is_message = false;
            }
            "}" if pending.is_some() => {
                if block_stack.pop().is_none() {
                    // Closed the message body itself.
                    let p = pending.take().unwrap();
                    body_open = false;
                    if p.has_field {
                        messages.push(message_struct_node(p.name, (p.start_row, 0, row, 0)));
                    }
                }
            }
            "{" | "}" | ";" => {} // braces/terminators outside any message
            _ => {
                next_block_is_message = false;
                if pending.is_none() {
                    if let Some(name) = parse_message_header(line) {
                        pending = Some(PendingMsg {
                            name,
                            start_row: row,
                            has_field: false,
                        });
                    }
                    continue;
                }
                // A nested `message Inner {` header — its fields belong to Inner
                // (skipped in v1), so flag the upcoming block as a message frame.
                if parse_message_header(line).is_some() {
                    next_block_is_message = true;
                    continue;
                }
                // Inside a nested message frame, fields are not the enclosing
                // message's — skip.
                if block_stack.iter().any(|&is_msg| is_msg) {
                    continue;
                }
                if let Some((field_name, type_token)) = parse_field_line(line) {
                    let type_class = classify_protobuf_type(type_token);
                    let span = (row, 0u32, row, line.len() as u32);
                    let owner = &pending.as_ref().unwrap().name;
                    out.push(RawSchemaField {
                        name: field_name.into_boxed_str(),
                        type_class,
                        owner_class: Box::from(owner.as_str()),
                        framework: PROTOBUF_FRAMEWORK,
                        span,
                    });
                    pending.as_mut().unwrap().has_field = true;
                }
            }
        }
    }
    // EOF flush: a truncated/unclosed message still gets its owner Struct node
    // so its already-emitted fields aren't dropped at schema_field_mirrors. The
    // span ends at its last seen field (no closing brace exists).
    if let Some(p) = pending.take() {
        if p.has_field {
            let end = out.last().map(|f| f.span.0).unwrap_or(p.start_row);
            messages.push(message_struct_node(p.name, (p.start_row, 0, end, 0)));
        }
    }

    (out, messages)
}

/// Build the `NodeKind::Struct` node for a proto `message` (the owner of its
/// schema fields). `owner_class: None` — a top-level message is not nested in
/// another type.
fn message_struct_node(name: String, span: (u32, u32, u32, u32)) -> RawNode {
    RawNode {
        name,
        kind: NodeKind::Struct,
        span,
        is_exported: true,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
        field_reads: vec![],
        owner_class: None,
        content_hash: 0,
    }
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
/// Build a [`RawRoute`] per gRPC `rpc` method, keyed by `service` + `package`.
///
/// Runs over [`lex_statements`] (string/comment-safe, single-line-body-safe).
/// `current_service` is `Some` while inside a service block; `block_depth`
/// tracks brace nesting within it so an rpc with a body block (`rpc M(..) {..}`)
/// still registers the method (read on the rpc statement itself, before its
/// `{`), and per-rpc option blocks don't desync the service scope. `package`
/// is captured wherever it appears at top level (a proto `package` directive is
/// position-independent), so a `package` after a service still prefixes paths.
fn extract_proto_services(text: &str) -> Vec<RawRoute> {
    let mut out: Vec<RawRoute> = Vec::new();
    let mut package: Option<String> = None;
    let mut current_service: Option<String> = None;
    let mut block_depth: u32 = 0;

    for stmt in lex_statements(text) {
        let line = stmt.text.as_str();
        let row = stmt.row;

        match line {
            "{" => {
                if current_service.is_some() {
                    block_depth += 1;
                }
            }
            "}" => {
                if current_service.is_some() {
                    block_depth = block_depth.saturating_sub(1);
                    if block_depth == 0 {
                        current_service = None;
                    }
                }
            }
            _ => {
                // `package` is top-level and position-independent.
                if current_service.is_none() {
                    if let Some(pkg) = parse_package_line(line) {
                        package = Some(pkg);
                        continue;
                    }
                    if let Some(name) = parse_service_header(line) {
                        current_service = Some(name);
                        continue;
                    }
                    continue;
                }
                let service = current_service.as_ref().unwrap();
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
        }
    }

    out
}

/// Parse a top-level `package foo.bar` statement, returning the dotted package
/// name. The `;` terminator is split off by `lex_statements`; a trailing `;` is
/// tolerated for direct callers / unit tests.
///
/// Rejects malformed dotted names — leading dot, trailing dot, or consecutive
/// dots — so a bad `package foo.` can never produce a double-dot wire path like
/// `/foo..Service/Method`.
fn parse_package_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("package")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let name = rest.trim().strip_suffix(';').unwrap_or(rest.trim()).trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        || name.starts_with('.')
        || name.ends_with('.')
        || name.contains("..")
    {
        return None;
    }
    Some(name.to_string())
}

/// Parse a `<keyword> <identifier>…` declaration, returning the identifier.
///
/// Shared skeleton for `service` / `message` / `rpc`: require the keyword
/// followed by whitespace, then read the identifier up to the first char that
/// is neither alphanumeric nor `_` (rpc additionally stops at `(`). Returns
/// `None` if the keyword/whitespace/identifier shape doesn't hold.
fn parse_keyword_ident(line: &str, keyword: &str) -> Option<String> {
    let rest = line.strip_prefix(keyword)?;
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

/// Parse a `service Name {` header, returning the service name.
fn parse_service_header(line: &str) -> Option<String> {
    parse_keyword_ident(line, "service")
}

/// Parse a `message Name {` header line, returning the message name.
fn parse_message_header(line: &str) -> Option<String> {
    parse_keyword_ident(line, "message")
}

/// Parse an `rpc Method(Req) returns (Resp)` statement, returning the method
/// name. The `(` immediately after the method name is a valid terminator (the
/// shared `parse_keyword_ident` stop set already treats `(` as a non-identifier
/// char), so request/response types are naturally excluded.
fn parse_rpc_line(line: &str) -> Option<String> {
    parse_keyword_ident(line, "rpc")
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
    // The `;` terminator is already split off by `lex_statements`; tolerate a
    // trailing one for direct callers / unit tests.
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
