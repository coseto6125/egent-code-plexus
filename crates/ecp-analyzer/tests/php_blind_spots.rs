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
