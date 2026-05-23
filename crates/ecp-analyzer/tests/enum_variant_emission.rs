//! Cross-language fixture: each language's `enum X { A, B, C }` syntax must
//! emit one `NodeKind::Enum` node and N `NodeKind::EnumVariant` children, with
//! each variant's `owner_class` set to the enum's name. The downstream
//! `post_process/enum_variant_defines` pass converts `owner_class` linkage
//! into `(Enum)-[:Defines]->(EnumVariant)` edges.
//!
//! Coverage scope (8 languages with first-class enum syntax). Languages
//! deferred to FOLLOWUPS:
//! - Go (`const ( ... iota )` — not modeled as enum)
//! - PHP (8.1+ syntax — fixture cost)
//! - Python (`class Foo(Enum):` — requires base-class detection)
//! - Ruby, JS (no first-class enum)
//! - C (currently shares cpp tooling; cpp covers `enum class`)

use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn variant_names<'a>(g: &'a LocalGraph, enum_name: &str) -> Vec<&'a str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant && n.owner_class.as_deref() == Some(enum_name))
        .map(|n| n.name.as_str())
        .collect()
}

fn assert_variants(g: &LocalGraph, enum_name: &str, expected: &[&str]) {
    let enum_node = g
        .nodes
        .iter()
        .find(|n| n.name == enum_name && n.kind == NodeKind::Enum);
    assert!(
        enum_node.is_some(),
        "Enum node {enum_name:?} missing; nodes: {:?}",
        g.nodes
            .iter()
            .map(|n| (n.name.as_str(), n.kind))
            .collect::<Vec<_>>()
    );

    let mut variants = variant_names(g, enum_name);
    variants.sort();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort();
    assert_eq!(
        variants, want,
        "EnumVariant set for {enum_name:?} mismatch — variants={variants:?} expected={want:?}"
    );
}

// ── Rust ────────────────────────────────────────────────────────────────────

#[test]
fn rust_enum_emits_three_variants() {
    let p = ecp_analyzer::rust::parser::RustProvider::new().expect("provider");
    let src = "pub enum Color { Red, Green, Blue }\n";
    let g = p
        .parse_file(Path::new("color.rs"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "Color", &["Red", "Green", "Blue"]);
}

#[test]
fn rust_enum_with_payload_variants_still_emits_names() {
    let p = ecp_analyzer::rust::parser::RustProvider::new().expect("provider");
    let src = "pub enum Msg { Quit, Move { x: i32, y: i32 }, Write(String) }\n";
    let g = p
        .parse_file(Path::new("msg.rs"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "Msg", &["Quit", "Move", "Write"]);
}

// ── TypeScript ──────────────────────────────────────────────────────────────

#[test]
fn typescript_enum_emits_three_variants() {
    let p = ecp_analyzer::typescript::parser::TypeScriptProvider::new().expect("provider");
    let src = "export enum Status { Active = 'a', Idle = 'i', Done = 'd' }\n";
    let g = p
        .parse_file(Path::new("status.ts"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "Status", &["Active", "Idle", "Done"]);
}

// ── Java ────────────────────────────────────────────────────────────────────

#[test]
fn java_enum_emits_three_variants() {
    let p = ecp_analyzer::java::parser::JavaProvider::new().expect("provider");
    let src = "public enum Day { MONDAY, TUESDAY, WEDNESDAY }\n";
    let g = p
        .parse_file(Path::new("Day.java"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "Day", &["MONDAY", "TUESDAY", "WEDNESDAY"]);
}

// ── C# ──────────────────────────────────────────────────────────────────────

#[test]
fn csharp_enum_emits_three_variants() {
    let p = ecp_analyzer::c_sharp::parser::CSharpProvider::new().expect("provider");
    let src = "public enum Level { Info, Warn = 5, Err }\n";
    let g = p
        .parse_file(Path::new("Level.cs"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "Level", &["Info", "Warn", "Err"]);
}

// ── Swift ───────────────────────────────────────────────────────────────────

#[test]
fn swift_enum_emits_variants_for_all_cases() {
    let p = ecp_analyzer::swift::parser::SwiftProvider::new().expect("provider");
    // `case clubs, diamonds` declares two variants in one case statement.
    let src = "enum Suit { case clubs, diamonds\n  case hearts\n  case spades }\n";
    let g = p
        .parse_file(Path::new("suit.swift"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "Suit", &["clubs", "diamonds", "hearts", "spades"]);
}

// ── Dart ────────────────────────────────────────────────────────────────────

#[test]
fn dart_enum_emits_three_variants() {
    let p = ecp_analyzer::dart::parser::DartProvider::new().expect("provider");
    let src = "enum Phase { init, run, done }\n";
    let g = p
        .parse_file(Path::new("phase.dart"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "Phase", &["init", "run", "done"]);
}

// ── C++ ─────────────────────────────────────────────────────────────────────

#[test]
fn cpp_enum_class_emits_two_variants() {
    let p = ecp_analyzer::cpp::parser::CppProvider::new().expect("provider");
    let src = "enum class State { Active, Idle };\n";
    let g = p
        .parse_file(Path::new("state.cpp"), src.as_bytes())
        .expect("parse");
    assert_variants(&g, "State", &["Active", "Idle"]);
}
