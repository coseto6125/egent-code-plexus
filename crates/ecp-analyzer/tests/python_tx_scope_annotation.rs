use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawTxScope};
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PythonProvider::new().expect("PythonProvider::new");
    p.parse_file(Path::new("test.py"), src.as_bytes())
        .expect("parse_file")
}

fn scopes(graph: &LocalGraph) -> &[RawTxScope] {
    graph.tx_scopes.as_deref().unwrap_or(&[])
}

fn fn_name_of(graph: &LocalGraph, scope_idx: usize) -> &str {
    let s = &scopes(graph)[scope_idx];
    graph.nodes[s.node_idx() as usize].name.as_str()
}

fn fn_name_of_scope<'g>(graph: &'g LocalGraph, scope: &RawTxScope) -> &'g str {
    graph.nodes[scope.node_idx() as usize].name.as_str()
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
        scopes(&g).len(),
        1,
        "exactly one tx_scope expected; got: {:?}",
        scopes(&g)
            .iter()
            .map(|s| fn_name_of_scope(&g, s))
            .collect::<Vec<_>>()
    );
    assert_eq!(fn_name_of(&g, 0), "place_order");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::DjangoAtomic);
}

#[test]
fn transaction_atomic_call_form_emits_django_atomic_scope() {
    let src = r#"
from django.db import transaction

@transaction.atomic(using='primary')
def save_order():
    pass
"#;
    let g = parse(src);
    assert_eq!(
        scopes(&g).len(),
        1,
        "call-form @transaction.atomic(...) should emit"
    );
    assert_eq!(fn_name_of(&g, 0), "save_order");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::DjangoAtomic);
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
    assert_eq!(scopes(&g).len(), 1);
    assert_eq!(fn_name_of(&g, 0), "get_user");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::PonyDbSession);
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
    assert_eq!(scopes(&g).len(), 2);
    let by_framework: std::collections::HashMap<&'static str, &str> = scopes(&g)
        .iter()
        .map(|s| (s.framework().as_str(), fn_name_of_scope(&g, s)))
        .collect();
    assert_eq!(
        by_framework.get("django-atomic").copied(),
        Some("create_order"),
        "django-atomic scope missing or wrong fn"
    );
    assert_eq!(
        by_framework.get("pony-db-session").copied(),
        Some("fetch_user"),
        "pony-db-session scope missing or wrong fn"
    );
}

#[test]
fn non_tx_decorators_do_not_emit_tx_scope() {
    let src = r#"
from functools import cached_property

class Service:
    @cached_property
    def items(self):
        return []
"#;
    let g = parse(src);
    assert!(scopes(&g).is_empty());
    assert!(g.tx_scopes.is_none());
}

#[test]
fn test_resolve_tx_scopes_decorated_nested_function_binds_inner() {
    let src = r#"
from django.db import transaction

def outer():
    @transaction.atomic
    def inner():
        pass
    return inner
"#;
    let g = parse(src);
    assert_eq!(scopes(&g).len(), 1);
    assert_eq!(fn_name_of(&g, 0), "inner");
}
