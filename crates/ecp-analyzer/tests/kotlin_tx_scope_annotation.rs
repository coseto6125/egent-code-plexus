use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let p = KotlinProvider::new().expect("KotlinProvider::new");
    p.parse_file(Path::new("Test.kt"), src.as_bytes())
        .expect("parse_file")
}

fn resolve_fn(graph: &ecp_core::analyzer::types::LocalGraph, scope_idx: usize) -> &str {
    graph.tx_scopes[scope_idx]
        .enclosing_fn
        .resolve(&graph.pool_bytes)
}

#[test]
fn annotated_function_emits_tx_scope() {
    let src = r#"
import org.springframework.transaction.annotation.Transactional

class OrderService {
    @Transactional
    fun placeOrder() {}

    fun listOrders() {}
}
"#;
    let g = parse(src);
    assert_eq!(
        g.tx_scopes.len(),
        1,
        "exactly one tx_scope expected; got: {:?}",
        g.tx_scopes
            .iter()
            .map(|s| s.enclosing_fn.resolve(&g.pool_bytes))
            .collect::<Vec<_>>()
    );
    assert_eq!(resolve_fn(&g, 0), "placeOrder");
    assert_eq!(g.tx_scopes[0].source_pattern, "java-transactional");
}

#[test]
fn non_annotated_function_produces_no_tx_scope() {
    let src = r#"
class UserService {
    fun getUser() {}
    fun deleteUser() {}
}
"#;
    let g = parse(src);
    assert!(
        g.tx_scopes.is_empty(),
        "no tx_scopes expected for plain functions; got: {:?}",
        g.tx_scopes
    );
}

#[test]
fn transactional_with_args_emits_tx_scope() {
    let src = r#"
import org.springframework.transaction.annotation.Transactional

class PaymentService {
    @Transactional(rollbackFor = [Exception::class])
    fun processPayment() {}
}
"#;
    let g = parse(src);
    assert_eq!(
        g.tx_scopes.len(),
        1,
        "tx_scope expected for @Transactional with args"
    );
    assert_eq!(resolve_fn(&g, 0), "processPayment");
    assert_eq!(g.tx_scopes[0].source_pattern, "java-transactional");
}

#[test]
fn multiple_annotated_functions_each_emit_tx_scope() {
    let src = r#"
import org.springframework.transaction.annotation.Transactional

class AccountService {
    @Transactional
    fun deposit() {}

    @Transactional
    fun withdraw() {}

    fun balance() {}
}
"#;
    let g = parse(src);
    assert_eq!(
        g.tx_scopes.len(),
        2,
        "two tx_scopes expected; got {}",
        g.tx_scopes.len()
    );
    let names: Vec<&str> = g
        .tx_scopes
        .iter()
        .map(|s| s.enclosing_fn.resolve(&g.pool_bytes))
        .collect();
    assert!(names.contains(&"deposit"), "deposit missing: {:?}", names);
    assert!(names.contains(&"withdraw"), "withdraw missing: {:?}", names);
}
