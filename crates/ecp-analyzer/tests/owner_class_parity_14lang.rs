//! 14-language parity test for `RawNode.owner_class`.
//!
//! For each language: builds a minimal fixture with one class+method pair.
//! Asserts owner_class is populated on every method node inside the class,
//! and None on every module-level function.

use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn method_owners(g: &LocalGraph) -> Vec<(String, Option<String>)> {
    g.nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Method | NodeKind::Function | NodeKind::Constructor
            )
        })
        .map(|n| (n.name.clone(), n.owner_class.clone()))
        .collect()
}

// ─── Rust ────────────────────────────────────────────────────────────────────
#[test]
fn rust_owner_class_parity() {
    use ecp_analyzer::rust::parser::RustProvider;
    let src = "struct Dog;\nimpl Dog {\n    fn bark(&self) {}\n}\nfn free_fn() {}\n";
    let g = RustProvider::new()
        .unwrap()
        .parse_file(Path::new("t.rs"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert_eq!(bark.1.as_deref(), Some("Dog"), "bark must own Dog");
    let free = pairs.iter().find(|(n, _)| n == "free_fn").expect("free_fn");
    assert!(
        free.1.is_none(),
        "free_fn must have no owner; got {:?}",
        free.1
    );
}

// ─── Python ──────────────────────────────────────────────────────────────────
#[test]
fn python_owner_class_parity() {
    use ecp_analyzer::python::parser::PythonProvider;
    let src = "class Dog:\n    def bark(self): pass\ndef free_fn(): pass\n";
    let g = PythonProvider::new()
        .unwrap()
        .parse_file(Path::new("t.py"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(
        bark.1.is_some(),
        "Python bark must own Dog; got {:?}",
        bark.1
    );
    let free = pairs.iter().find(|(n, _)| n == "free_fn").expect("free_fn");
    assert!(
        free.1.is_none(),
        "free_fn must have no owner; got {:?}",
        free.1
    );
}

// ─── TypeScript ───────────────────────────────────────────────────────────────
#[test]
fn typescript_owner_class_parity() {
    use ecp_analyzer::typescript::parser::TypeScriptProvider;
    let src = "class Dog { bark() {} }\nfunction freeFn() {}\n";
    let g = TypeScriptProvider::new()
        .unwrap()
        .parse_file(Path::new("t.ts"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(bark.1.is_some(), "TS bark must own Dog; got {:?}", bark.1);
    let free = pairs.iter().find(|(n, _)| n == "freeFn").expect("freeFn");
    assert!(
        free.1.is_none(),
        "freeFn must have no owner; got {:?}",
        free.1
    );
}

// ─── JavaScript ───────────────────────────────────────────────────────────────
#[test]
fn javascript_owner_class_parity() {
    use ecp_analyzer::javascript::parser::JavaScriptProvider;
    let src = "class Dog { bark() {} }\nfunction freeFn() {}\n";
    let g = JavaScriptProvider::new()
        .unwrap()
        .parse_file(Path::new("t.js"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(bark.1.is_some(), "JS bark must own Dog; got {:?}", bark.1);
    let free = pairs.iter().find(|(n, _)| n == "freeFn").expect("freeFn");
    assert!(
        free.1.is_none(),
        "freeFn must have no owner; got {:?}",
        free.1
    );
}

// ─── Java ─────────────────────────────────────────────────────────────────────
#[test]
fn java_owner_class_parity() {
    use ecp_analyzer::java::parser::JavaProvider;
    let src = "class Dog { void bark() {} }\n";
    let g = JavaProvider::new()
        .unwrap()
        .parse_file(Path::new("Dog.java"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(bark.1.is_some(), "Java bark must own Dog; got {:?}", bark.1);
}

// ─── Kotlin ───────────────────────────────────────────────────────────────────
#[test]
fn kotlin_owner_class_parity() {
    use ecp_analyzer::kotlin::parser::KotlinProvider;
    let src = "class Dog { fun bark() {} }\nfun freeFn() {}\n";
    let g = KotlinProvider::new()
        .unwrap()
        .parse_file(Path::new("t.kt"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(
        bark.1.is_some(),
        "Kotlin bark must own Dog; got {:?}",
        bark.1
    );
    let free = pairs.iter().find(|(n, _)| n == "freeFn").expect("freeFn");
    assert!(
        free.1.is_none(),
        "freeFn must have no owner; got {:?}",
        free.1
    );
}

// ─── C# ───────────────────────────────────────────────────────────────────────
#[test]
fn csharp_owner_class_parity() {
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    let src = "class Dog { void Bark() {} }\n";
    let g = CSharpProvider::new()
        .unwrap()
        .parse_file(Path::new("Dog.cs"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "Bark").expect("Bark");
    assert!(bark.1.is_some(), "C# Bark must own Dog; got {:?}", bark.1);
}

// ─── Go ───────────────────────────────────────────────────────────────────────
#[test]
fn go_owner_class_parity() {
    use ecp_analyzer::go::parser::GoProvider;
    let src = "package main\ntype Dog struct{}\nfunc (d *Dog) Bark() {}\nfunc FreeFunc() {}\n";
    let g = GoProvider::new()
        .unwrap()
        .parse_file(Path::new("t.go"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "Bark").expect("Bark");
    assert!(bark.1.is_some(), "Go Bark must own Dog; got {:?}", bark.1);
    let free = pairs
        .iter()
        .find(|(n, _)| n == "FreeFunc")
        .expect("FreeFunc");
    assert!(
        free.1.is_none(),
        "FreeFunc must have no owner; got {:?}",
        free.1
    );
}

// ─── PHP ──────────────────────────────────────────────────────────────────────
#[test]
fn php_owner_class_parity() {
    use ecp_analyzer::php::parser::PhpProvider;
    let src = "<?php\nclass Dog { function bark() {} }\nfunction freeFn() {}\n";
    let g = PhpProvider::new()
        .unwrap()
        .parse_file(Path::new("t.php"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(bark.1.is_some(), "PHP bark must own Dog; got {:?}", bark.1);
    let free = pairs.iter().find(|(n, _)| n == "freeFn").expect("freeFn");
    assert!(
        free.1.is_none(),
        "freeFn must have no owner; got {:?}",
        free.1
    );
}

// ─── Ruby ─────────────────────────────────────────────────────────────────────
#[test]
fn ruby_owner_class_parity() {
    use ecp_analyzer::ruby::parser::RubyProvider;
    let src = "class Dog\n  def bark; end\nend\ndef free_fn; end\n";
    let g = RubyProvider::new()
        .unwrap()
        .parse_file(Path::new("t.rb"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(bark.1.is_some(), "Ruby bark must own Dog; got {:?}", bark.1);
    let free = pairs.iter().find(|(n, _)| n == "free_fn").expect("free_fn");
    assert!(
        free.1.is_none(),
        "free_fn must have no owner; got {:?}",
        free.1
    );
}

// ─── Swift ────────────────────────────────────────────────────────────────────
#[test]
fn swift_owner_class_parity() {
    use ecp_analyzer::swift::parser::SwiftProvider;
    let src = "class Dog { func bark() {} }\nfunc freeFn() {}\n";
    let g = SwiftProvider::new()
        .unwrap()
        .parse_file(Path::new("t.swift"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(
        bark.1.is_some(),
        "Swift bark must own Dog; got {:?}",
        bark.1
    );
    let free = pairs.iter().find(|(n, _)| n == "freeFn").expect("freeFn");
    assert!(
        free.1.is_none(),
        "freeFn must have no owner; got {:?}",
        free.1
    );
}

// ─── C ────────────────────────────────────────────────────────────────────────
#[test]
fn c_owner_class_parity() {
    use ecp_analyzer::c::parser::CProvider;
    // Receiver-convention: self/this first-param identifies struct ownership.
    let src =
        "struct Dog { int val; };\nvoid dog_bark(struct Dog *self) {}\nvoid free_fn(int x) {}\n";
    let g = CProvider::new()
        .unwrap()
        .parse_file(Path::new("t.c"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs
        .iter()
        .find(|(n, _)| n == "dog_bark")
        .expect("dog_bark");
    assert!(
        bark.1.is_some(),
        "C dog_bark must own Dog; got {:?}",
        bark.1
    );
    let free = pairs.iter().find(|(n, _)| n == "free_fn").expect("free_fn");
    assert!(
        free.1.is_none(),
        "free_fn must have no owner; got {:?}",
        free.1
    );
}

// ─── C++ ──────────────────────────────────────────────────────────────────────
#[test]
fn cpp_owner_class_parity() {
    use ecp_analyzer::cpp::parser::CppProvider;
    let src = "class Dog { public: void bark() {} };\nvoid freeFn() {}\n";
    let g = CppProvider::new()
        .unwrap()
        .parse_file(Path::new("t.cpp"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(bark.1.is_some(), "C++ bark must own Dog; got {:?}", bark.1);
    let free = pairs.iter().find(|(n, _)| n == "freeFn").expect("freeFn");
    assert!(
        free.1.is_none(),
        "freeFn must have no owner; got {:?}",
        free.1
    );
}

// ─── Dart ─────────────────────────────────────────────────────────────────────
#[test]
fn dart_owner_class_parity() {
    use ecp_analyzer::dart::parser::DartProvider;
    let src = "class Dog { void bark() {} }\nvoid freeFn() {}\n";
    let g = DartProvider::new()
        .unwrap()
        .parse_file(Path::new("t.dart"), src.as_bytes())
        .unwrap();
    let pairs = method_owners(&g);
    let bark = pairs.iter().find(|(n, _)| n == "bark").expect("bark");
    assert!(bark.1.is_some(), "Dart bark must own Dog; got {:?}", bark.1);
    let free = pairs.iter().find(|(n, _)| n == "freeFn").expect("freeFn");
    assert!(
        free.1.is_none(),
        "freeFn must have no owner; got {:?}",
        free.1
    );
}
