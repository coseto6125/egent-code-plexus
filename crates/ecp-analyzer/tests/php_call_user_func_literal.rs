//! `call_user_func` / `call_user_func_array` with a literal callable is as
//! static as the ordinary call of the same shape: it records the call the
//! ordinary call records, so the resolver binds it and `ecp impact` on the
//! target lists the caller. A non-literal or malformed target stays a blind
//! spot with no edge.
mod instantiation_calls_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use instantiation_calls_support::{calls_from, graph_of};
use std::path::Path;

const ROWS: (&str, &str) = (
    "src/Rows.php",
    "<?php namespace App; class Rows { public static function hero() {} public function inst() {} }",
);
const HELPER: (&str, &str) = ("src/helper.php", "<?php function helper() {}");

/// `(owner, name)` of every `Calls` target of `caller`, minus the
/// `call_user_func*` builtins themselves.
fn targets(files: &[(&str, &str)], caller: &str) -> Vec<(String, String)> {
    let graph = graph_of(&PhpProvider::new().unwrap(), files);
    calls_from(&graph, caller)
        .into_iter()
        .filter(|h| !h.name.starts_with("call_user_func"))
        .map(|h| (h.owner, h.name))
        .collect()
}

fn blind_kinds(src: &str) -> Vec<String> {
    PhpProvider::new()
        .unwrap()
        .parse_file(Path::new("t.php"), src.as_bytes())
        .unwrap()
        .blind_spots
        .into_iter()
        .map(|b| b.kind)
        .collect()
}

fn caller(body: &str) -> (String, String) {
    (
        "src/app.php".to_string(),
        format!("<?php namespace App; use App\\Rows; function go($args, $fn) {{ {body} }}"),
    )
}

fn targets_of_body(body: &str) -> Vec<(String, String)> {
    let (path, src) = caller(body);
    targets(&[ROWS, HELPER, (&path, &src)], "go")
}

fn rows_hero() -> Vec<(String, String)> {
    vec![("Rows".to_string(), "hero".to_string())]
}

#[test]
fn test_call_user_func_plain_function_literal_calls_function() {
    let got = targets_of_body("call_user_func('helper');");
    assert_eq!(got, vec![(String::new(), "helper".to_string())]);
}

#[test]
fn test_call_user_func_array_function_literal_calls_function() {
    let got = targets_of_body("call_user_func_array('helper', $args);");
    assert_eq!(got, vec![(String::new(), "helper".to_string())]);
}

#[test]
fn test_call_user_func_class_string_literal_calls_static_method() {
    let got = targets_of_body(r"call_user_func('Rows::hero');");
    assert_eq!(got, rows_hero());
}

#[test]
fn test_call_user_func_qualified_class_string_escaped_backslash_calls_static_method() {
    let got = targets_of_body(r"call_user_func('App\\Rows::hero');");
    assert_eq!(got, rows_hero());
}

#[test]
fn test_call_user_func_qualified_class_string_leading_backslash_calls_static_method() {
    let got = targets_of_body(r"call_user_func('\\App\\Rows::hero');");
    assert_eq!(got, rows_hero());
}

#[test]
fn test_call_user_func_class_constant_array_calls_static_method() {
    let got = targets_of_body("call_user_func([Rows::class, 'hero']);");
    assert_eq!(got, rows_hero());
}

#[test]
fn test_call_user_func_array_class_constant_array_calls_static_method() {
    let got = targets_of_body("call_user_func_array([Rows::class, 'hero'], $args);");
    assert_eq!(got, rows_hero());
}

#[test]
fn test_call_user_func_class_string_array_calls_static_method() {
    let got = targets_of_body(r"call_user_func(['App\\Rows', 'hero']);");
    assert_eq!(got, rows_hero());
}

#[test]
fn test_call_user_func_this_array_calls_enclosing_class_method() {
    let src = "<?php namespace App; class Svc { function hero() {} function go() { call_user_func([$this, 'hero']); } }";
    let got = targets(&[("src/svc.php", src)], "go");
    assert_eq!(got, vec![("Svc".to_string(), "hero".to_string())]);
}

#[test]
fn test_call_user_func_literal_emits_no_blind_spot() {
    for body in [
        "call_user_func('helper');",
        "call_user_func('Rows::hero');",
        "call_user_func([Rows::class, 'hero']);",
    ] {
        let (_, src) = caller(body);
        assert!(
            blind_kinds(&src).is_empty(),
            "{body}: {:?}",
            blind_kinds(&src)
        );
    }
}

#[test]
fn test_call_user_func_variable_target_keeps_blind_spot_and_no_edge() {
    let (_, src) = caller("call_user_func($fn, $args);");
    assert_eq!(blind_kinds(&src), vec!["php-call-user-func"]);
    assert!(targets_of_body("call_user_func($fn, $args);").is_empty());
}

#[test]
fn test_call_user_func_variable_array_class_keeps_blind_spot_and_no_edge() {
    let body = "call_user_func([$fn, 'hero']);";
    let (_, src) = caller(body);
    assert_eq!(blind_kinds(&src), vec!["php-call-user-func"]);
    assert!(targets_of_body(body).is_empty());
}

#[test]
fn test_call_user_func_invalid_literal_keeps_blind_spot_and_no_edge() {
    for body in [
        "call_user_func('not a name');",
        "call_user_func('');",
        "call_user_func('Rows::');",
        "call_user_func('Rows::bad name');",
        "call_user_func([Rows::class, 'bad name']);",
    ] {
        let (_, src) = caller(body);
        assert_eq!(blind_kinds(&src), vec!["php-call-user-func"], "{body}");
        assert!(targets_of_body(body).is_empty(), "{body}");
    }
}

#[test]
fn test_call_user_func_array_invalid_literal_no_edge() {
    assert!(targets_of_body("call_user_func_array('not a name', $args);").is_empty());
}

#[test]
fn test_ordinary_calls_still_resolve_after_literal_callable_change() {
    assert_eq!(targets_of_body("Rows::hero();"), rows_hero());
    assert_eq!(
        targets_of_body("helper();"),
        vec![(String::new(), "helper".to_string())]
    );
}
