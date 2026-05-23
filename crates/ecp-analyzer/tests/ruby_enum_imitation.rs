use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_rb(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = RubyProvider::new().expect("RubyProvider::new");
    provider
        .parse_file(Path::new("test.rb"), src.as_bytes())
        .expect("parse_file")
}

fn enum_imitation_count(g: &ecp_core::analyzer::types::LocalGraph) -> usize {
    g.blind_spots
        .iter()
        .filter(|b| b.kind == "ruby-module-as-enum")
        .count()
}

// ── Positive cases ──

#[test]
fn test_module_int_constants_emits_blind_spot() {
    // Canonical Ruby enum imitation: integer values, no methods, no nested class.
    let src = "module Status\n  ACTIVE = 1\n  INACTIVE = 2\nend";
    let g = parse_rb(src);
    assert_eq!(
        enum_imitation_count(&g),
        1,
        "expected 1 ruby-module-as-enum blind spot; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn test_module_symbol_constants_emits_blind_spot() {
    // Three symbol constants — still matches (≥2).
    let src = "module Color\n  RED = :red\n  BLUE = :blue\n  GREEN = :green\nend";
    let g = parse_rb(src);
    assert_eq!(
        enum_imitation_count(&g),
        1,
        "expected 1 ruby-module-as-enum blind spot; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn test_module_float_and_string_constants_emits_blind_spot() {
    // Mix of float and string scalar literals.
    let src = "module Thresholds\n  LOW = 0.1\n  HIGH = \"high\"\nend";
    let g = parse_rb(src);
    assert_eq!(
        enum_imitation_count(&g),
        1,
        "expected 1 ruby-module-as-enum blind spot; got: {:?}",
        g.blind_spots
    );
}

// ── Known false-negative (documented conservative choice) ──

#[test]
fn test_module_freeze_string_not_emitted() {
    // `X = "active".freeze` — RHS is a `call` node (method call on string),
    // not a pure scalar literal. Conservative heuristic skips it.
    // This is a known false-negative: frozen-string idiom is common but
    // the RHS structure isn't a scalar, so 0 constants qualify and no
    // BlindSpot fires.
    let src = "module Status\n  ACTIVE = \"active\".freeze\n  INACTIVE = \"inactive\".freeze\nend";
    let g = parse_rb(src);
    assert_eq!(
        enum_imitation_count(&g),
        0,
        "frozen-string idiom is a known false-negative (RHS is call, not scalar); got: {:?}",
        g.blind_spots
    );
}

// ── False-positive guards ──

#[test]
fn test_module_with_method_not_emitted() {
    // Module has ≥2 constants but also defines a method — not an enum imitation.
    let src = "module Helper\n  THRESHOLD = 10\n  LIMIT = 100\n  def self.check(x)\n    x > THRESHOLD\n  end\nend";
    let g = parse_rb(src);
    assert_eq!(
        enum_imitation_count(&g),
        0,
        "module with a def must not emit; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn test_module_single_constant_not_emitted() {
    // Only 1 constant — below the ≥2 threshold.
    let src = "module SingleConst\n  X = 1\nend";
    let g = parse_rb(src);
    assert_eq!(
        enum_imitation_count(&g),
        0,
        "single constant must not emit (need ≥2); got: {:?}",
        g.blind_spots
    );
}

#[test]
fn test_module_with_nested_class_not_emitted() {
    // Module has ≥2 constants but a nested class — not an enum imitation.
    let src = "module Helper\n  X = 1\n  Y = 2\n  class Foo; end\nend";
    let g = parse_rb(src);
    assert_eq!(
        enum_imitation_count(&g),
        0,
        "module with nested class must not emit; got: {:?}",
        g.blind_spots
    );
}
