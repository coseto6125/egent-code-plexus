//! `send` / `public_send` / `__send__` with a literal method name is as static
//! as the ordinary call of the same name: it records the call the ordinary
//! call records, so the resolver binds it and `ecp impact` on the target lists
//! the caller. A non-literal or malformed name stays a blind spot with no edge.
mod instantiation_calls_support;

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use instantiation_calls_support::{calls_from, graph_of};
use std::path::Path;

const HELPER: (&str, &str) = ("lib/helper.rb", "def helper; end\ndef other; end\n");
/// A receiver call reaches a method, never a top-level `def` (an untyped
/// member never binds a free function), so the receiver forms target `ping`.
const SVC: (&str, &str) = ("lib/svc.rb", "class Svc\n  def ping; end\nend\n");

/// `name` of every `Calls` target of `go`, minus `send` itself.
fn targets_of_body(body: &str) -> Vec<String> {
    let src = format!("def go(obj, name)\n  {body}\nend\n");
    let graph = graph_of(
        &RubyProvider::new().unwrap(),
        &[HELPER, SVC, ("app.rb", &src)],
    );
    calls_from(&graph, "go")
        .into_iter()
        .map(|h| h.name)
        .filter(|n| !n.ends_with("send"))
        .collect()
}

fn blind_kinds(body: &str) -> Vec<String> {
    let src = format!("def go(obj, name)\n  {body}\nend\n");
    RubyProvider::new()
        .unwrap()
        .parse_file(Path::new("t.rb"), src.as_bytes())
        .unwrap()
        .blind_spots
        .into_iter()
        .map(|b| b.kind)
        .collect()
}

fn helper() -> Vec<String> {
    vec!["helper".to_string()]
}

fn ping() -> Vec<String> {
    vec!["ping".to_string()]
}

#[test]
fn test_send_symbol_without_receiver_calls_method() {
    assert_eq!(targets_of_body("send(:helper)"), helper());
}

#[test]
fn test_public_send_symbol_without_receiver_calls_method() {
    assert_eq!(targets_of_body("public_send(:helper)"), helper());
}

#[test]
fn test_send_string_without_receiver_calls_method() {
    assert_eq!(targets_of_body("send(\"helper\")"), helper());
}

#[test]
fn test_send_symbol_with_receiver_calls_method() {
    assert_eq!(targets_of_body("obj.send(:ping)"), ping());
}

#[test]
fn test_send_string_with_receiver_calls_method() {
    assert_eq!(targets_of_body("obj.send('ping')"), ping());
}

#[test]
fn test_send_with_literal_keeps_extra_arguments_and_calls_method() {
    assert_eq!(targets_of_body("obj.public_send(:ping, 1, 2)"), ping());
}

#[test]
fn test_send_literal_emits_no_blind_spot() {
    for body in ["obj.send(:helper)", "obj.send(\"helper\")", "send(:helper)"] {
        assert!(
            blind_kinds(body).is_empty(),
            "{body}: {:?}",
            blind_kinds(body)
        );
    }
}

/// A setter or an operator is a method name too: the send is static.
#[test]
fn test_send_setter_and_operator_literal_emits_no_blind_spot() {
    for body in [
        "obj.send(:name=, 1)",
        "obj.send(:[], 0)",
        "obj.send(:<=>, 1)",
    ] {
        assert!(
            blind_kinds(body).is_empty(),
            "{body}: {:?}",
            blind_kinds(body)
        );
    }
}

#[test]
fn test_send_variable_target_keeps_blind_spot_and_no_edge() {
    assert_eq!(blind_kinds("obj.send(name)"), vec!["rb-send"]);
    assert!(targets_of_body("obj.send(name)").is_empty());
    assert!(targets_of_body("send(name)").is_empty());
}

#[test]
fn test_send_interpolated_string_keeps_blind_spot_and_no_edge() {
    let body = "obj.send(\"help#{name}\")";
    assert_eq!(blind_kinds(body), vec!["rb-send"]);
    assert!(targets_of_body(body).is_empty());
}

#[test]
fn test_send_invalid_literal_keeps_blind_spot_and_no_edge() {
    for body in ["obj.send('not a name')", "obj.send('')", "obj.send(:'a b')"] {
        assert_eq!(blind_kinds(body), vec!["rb-send"], "{body}");
        assert!(targets_of_body(body).is_empty(), "{body}");
    }
}

#[test]
fn test_ordinary_calls_still_resolve_after_literal_send_change() {
    assert_eq!(targets_of_body("helper()"), helper());
    assert_eq!(targets_of_body("obj.ping"), ping());
}
