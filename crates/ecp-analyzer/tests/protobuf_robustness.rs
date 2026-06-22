//! Robustness regression suite for the protobuf provider — the real-world
//! proto shapes that the first line-oriented + naive-brace-count implementation
//! silently mis-parsed (each one verified live during code review):
//!
//! - single-line `message`/`service` bodies
//! - per-rpc/per-message option blocks whose string values contain `{`/`}`
//!   (e.g. `google.api.http = { post: "/v1/{name}" }`)
//! - multi-line rpc bodies (`rpc X(A) returns (B) { ... }`)
//! - `oneof`-only messages (proto3 union types)
//! - truncated / unclosed messages at EOF
//! - block comments (`/* … */`) and `//` inside string literals
//! - malformed package names (leading/trailing/double dots)
//!
//! All assertions go through `ProtobufProvider::parse_file` — the real
//! provider entry point.

use ecp_analyzer::protobuf::ProtobufProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    ProtobufProvider::new()
        .expect("provider")
        .parse_file(Path::new("t.proto"), src.as_bytes())
        .expect("parse")
}

fn route_paths(lg: &LocalGraph) -> Vec<String> {
    lg.routes.iter().map(|r| r.path.clone()).collect()
}

fn field_names(lg: &LocalGraph) -> Vec<String> {
    lg.schema_fields
        .as_ref()
        .map(|fs| fs.iter().map(|f| f.name.to_string()).collect())
        .unwrap_or_default()
}

fn struct_names(lg: &LocalGraph) -> Vec<String> {
    lg.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Struct)
        .map(|n| n.name.clone())
        .collect()
}

// ── single-line bodies ───────────────────────────────────────────────────────

#[test]
fn single_line_message_emits_struct_and_field() {
    let lg = parse("message Foo { string a = 1; }\n");
    assert_eq!(struct_names(&lg), vec!["Foo".to_string()]);
    assert_eq!(field_names(&lg), vec!["a".to_string()]);
}

#[test]
fn single_line_service_emits_route() {
    let lg = parse("package p;\nservice Foo { rpc M(A) returns (B); }\n");
    assert_eq!(route_paths(&lg), vec!["/p.Foo/M".to_string()]);
}

// ── per-rpc options with string-literal braces ───────────────────────────────

#[test]
fn rpc_with_http_option_does_not_drop_following_rpc() {
    let src = "\
package p;
service Greeter {
  rpc SayHello(Req) returns (Resp) {
    option (google.api.http) = { post: \"/v1/{name}\" };
  }
  rpc Second(Req) returns (Resp);
}
";
    let mut paths = route_paths(&parse(src));
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "/p.Greeter/SayHello".to_string(),
            "/p.Greeter/Second".to_string()
        ]
    );
}

#[test]
fn multiline_rpc_body_captures_the_rpc() {
    let src = "\
package p;
service S {
  rpc First(A) returns (B) {
    option deadline = 5;
  }
  rpc Second(A) returns (B);
}
";
    let mut paths = route_paths(&parse(src));
    paths.sort();
    assert_eq!(
        paths,
        vec!["/p.S/First".to_string(), "/p.S/Second".to_string()]
    );
}

#[test]
fn message_option_with_brace_in_string_keeps_fields() {
    let src = "\
message M {
  option (some.opt) = \"{not a brace block\";
  string a = 1;
  int32 b = 2;
}
";
    let lg = parse(src);
    let mut fields = field_names(&lg);
    fields.sort();
    assert_eq!(fields, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(struct_names(&lg), vec!["M".to_string()]);
}

// ── oneof ────────────────────────────────────────────────────────────────────

#[test]
fn oneof_fields_belong_to_enclosing_message() {
    let src = "\
message Result {
  oneof payload {
    string ok = 1;
    string err = 2;
  }
}
";
    let lg = parse(src);
    assert_eq!(struct_names(&lg), vec!["Result".to_string()]);
    let mut fields = field_names(&lg);
    fields.sort();
    assert_eq!(fields, vec!["err".to_string(), "ok".to_string()]);
}

// ── truncated / unclosed at EOF ──────────────────────────────────────────────

#[test]
fn unclosed_message_at_eof_still_emits_struct_for_its_fields() {
    // Fields are emitted during the walk; the owner Struct must be flushed at
    // EOF too, else schema_field_mirrors drops the orphaned fields downstream.
    let lg = parse("message Trunc {\n  string a = 1;\n  int32 b = 2;\n");
    assert_eq!(struct_names(&lg), vec!["Trunc".to_string()]);
    assert_eq!(field_names(&lg).len(), 2);
}

// ── comments ─────────────────────────────────────────────────────────────────

#[test]
fn double_slash_inside_string_is_not_a_comment() {
    let src = "\
message M {
  string url = 1 [(validate.rules).string.pattern = \"https://x\"];
  int32 n = 2;
}
";
    let mut fields = field_names(&parse(src));
    fields.sort();
    assert_eq!(fields, vec!["n".to_string(), "url".to_string()]);
}

#[test]
fn block_comment_lines_are_ignored() {
    let src = "\
/* message Ghost {
   string should_not_appear = 1;
} */
message Real {
  string a = 1;
}
";
    let lg = parse(src);
    assert_eq!(struct_names(&lg), vec!["Real".to_string()]);
    assert_eq!(field_names(&lg), vec!["a".to_string()]);
}

// ── package validation ───────────────────────────────────────────────────────

#[test]
fn malformed_package_with_trailing_dot_is_rejected() {
    // `package foo.;` is malformed — must not produce `/foo..Svc/M`.
    let lg = parse("package foo.;\nservice S {\n  rpc M(A) returns (B);\n}\n");
    // Either reject the package (path `/S/M`) — never a double-dot path.
    let paths = route_paths(&lg);
    assert!(
        paths.iter().all(|p| !p.contains("..")),
        "no double-dot paths: {paths:?}"
    );
}

// ── regression: clean multi-line cases still work ────────────────────────────

#[test]
fn clean_multiline_message_and_service_unchanged() {
    let src = "\
syntax = \"proto3\";
package api.v1;

message User {
  string email = 1;
  int32 age = 2;
}

service UserService {
  rpc GetUser(User) returns (User);
}
";
    let lg = parse(src);
    assert_eq!(struct_names(&lg), vec!["User".to_string()]);
    let mut fields = field_names(&lg);
    fields.sort();
    assert_eq!(fields, vec!["age".to_string(), "email".to_string()]);
    assert_eq!(
        route_paths(&lg),
        vec!["/api.v1.UserService/GetUser".to_string()]
    );
}
