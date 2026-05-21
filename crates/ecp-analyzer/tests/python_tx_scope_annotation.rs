use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let p = PythonProvider::new().expect("PythonProvider::new");
    p.parse_file(Path::new("views.py"), src.as_bytes())
        .expect("parse_file")
}

fn resolve_fn(graph: &ecp_core::analyzer::types::LocalGraph, scope_idx: usize) -> &str {
    graph.tx_scopes[scope_idx]
        .enclosing_fn
        .resolve(&graph.pool_bytes)
}

#[test]
fn transaction_atomic_decorator_emits_django_atomic_scope() {
    let src = r#"
from django.db import transaction

@transaction.atomic
def place_order():
    pass

def list_orders():
    pass
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
    assert_eq!(resolve_fn(&g, 0), "place_order");
    assert_eq!(g.tx_scopes[0].source_pattern, "django-atomic");
}

#[test]
fn db_session_decorator_emits_pony_db_session_scope() {
    let src = r#"
from pony.orm import db_session

@db_session
def get_user():
    pass

def delete_user():
    pass
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
    assert_eq!(resolve_fn(&g, 0), "get_user");
    assert_eq!(g.tx_scopes[0].source_pattern, "pony-db-session");
}

#[test]
fn both_patterns_in_same_file() {
    let src = r#"
from django.db import transaction
from pony.orm import db_session

@transaction.atomic
def create_order():
    pass

@db_session
def fetch_user():
    pass

def plain_func():
    pass
"#;
    let g = parse(src);
    assert_eq!(
        g.tx_scopes.len(),
        2,
        "two tx_scopes expected; got {}",
        g.tx_scopes.len()
    );
    let by_pattern: std::collections::HashMap<&str, &str> = g
        .tx_scopes
        .iter()
        .map(|s| {
            (
                s.source_pattern.as_str(),
                s.enclosing_fn.resolve(&g.pool_bytes),
            )
        })
        .collect();
    assert_eq!(
        by_pattern.get("django-atomic").copied(),
        Some("create_order"),
        "django-atomic scope missing or wrong fn"
    );
    assert_eq!(
        by_pattern.get("pony-db-session").copied(),
        Some("fetch_user"),
        "pony-db-session scope missing or wrong fn"
    );
}

#[test]
fn non_tx_decorators_do_not_emit_tx_scope() {
    let src = r#"
from functools import cached_property

class MyModel:
    @cached_property
    def expensive_attr(self):
        return 42

@staticmethod
def helper():
    pass
"#;
    let g = parse(src);
    assert!(
        g.tx_scopes.is_empty(),
        "no tx_scopes expected for @cached_property / @staticmethod; got: {:?}",
        g.tx_scopes
    );
}

#[test]
fn plain_function_without_decorator_produces_no_tx_scope() {
    let src = r#"
def compute():
    return 1 + 1
"#;
    let g = parse(src);
    assert!(
        g.tx_scopes.is_empty(),
        "no tx_scopes expected; got: {:?}",
        g.tx_scopes
    );
}
