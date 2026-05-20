use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

/// Helper: run the Rust parser and return the union of `calls` from every
/// node named `fn_name`. Multiple same-name nodes can exist (trait
/// declaration + impl definition); we want call edges from any of them.
fn calls_for(source: &str, fn_name: &str) -> Vec<String> {
    let provider = RustProvider::new().expect("RustProvider::new");
    let graph = provider
        .parse_file(Path::new("test.rs"), source.as_bytes())
        .expect("parse_file");
    graph
        .nodes
        .iter()
        .filter(|n| n.name == fn_name)
        .flat_map(|n| n.calls.iter().cloned())
        .collect()
}

/// `self.method()` inside an inherent impl resolves to `Type.method`.
#[test]
fn test_rust_self_call_in_impl() {
    let src = r#"
struct Dog;
impl Dog {
    fn bark(&self) {}
    fn fetch(&self) {
        self.bark();
    }
}
"#;
    let calls = calls_for(src, "fetch");
    assert!(
        calls.iter().any(|c| c == "Dog.bark"),
        "expected Dog.bark in calls, got: {calls:?}"
    );
}

/// `self.method()` inside a trait impl resolves to the implementing type.
#[test]
fn test_rust_self_call_in_trait_impl() {
    let src = r#"
trait Speak { fn speak(&self); }
struct Cat;
impl Speak for Cat {
    fn speak(&self) {
        self.purr();
    }
}
impl Cat {
    fn purr(&self) {}
}
"#;
    let calls = calls_for(src, "speak");
    assert!(
        calls.iter().any(|c| c == "Cat.purr"),
        "expected Cat.purr in calls (trait impl self-call), got: {calls:?}"
    );
}

/// Typed function parameter: `fn greet(d: &Dog) { d.bark(); }`.
#[test]
fn test_rust_typed_param_call() {
    let src = r#"
struct Dog;
impl Dog {
    fn bark(&self) {}
}
fn greet(d: &Dog) {
    d.bark();
}
"#;
    let calls = calls_for(src, "greet");
    assert!(
        calls.iter().any(|c| c == "Dog.bark"),
        "expected Dog.bark from typed param, got: {calls:?}"
    );
}

/// `let x: Dog = ...` — typed local binding.
#[test]
fn test_rust_let_typed_binding_call() {
    let src = r#"
struct Dog;
impl Dog {
    fn new() -> Dog { Dog }
    fn bark(&self) {}
}
fn process() {
    let d: Dog = Dog::new();
    d.bark();
}
"#;
    let calls = calls_for(src, "process");
    assert!(
        calls.iter().any(|c| c == "Dog.bark"),
        "expected Dog.bark from let-typed binding, got: {calls:?}"
    );
}

/// `let mut d: Dog = ...` — mut binding still resolves.
#[test]
fn test_rust_let_mut_typed_binding_call() {
    let src = r#"
struct Dog;
impl Dog {
    fn new() -> Dog { Dog }
    fn sit(&mut self) {}
}
fn train() {
    let mut d: Dog = Dog::new();
    d.sit();
}
"#;
    let calls = calls_for(src, "train");
    assert!(
        calls.iter().any(|c| c == "Dog.sit"),
        "expected Dog.sit from let-mut binding, got: {calls:?}"
    );
}

/// Unknown receiver falls back to bare method name.
#[test]
fn test_rust_unknown_receiver_fallback() {
    let src = r#"
fn process(a: &dyn std::fmt::Debug) {
    a.speak();
}
"#;
    let calls = calls_for(src, "process");
    // dyn Trait is opaque — bare method name should appear as fallback.
    assert!(
        calls.iter().any(|c| c == "speak"),
        "expected bare 'speak' fallback for dyn receiver, got: {calls:?}"
    );
}
