//! A PHP callable handed on as a value is a `References` edge with reason
//! `function_reference` from the function holding it to the function or method
//! it names, never a `Calls` edge. The string rule: a string names a callable
//! only as the callable argument of a known callable-taking builtin
//! (`array_map`, `usort`, `is_callable`, ...), and only when it binds a
//! project symbol; any other string, and any name the project does not
//! declare, records nothing. The contract behind each assertion is the cypher
//! row `MATCH (a)-[r:References]->(b) RETURN a.name, b.name, r.reason` that
//! `ecp impact --direction upstream` reads.
use crate::name_ref_support::{graph, refs, FUNCTION_REFERENCE};

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::graph::RelType;

const CB: (&str, &str) = (
    "src/Cb.php",
    "<?php namespace P; class Cb { public static function cb_qual(int $x): void {} public function cb_ptr(int $x): void {} }",
);
const FUNCTIONS: (&str, &str) = (
    "src/fns.php",
    "<?php namespace P; function cb_bare(int $x): void {}",
);

fn holder(params: &str, body: &str) -> (String, String) {
    (
        "src/Holder.php".to_string(),
        format!(
            "<?php namespace P; use P\\Cb; class Holder {{ public function cb_own($v) {{}} public function m({params}) {{ {body} }} }}"
        ),
    )
}

fn edges(params: &str, body: &str, rel: RelType, reason: &str) -> Vec<String> {
    let (path, src) = holder(params, body);
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[CB, FUNCTIONS, (path.as_str(), src.as_str())],
    );
    refs(&g, "m", rel, reason)
        .into_iter()
        .map(|(_, name, _, _)| name)
        .collect()
}

fn references(body: &str) -> Vec<String> {
    edges(
        "Cb $o, array $a",
        body,
        RelType::References,
        FUNCTION_REFERENCE,
    )
}

fn calls(body: &str) -> Vec<String> {
    edges("Cb $o, array $a", body, RelType::Calls, "call")
}

fn one(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[test]
fn test_string_callable_namespaced_function_references_function() {
    assert_eq!(references("array_map('P\\cb_bare', $a);"), one("cb_bare"));
}

#[test]
fn test_string_callable_bare_function_references_function() {
    assert_eq!(references("array_map('cb_bare', $a);"), one("cb_bare"));
}

#[test]
fn test_string_callable_is_callable_references_function() {
    assert_eq!(references("is_callable('cb_bare');"), one("cb_bare"));
}

#[test]
fn test_first_class_callable_function_references_not_calls() {
    let body = "array_map(cb_bare(...), $a);";
    assert_eq!(references(body), one("cb_bare"));
    assert_eq!(calls(body), Vec::<String>::new());
}

#[test]
fn test_array_callable_class_constant_references_method() {
    assert_eq!(
        references("array_map([Cb::class, 'cb_qual'], $a);"),
        one("cb_qual")
    );
}

#[test]
fn test_array_callable_qualified_string_class_references_method() {
    assert_eq!(
        references("array_map(['P\\Cb', 'cb_qual'], $a);"),
        one("cb_qual")
    );
}

#[test]
fn test_array_callable_this_references_own_method() {
    assert_eq!(
        references("array_map([$this, 'cb_own'], $a);"),
        one("cb_own")
    );
}

#[test]
fn test_array_callable_typed_parameter_references_method_on_its_class() {
    assert_eq!(references("array_map([$o, 'cb_ptr'], $a);"), one("cb_ptr"));
}

#[test]
fn test_array_callable_nullable_typed_parameter_references_method() {
    let got = edges(
        "?Cb $o, array $a",
        "array_map([$o, 'cb_ptr'], $a);",
        RelType::References,
        FUNCTION_REFERENCE,
    );
    assert_eq!(got, one("cb_ptr"));
}

#[test]
fn test_first_class_callable_static_method_references_not_calls() {
    let body = "array_map(Cb::cb_qual(...), $a);";
    assert_eq!(references(body), one("cb_qual"));
    assert_eq!(calls(body), Vec::<String>::new());
}

#[test]
fn test_first_class_callable_this_method_references_not_calls() {
    let body = "array_map($this->cb_own(...), $a);";
    assert_eq!(references(body), one("cb_own"));
    assert_eq!(calls(body), Vec::<String>::new());
}

#[test]
fn test_string_callable_static_method_references_method() {
    assert_eq!(
        references("array_map('P\\Cb::cb_qual', $a);"),
        one("cb_qual")
    );
}

#[test]
fn test_ordinary_calls_stay_calls_not_references() {
    let body = "cb_bare(1); Cb::cb_qual(1); $this->cb_own(1);";
    assert_eq!(references(body), Vec::<String>::new());
    assert_eq!(calls(body).len(), 3);
}

#[test]
fn test_string_argument_to_non_callable_function_records_nothing() {
    assert_eq!(
        references("strlen('cb_bare'); my_log('cb_bare');"),
        Vec::<String>::new()
    );
}

#[test]
fn test_string_callable_unknown_name_records_nothing() {
    assert_eq!(
        references("array_map('no_such_fn', $a);"),
        Vec::<String>::new()
    );
}

#[test]
fn test_string_callable_other_namespace_qualifier_records_nothing() {
    assert_eq!(
        references("array_map('Other\\cb_bare', $a);"),
        Vec::<String>::new()
    );
}

#[test]
fn test_string_callable_empty_and_whitespace_record_nothing() {
    assert_eq!(
        references("array_map('', $a); array_map('  ', $a);"),
        Vec::<String>::new()
    );
}

#[test]
fn test_array_callable_untyped_variable_records_nothing() {
    let got = edges(
        "$x, array $a",
        "array_map([$x, 'cb_ptr'], $a);",
        RelType::References,
        FUNCTION_REFERENCE,
    );
    assert_eq!(got, Vec::<String>::new());
}

#[test]
fn test_array_callable_primitive_typed_variable_records_nothing() {
    let got = edges(
        "int $x, array $a",
        "array_map([$x, 'cb_ptr'], $a);",
        RelType::References,
        FUNCTION_REFERENCE,
    );
    assert_eq!(got, Vec::<String>::new());
}

#[test]
fn test_array_callable_with_three_elements_records_nothing() {
    assert_eq!(
        references("array_map([Cb::class, 'cb_qual', 1], $a);"),
        Vec::<String>::new()
    );
}

#[test]
fn test_first_class_callable_untyped_receiver_records_no_reference() {
    let got = edges(
        "$u, array $a",
        "array_map($u->cb_ptr(...), $a);",
        RelType::References,
        FUNCTION_REFERENCE,
    );
    assert_eq!(got, Vec::<String>::new());
}

#[test]
fn test_callable_reference_repeated_twice_yields_one_edge() {
    let body = "array_map('cb_bare', $a); array_map(cb_bare(...), $a);";
    assert_eq!(references(body), one("cb_bare"));
}

#[test]
fn test_callable_reference_row_carries_function_reference_reason_and_confidence() {
    let (path, src) = holder("Cb $o, array $a", "array_map([Cb::class, 'cb_qual'], $a);");
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[CB, FUNCTIONS, (path.as_str(), src.as_str())],
    );
    let rows = refs(&g, "m", RelType::References, FUNCTION_REFERENCE);
    assert_eq!(rows.len(), 1);
    let (file, name, reason, confidence) = &rows[0];
    assert_eq!(
        (file.as_str(), name.as_str(), reason.as_str()),
        ("src/Cb.php", "cb_qual", FUNCTION_REFERENCE)
    );
    assert!(*confidence > 0.0 && *confidence <= 1.0);
}

#[test]
fn test_callable_reference_braced_namespace_function_references_function() {
    let src = "<?php namespace P { class H { public function m(array $a) { array_map('P\\cb_bare', $a); } } }";
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[FUNCTIONS, ("src/H.php", src)],
    );
    let got: Vec<_> = refs(&g, "m", RelType::References, FUNCTION_REFERENCE)
        .into_iter()
        .map(|r| r.1)
        .collect();
    assert_eq!(got, one("cb_bare"));
}
