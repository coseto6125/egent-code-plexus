use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_php(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = PhpProvider::new().expect("PhpProvider::new");
    provider
        .parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── eval: always blind ──

#[test]
fn php_eval_emits_blind_spot() {
    let src = "<?php function run($code) { eval($code); }";
    let g = parse_php(src);
    assert!(
        kinds(&g).contains(&"php-eval"),
        "expected php-eval; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn php_eval_with_literal_still_emits_blind_spot() {
    let src = "<?php eval('echo 1;');";
    let g = parse_php(src);
    assert!(
        kinds(&g).contains(&"php-eval"),
        "literal-arg eval still blind; got: {:?}",
        kinds(&g)
    );
}

// ── call_user_func: literal-vs-variable check (Constraint 2) ──

#[test]
fn php_call_user_func_with_variable_emits_blind_spot() {
    let src = "<?php function dispatch($fn, $arg) { call_user_func($fn, $arg); }";
    let g = parse_php(src);
    assert!(
        kinds(&g).contains(&"php-call-user-func"),
        "expected php-call-user-func for variable; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn php_call_user_func_with_literal_skipped() {
    // call_user_func("known_function", ...) is statically resolvable — must
    // NOT emit, per Constraint 2.
    let src = "<?php call_user_func('strlen', 'hello');";
    let g = parse_php(src);
    assert!(
        !kinds(&g).contains(&"php-call-user-func"),
        "literal callable must NOT emit; got: {:?}",
        kinds(&g)
    );
}

// ── variable function call: $func() ──

#[test]
fn php_variable_function_call_emits_blind_spot() {
    let src = "<?php function run($fn) { $fn(); }";
    let g = parse_php(src);
    assert!(
        kinds(&g).contains(&"php-variable-call"),
        "expected php-variable-call for $fn(); got: {:?}",
        kinds(&g)
    );
}

// ── unrelated: NOT blind ──

#[test]
fn php_ordinary_call_emits_no_blind_spot() {
    let src = "<?php function add($a, $b) { return $a + $b; } add(1, 2);";
    let g = parse_php(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}

// ── variable method name: Cls::$m(), $o->$m(), $o?->$m(), $o->{expr}() ──

fn method_kinds(src: &str) -> usize {
    kinds(&parse_php(src))
        .into_iter()
        .filter(|k| *k == "php-variable-method-call")
        .count()
}

/// Every callee the parser attached to a node, for junk-callee checks.
fn all_calls(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

#[test]
fn test_parse_file_scoped_variable_method_call_records_blind_spot() {
    let src = "<?php class A { function f($m) { return Rows::$m(1); } }";
    assert_eq!(method_kinds(src), 1);
}

#[test]
fn test_parse_file_member_variable_method_call_records_blind_spot() {
    let src = "<?php class A { function f($o, $m) { return $o->$m(1); } }";
    assert_eq!(method_kinds(src), 1);
}

#[test]
fn test_parse_file_nullsafe_variable_method_call_records_blind_spot() {
    let src = "<?php class A { function f($o, $m) { return $o?->$m(1); } }";
    assert_eq!(method_kinds(src), 1);
}

#[test]
fn test_parse_file_braced_expression_method_call_records_blind_spot() {
    let src =
        "<?php class A { function f($o, $p) { $o->{$p . 'x'}(); Rows::{$p}(); $o->{FOO}(); } }";
    assert_eq!(method_kinds(src), 3);
}

#[test]
fn test_parse_file_this_variable_method_call_with_param_name_records_blind_spot() {
    let src = "<?php class A { function f($m) { return $this->$m(); } function g() {} }";
    assert_eq!(method_kinds(src), 1);
}

#[test]
fn test_parse_file_plain_method_calls_record_no_variable_method_blind_spot() {
    let src = "<?php class A { function f($o) { Rows::hero(); self::g(); $o->m(); $o?->m(); $this->g(); } function g() {} }";
    assert_eq!(method_kinds(src), 0);
}

#[test]
fn test_parse_file_variable_method_call_emits_no_junk_callee() {
    let src = "<?php class A { function f($o, $m, $p) { Rows::$m(); $o->$m(); $o?->$m(); $o->{$p . 'x'}(); $o->{FOO}(); } }";
    let g = parse_php(src);
    let calls = all_calls(&g);
    assert!(
        calls.iter().all(|c| !c.contains('$') && !c.contains("FOO")),
        "a runtime method name must not become a callee; got: {calls:?}"
    );
}
