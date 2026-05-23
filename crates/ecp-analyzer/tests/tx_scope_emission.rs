//! Integration tests for `TransactionScope` node + `OpensTxScope` edge emission
//! across 5 languages / frameworks (T10 post-process pass).
//!
//! Each test covers two layers:
//!   Layer 1 — parser: `LocalGraph.tx_scopes` populated correctly.
//!   Layer 2 — post-process: `emit_edges` produces the right node + edge counts.
//!
//! Languages / frameworks covered in this file:
//!   - Python (Django `@transaction.atomic`) — regression guard
//!   - Java (Spring `@Transactional`)
//!   - Kotlin (Spring `@Transactional`)
//!   - C# (.NET `[Transactional]`)
//!   - PHP (Symfony `#[Transactional]`)

use ecp_analyzer::post_process::tx_scope_edges;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawTxScope};
use ecp_core::graph::{NodeKind, RelType};
use ecp_core::pool::StringPool;

// ── helpers ────────────────────────────────────────────────────────────────

fn scopes(g: &LocalGraph) -> &[RawTxScope] {
    g.tx_scopes.as_deref().unwrap_or(&[])
}

fn fn_name_of_scope<'g>(g: &'g LocalGraph, s: &RawTxScope) -> &'g str {
    g.nodes[s.node_idx() as usize].name.as_str()
}

/// Build the post-process output for a single LocalGraph, returning
/// `(TransactionScope_node_count, OpensTxScope_edge_count)`.
fn run_tx_scope_emit(local_graph: LocalGraph) -> (usize, usize) {
    let lgs = vec![local_graph];
    let mut sp = StringPool::new();
    let mut nodes: Vec<ecp_core::graph::Node> = Vec::new();
    let mut edges: Vec<ecp_core::graph::Edge> = Vec::new();
    tx_scope_edges::emit_edges(&lgs, &mut sp, &mut nodes, &mut edges);
    let scope_nodes = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::TransactionScope)
        .count();
    let scope_edges = edges
        .iter()
        .filter(|e| e.rel_type == RelType::OpensTxScope)
        .count();
    (scope_nodes, scope_edges)
}

// ── Layer 1: parser unit tests ─────────────────────────────────────────────

// ── Python (regression) ─────────────────────────────────────────────────

#[test]
fn python_transaction_atomic_emits_django_atomic_scope() {
    use ecp_analyzer::python::parser::PythonProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = PythonProvider::new().expect("provider");
    let src = r#"
from django.db import transaction

@transaction.atomic
def place_order():
    pass

def list_orders():
    pass
"#;
    let g = p
        .parse_file(Path::new("orders.py"), src.as_bytes())
        .expect("parse");
    assert_eq!(scopes(&g).len(), 1, "one tx_scope expected");
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "place_order");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::DjangoAtomic);
}

// ── Java (Spring @Transactional) ────────────────────────────────────────

#[test]
fn java_transactional_annotation_emits_spring_scope() {
    use ecp_analyzer::java::parser::JavaProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = JavaProvider::new().expect("provider");
    let src = r#"
import org.springframework.transaction.annotation.Transactional;

public class OrderService {
    @Transactional
    public void placeOrder() {}

    public void listOrders() {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.java"), src.as_bytes())
        .expect("parse");
    assert_eq!(scopes(&g).len(), 1, "one tx_scope expected");
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "placeOrder");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::SpringTransactional);
}

#[test]
fn java_no_transactional_produces_no_scope() {
    use ecp_analyzer::java::parser::JavaProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = JavaProvider::new().expect("provider");
    let src = r#"
public class UserService {
    public void getUser() {}
}
"#;
    let g = p
        .parse_file(Path::new("UserService.java"), src.as_bytes())
        .expect("parse");
    assert!(scopes(&g).is_empty(), "no tx_scope expected");
    assert!(g.tx_scopes.is_none());
}

// ── Kotlin (Spring @Transactional) ──────────────────────────────────────

#[test]
fn kotlin_transactional_annotation_emits_spring_scope() {
    use ecp_analyzer::kotlin::parser::KotlinProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = KotlinProvider::new().expect("provider");
    let src = r#"
import org.springframework.transaction.annotation.Transactional

class OrderService {
    @Transactional
    fun placeOrder() {}

    fun listOrders() {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.kt"), src.as_bytes())
        .expect("parse");
    assert_eq!(scopes(&g).len(), 1, "one tx_scope expected");
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "placeOrder");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::SpringTransactional);
}

// ── C# (.NET [Transactional]) ───────────────────────────────────────────

#[test]
fn csharp_transactional_attribute_emits_dotnet_scope() {
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = CSharpProvider::new().expect("provider");
    let src = r#"
public class OrderService {
    [Transactional]
    public void PlaceOrder() {}

    public void ListOrders() {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.cs"), src.as_bytes())
        .expect("parse");
    assert_eq!(
        scopes(&g).len(),
        1,
        "one tx_scope expected; got: {:?}",
        scopes(&g)
            .iter()
            .map(|s| fn_name_of_scope(&g, s))
            .collect::<Vec<_>>()
    );
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "PlaceOrder");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::DotNetTransactional);
}

#[test]
fn csharp_transaction_attribute_parameterized_emits_scope() {
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = CSharpProvider::new().expect("provider");
    let src = r#"
public class PaymentService {
    [Transactional(IsolationLevel.Serializable)]
    public void ProcessPayment() {}
}
"#;
    let g = p
        .parse_file(Path::new("PaymentService.cs"), src.as_bytes())
        .expect("parse");
    assert_eq!(
        scopes(&g).len(),
        1,
        "parameterized [Transactional(...)] should emit"
    );
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "ProcessPayment");
}

#[test]
fn csharp_no_transactional_produces_no_scope() {
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = CSharpProvider::new().expect("provider");
    let src = r#"
public class UserService {
    [Authorize]
    public void GetUser() {}
}
"#;
    let g = p
        .parse_file(Path::new("UserService.cs"), src.as_bytes())
        .expect("parse");
    assert!(
        scopes(&g).is_empty(),
        "non-tx attribute must not produce tx_scope"
    );
    assert!(g.tx_scopes.is_none());
}

#[test]
fn csharp_multiple_transactional_methods_emit_multiple_scopes() {
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = CSharpProvider::new().expect("provider");
    let src = r#"
public class AccountService {
    [Transactional]
    public void Deposit() {}

    [Transactional]
    public void Withdraw() {}

    public void ReadBalance() {}
}
"#;
    let g = p
        .parse_file(Path::new("AccountService.cs"), src.as_bytes())
        .expect("parse");
    assert_eq!(scopes(&g).len(), 2, "two tx_scopes expected");
    let names: Vec<&str> = scopes(&g).iter().map(|s| fn_name_of_scope(&g, s)).collect();
    assert!(names.contains(&"Deposit"), "Deposit missing: {:?}", names);
    assert!(names.contains(&"Withdraw"), "Withdraw missing: {:?}", names);
}

// ── PHP (Symfony #[Transactional]) ──────────────────────────────────────

#[test]
fn php_transactional_attribute_emits_symfony_scope() {
    use ecp_analyzer::php::parser::PhpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = PhpProvider::new().expect("provider");
    let src = r#"<?php
class OrderService {
    #[Transactional]
    public function placeOrder(): void {}

    public function listOrders(): void {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.php"), src.as_bytes())
        .expect("parse");
    assert_eq!(
        scopes(&g).len(),
        1,
        "one tx_scope expected; got: {:?}",
        scopes(&g)
            .iter()
            .map(|s| fn_name_of_scope(&g, s))
            .collect::<Vec<_>>()
    );
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "placeOrder");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::SymfonyTransactional);
}

#[test]
fn php_no_transactional_produces_no_scope() {
    use ecp_analyzer::php::parser::PhpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = PhpProvider::new().expect("provider");
    let src = r#"<?php
class UserService {
    #[Route('/users')]
    public function getUsers(): void {}
}
"#;
    let g = p
        .parse_file(Path::new("UserService.php"), src.as_bytes())
        .expect("parse");
    assert!(
        scopes(&g).is_empty(),
        "non-tx attribute must not produce tx_scope"
    );
    assert!(g.tx_scopes.is_none());
}

#[test]
fn php_multiple_transactional_methods_emit_multiple_scopes() {
    use ecp_analyzer::php::parser::PhpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = PhpProvider::new().expect("provider");
    let src = r#"<?php
class AccountService {
    #[Transactional]
    public function deposit(): void {}

    #[Transactional]
    public function withdraw(): void {}

    public function readBalance(): float { return 0.0; }
}
"#;
    let g = p
        .parse_file(Path::new("AccountService.php"), src.as_bytes())
        .expect("parse");
    assert_eq!(scopes(&g).len(), 2, "two tx_scopes expected");
    let names: Vec<&str> = scopes(&g).iter().map(|s| fn_name_of_scope(&g, s)).collect();
    assert!(names.contains(&"deposit"), "deposit missing: {:?}", names);
    assert!(names.contains(&"withdraw"), "withdraw missing: {:?}", names);
}

// ── Rust (#[transaction] proc-macro) ────────────────────────────────────────

#[test]
fn rust_transaction_attr_on_free_function_emits_scope() {
    use ecp_analyzer::rust::parser::RustProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = RustProvider::new().expect("provider");
    let src = r#"
#[transaction]
pub async fn create_user(pool: &PgPool, data: UserDto) -> Result<User, Error> {
    // ...
}

pub fn list_users() {}
"#;
    let g = p
        .parse_file(Path::new("users.rs"), src.as_bytes())
        .expect("parse");
    assert_eq!(scopes(&g).len(), 1, "one tx_scope expected");
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "create_user");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::RustTransaction);
}

#[test]
fn rust_transaction_attr_on_impl_method_emits_scope() {
    use ecp_analyzer::rust::parser::RustProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = RustProvider::new().expect("provider");
    let src = r#"
struct UserService;

impl UserService {
    #[transaction]
    pub fn create_user(&self) {}

    pub fn list_users(&self) {}
}
"#;
    let g = p
        .parse_file(Path::new("service.rs"), src.as_bytes())
        .expect("parse");
    assert_eq!(scopes(&g).len(), 1, "one tx_scope expected");
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "create_user");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::RustTransaction);
}

#[test]
fn rust_transaction_attr_with_args_emits_scope() {
    use ecp_analyzer::rust::parser::RustProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = RustProvider::new().expect("provider");
    let src = r#"
#[transaction(rollback)]
pub fn transfer_funds() {}
"#;
    let g = p
        .parse_file(Path::new("payment.rs"), src.as_bytes())
        .expect("parse");
    assert_eq!(
        scopes(&g).len(),
        1,
        "arg-bearing #[transaction(...)] must emit scope"
    );
    assert_eq!(fn_name_of_scope(&g, &scopes(&g)[0]), "transfer_funds");
    assert_eq!(scopes(&g)[0].framework(), FrameworkId::RustTransaction);
}

#[test]
fn rust_test_attr_does_not_emit_scope() {
    use ecp_analyzer::rust::parser::RustProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = RustProvider::new().expect("provider");
    let src = r#"
#[test]
fn test_something() {}

#[tokio::test]
async fn test_async() {}

#[derive(Transaction)]
struct Foo;

pub fn plain_fn() {}
"#;
    let g = p
        .parse_file(Path::new("lib.rs"), src.as_bytes())
        .expect("parse");
    assert!(
        scopes(&g).is_empty(),
        "#[test] / #[tokio::test] / #[derive(Transaction)] must not emit tx_scope; got: {:?}",
        scopes(&g)
            .iter()
            .map(|s| fn_name_of_scope(&g, s))
            .collect::<Vec<_>>()
    );
    assert!(g.tx_scopes.is_none());
}

// ── Layer 2: post-process integration tests ────────────────────────────────

#[test]
fn emit_produces_transaction_scope_node_and_opens_tx_scope_edge_python() {
    use ecp_analyzer::python::parser::PythonProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = PythonProvider::new().expect("provider");
    let src = r#"
from django.db import transaction

@transaction.atomic
def place_order():
    pass
"#;
    let g = p
        .parse_file(Path::new("orders.py"), src.as_bytes())
        .expect("parse");
    let (nodes, edges) = run_tx_scope_emit(g);
    assert_eq!(nodes, 1, "one TransactionScope node expected");
    assert_eq!(edges, 1, "one OpensTxScope edge expected");
}

#[test]
fn emit_produces_transaction_scope_node_and_opens_tx_scope_edge_java() {
    use ecp_analyzer::java::parser::JavaProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = JavaProvider::new().expect("provider");
    let src = r#"
import org.springframework.transaction.annotation.Transactional;

public class OrderService {
    @Transactional
    public void placeOrder() {}

    @Transactional
    public void processPayment() {}

    public void listOrders() {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.java"), src.as_bytes())
        .expect("parse");
    let (nodes, edges) = run_tx_scope_emit(g);
    assert_eq!(nodes, 2, "two TransactionScope nodes expected");
    assert_eq!(edges, 2, "two OpensTxScope edges expected");
}

#[test]
fn emit_produces_transaction_scope_node_and_opens_tx_scope_edge_csharp() {
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = CSharpProvider::new().expect("provider");
    let src = r#"
public class OrderService {
    [Transactional]
    public void PlaceOrder() {}

    public void ListOrders() {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.cs"), src.as_bytes())
        .expect("parse");
    let (nodes, edges) = run_tx_scope_emit(g);
    assert_eq!(nodes, 1, "one TransactionScope node expected");
    assert_eq!(edges, 1, "one OpensTxScope edge expected");
}

#[test]
fn emit_produces_transaction_scope_node_and_opens_tx_scope_edge_php() {
    use ecp_analyzer::php::parser::PhpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = PhpProvider::new().expect("provider");
    let src = r#"<?php
class OrderService {
    #[Transactional]
    public function placeOrder(): void {}

    public function listOrders(): void {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.php"), src.as_bytes())
        .expect("parse");
    let (nodes, edges) = run_tx_scope_emit(g);
    assert_eq!(nodes, 1, "one TransactionScope node expected");
    assert_eq!(edges, 1, "one OpensTxScope edge expected");
}

#[test]
fn emit_no_scope_when_no_tx_annotations() {
    use ecp_analyzer::java::parser::JavaProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = JavaProvider::new().expect("provider");
    let src = r#"
public class UserService {
    public void getUser() {}
}
"#;
    let g = p
        .parse_file(Path::new("UserService.java"), src.as_bytes())
        .expect("parse");
    let (nodes, edges) = run_tx_scope_emit(g);
    assert_eq!(nodes, 0, "no TransactionScope nodes expected");
    assert_eq!(edges, 0, "no OpensTxScope edges expected");
}

#[test]
fn emit_transaction_scope_node_name_contains_framework() {
    use ecp_analyzer::java::parser::JavaProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = JavaProvider::new().expect("provider");
    let src = r#"
import org.springframework.transaction.annotation.Transactional;

public class OrderService {
    @Transactional
    public void placeOrder() {}
}
"#;
    let g = p
        .parse_file(Path::new("OrderService.java"), src.as_bytes())
        .expect("parse");
    let lgs = vec![g];
    let mut sp = StringPool::new();
    let mut nodes: Vec<ecp_core::graph::Node> = Vec::new();
    let mut edges: Vec<ecp_core::graph::Edge> = Vec::new();
    tx_scope_edges::emit_edges(&lgs, &mut sp, &mut nodes, &mut edges);

    let scope_node = nodes
        .iter()
        .find(|n| n.kind == NodeKind::TransactionScope)
        .expect("scope node");
    let name = sp.resolve(&scope_node.name);
    assert!(
        name.contains("placeOrder"),
        "scope name must contain enclosing fn name: {:?}",
        name
    );
    assert!(
        name.contains("spring-transactional"),
        "scope name must contain framework label: {:?}",
        name
    );
}

// ── Go (db.Begin / db.BeginTx call-site) ────────────────────────────────

#[test]
fn go_db_begin_emits_gosqltx_scope() {
    use ecp_analyzer::go::parser::GoProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = GoProvider::new().expect("provider");
    let src = r#"package db

import "database/sql"

func createUser(db *sql.DB) error {
    tx, err := db.Begin()
    if err != nil {
        return err
    }
    defer tx.Rollback()
    return tx.Commit()
}

func listUsers(db *sql.DB) {}
"#;
    let g = p
        .parse_file(Path::new("users.go"), src.as_bytes())
        .expect("parse");
    let s = scopes(&g);
    assert_eq!(s.len(), 1, "one tx_scope expected; got: {:?}", s.len());
    assert_eq!(fn_name_of_scope(&g, &s[0]), "createUser");
    assert_eq!(s[0].framework(), FrameworkId::GoSqlTx);
}

#[test]
fn go_db_begin_tx_emits_gosqltx_scope() {
    use ecp_analyzer::go::parser::GoProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = GoProvider::new().expect("provider");
    let src = r#"package db

import (
    "context"
    "database/sql"
)

func transferFunds(ctx context.Context, db *sql.DB) error {
    tx, err := db.BeginTx(ctx, nil)
    if err != nil {
        return err
    }
    defer tx.Rollback()
    return tx.Commit()
}
"#;
    let g = p
        .parse_file(Path::new("transfer.go"), src.as_bytes())
        .expect("parse");
    let s = scopes(&g);
    assert_eq!(s.len(), 1, "one tx_scope expected for BeginTx");
    assert_eq!(fn_name_of_scope(&g, &s[0]), "transferFunds");
    assert_eq!(s[0].framework(), FrameworkId::GoSqlTx);
}

#[test]
fn go_multiple_begin_in_same_fn_deduplicates_to_one_scope() {
    use ecp_analyzer::go::parser::GoProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = GoProvider::new().expect("provider");
    // Two db.Begin() calls inside the same function — must emit exactly one scope.
    let src = r#"package db

import "database/sql"

func multiTx(db *sql.DB) error {
    tx1, _ := db.Begin()
    defer tx1.Rollback()
    tx2, _ := db.Begin()
    defer tx2.Rollback()
    return nil
}
"#;
    let g = p
        .parse_file(Path::new("multi.go"), src.as_bytes())
        .expect("parse");
    let s = scopes(&g);
    assert_eq!(
        s.len(),
        1,
        "multiple Begin() in same function must produce exactly one scope (got {})",
        s.len()
    );
    assert_eq!(fn_name_of_scope(&g, &s[0]), "multiTx");
}

#[test]
fn go_begin_outside_function_produces_no_scope() {
    use ecp_analyzer::go::parser::GoProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let p = GoProvider::new().expect("provider");
    // A file with no functions at all — Begin() would be top-level (impossible
    // in real Go, but the parser must not panic and must emit zero scopes).
    let src = r#"package db

import "database/sql"

var DB *sql.DB
"#;
    let g = p
        .parse_file(Path::new("nofunc.go"), src.as_bytes())
        .expect("parse");
    assert!(
        scopes(&g).is_empty(),
        "no functions → no tx_scopes expected"
    );
}
