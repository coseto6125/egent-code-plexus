use cgn_analyzer::go::parser::GoProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use std::path::Path;

/// Helper: run the Go parser and return the `calls` vec of the named node.
fn calls_for(source: &str, fn_name: &str) -> Vec<String> {
    let provider = GoProvider::new().expect("GoProvider::new");
    let graph = provider
        .parse_file(Path::new("test.go"), source.as_bytes())
        .expect("parse_file");
    graph
        .nodes
        .iter()
        .find(|n| n.name == fn_name)
        .map(|n| n.calls.clone())
        .unwrap_or_default()
}

/// Pointer-receiver method calling a sibling method on the same type.
/// `func (d *Dog) Fetch() { d.Bark() }` → calls includes "Dog.Bark".
#[test]
fn test_go_pointer_receiver_self_call() {
    let src = r#"package main

type Dog struct { Name string }

func (d *Dog) Bark() {}

func (d *Dog) Fetch() {
    d.Bark()
}
"#;
    let calls = calls_for(src, "Fetch");
    assert!(
        calls.iter().any(|c| c == "Dog.Bark"),
        "expected Dog.Bark in calls, got: {calls:?}"
    );
}

/// Value-receiver method: `func (c Cat) Purr() { c.Sleep() }`.
#[test]
fn test_go_value_receiver_self_call() {
    let src = r#"package main

type Cat struct{}

func (c Cat) Sleep() {}

func (c Cat) Purr() {
    c.Sleep()
}
"#;
    let calls = calls_for(src, "Purr");
    assert!(
        calls.iter().any(|c| c == "Cat.Sleep"),
        "expected Cat.Sleep in calls, got: {calls:?}"
    );
}

/// Typed function parameter: `func greet(d *Dog) { d.Bark() }`.
#[test]
fn test_go_typed_param_call() {
    let src = r#"package main

type Dog struct{}

func (d *Dog) Bark() {}

func greet(d *Dog) {
    d.Bark()
}
"#;
    let calls = calls_for(src, "greet");
    assert!(
        calls.iter().any(|c| c == "Dog.Bark"),
        "expected Dog.Bark in calls from typed param, got: {calls:?}"
    );
}

/// `var` declaration: `var dog Dog; dog.Bark()`.
#[test]
fn test_go_var_declaration_call() {
    let src = r#"package main

type Dog struct{}

func (d *Dog) Bark() {}

func process() {
    var dog Dog
    dog.Bark()
}
"#;
    let calls = calls_for(src, "process");
    assert!(
        calls.iter().any(|c| c == "Dog.Bark"),
        "expected Dog.Bark from var decl, got: {calls:?}"
    );
}

/// Short-var with composite literal: `d := Dog{}; d.Bark()`.
#[test]
fn test_go_short_var_composite_literal_call() {
    let src = r#"package main

type Dog struct{}

func (d *Dog) Bark() {}

func create() {
    d := Dog{}
    d.Bark()
}
"#;
    let calls = calls_for(src, "create");
    assert!(
        calls.iter().any(|c| c == "Dog.Bark"),
        "expected Dog.Bark from composite literal short-var, got: {calls:?}"
    );
}

/// Unknown receiver: call on an untyped variable falls back to bare method name.
#[test]
fn test_go_unknown_receiver_fallback() {
    let src = r#"package main

func process(a interface{}) {
    a.Speak()
}
"#;
    let calls = calls_for(src, "process");
    // Should contain the bare method name "Speak" (fallback), not a qualified form.
    assert!(
        calls.iter().any(|c| c == "Speak"),
        "expected bare 'Speak' fallback, got: {calls:?}"
    );
}
