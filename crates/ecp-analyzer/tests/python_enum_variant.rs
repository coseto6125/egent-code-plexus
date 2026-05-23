//! Python enum visibility — Track A (EnumVariant emission) + Track B (class-as-enum BlindSpot).

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = PythonProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.py"), source.as_bytes())
        .expect("parse")
}

fn enum_variant_names(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .map(|n| n.name.as_str())
        .collect()
}

fn enum_variant_owners(g: &LocalGraph) -> Vec<Option<&str>> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .map(|n| n.owner_class.as_deref())
        .collect()
}

fn blind_spot_kinds(g: &LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── Track A: true Enum subclasses ────────────────────────────────────────────

#[test]
fn basic_enum_emits_variants() {
    let g = parse("from enum import Enum\nclass Color(Enum):\n    RED = 1\n    GREEN = 2\n");
    let mut names = enum_variant_names(&g);
    names.sort();
    assert_eq!(
        names,
        vec!["GREEN", "RED"],
        "expected 2 EnumVariants; nodes: {:?}",
        g.nodes
    );
    for owner in enum_variant_owners(&g) {
        assert_eq!(owner, Some("Color"), "owner_class must be Color");
    }
    // No imitation BlindSpot for a true Enum
    assert!(
        !blind_spot_kinds(&g).contains(&"python-class-as-enum"),
        "true Enum must not emit class-as-enum BlindSpot"
    );
}

#[test]
fn int_enum_emits_variants() {
    let g = parse("from enum import IntEnum\nclass Status(IntEnum):\n    OK = 0\n    ERR = 1\n");
    let mut names = enum_variant_names(&g);
    names.sort();
    assert_eq!(names, vec!["ERR", "OK"]);
    for owner in enum_variant_owners(&g) {
        assert_eq!(owner, Some("Status"));
    }
}

#[test]
fn multi_inheritance_with_enum_base_emits_variants() {
    // Track A fires when ANY base is an Enum marker; Mixin is concrete but irrelevant
    let g = parse("from enum import Enum\nclass X(Enum, Mixin):\n    A = 1\n");
    let names = enum_variant_names(&g);
    assert_eq!(names, vec!["A"], "expected 1 EnumVariant; names={names:?}");
    assert_eq!(enum_variant_owners(&g), vec![Some("X")]);
}

#[test]
fn enum_with_method_method_stays_method_variant_stays_variant() {
    let src =
        "from enum import Enum\nclass C(Enum):\n    R = 1\n    def label(self): return \"\"\n";
    let g = parse(src);
    // R must be EnumVariant
    let r_node = g.nodes.iter().find(|n| n.name == "R").expect("R not found");
    assert_eq!(r_node.kind, NodeKind::EnumVariant);
    // label must remain Method (not EnumVariant)
    let label_node = g
        .nodes
        .iter()
        .find(|n| n.name == "label")
        .expect("label not found");
    assert_eq!(label_node.kind, NodeKind::Method);
}

#[test]
fn dotted_enum_base_emits_variants() {
    // `enum.Enum` dotted form — trailing `Enum` must be recognised
    let g = parse("import enum\nclass Flags(enum.Flag):\n    READ = 1\n    WRITE = 2\n");
    let mut names = enum_variant_names(&g);
    names.sort();
    assert_eq!(names, vec!["READ", "WRITE"]);
}

#[test]
fn str_enum_emits_variants() {
    let g = parse(
        "from enum import StrEnum\nclass Color(StrEnum):\n    RED = \"red\"\n    BLUE = \"blue\"\n",
    );
    let mut names = enum_variant_names(&g);
    names.sort();
    assert_eq!(names, vec!["BLUE", "RED"]);
}

// ── Track B: class-as-enum imitation BlindSpot ───────────────────────────────

#[test]
fn imitation_two_int_consts_no_method_emits_blindspot() {
    let g = parse("class Status:\n    ACTIVE = 1\n    INACTIVE = 2\n");
    assert_eq!(
        enum_variant_names(&g),
        Vec::<&str>::new(),
        "no EnumVariants for imitation class"
    );
    assert!(
        blind_spot_kinds(&g).contains(&"python-class-as-enum"),
        "expected python-class-as-enum BlindSpot; got: {:?}",
        blind_spot_kinds(&g)
    );
    // Only one BlindSpot per class, not per member
    assert_eq!(
        blind_spot_kinds(&g)
            .iter()
            .filter(|&&k| k == "python-class-as-enum")
            .count(),
        1
    );
}

#[test]
fn imitation_three_string_consts_no_method_emits_blindspot() {
    let g = parse("class Color:\n    RED = \"red\"\n    BLUE = \"blue\"\n    GREEN = \"green\"\n");
    assert!(
        blind_spot_kinds(&g).contains(&"python-class-as-enum"),
        "expected python-class-as-enum BlindSpot; got: {:?}",
        blind_spot_kinds(&g)
    );
}

// ── Track B false-positive guards ────────────────────────────────────────────

#[test]
fn class_with_method_does_not_emit_blindspot() {
    let g = parse("class WithMethod:\n    X = 1\n    Y = 2\n    def go(self): pass\n");
    assert!(
        !blind_spot_kinds(&g).contains(&"python-class-as-enum"),
        "class with method must not emit BlindSpot; got: {:?}",
        blind_spot_kinds(&g)
    );
}

#[test]
fn class_with_only_one_uppercase_const_does_not_emit_blindspot() {
    let g = parse("class OneConst:\n    X = 1\n");
    assert!(
        !blind_spot_kinds(&g).contains(&"python-class-as-enum"),
        "< 2 uppercase consts must not emit BlindSpot; got: {:?}",
        blind_spot_kinds(&g)
    );
}

#[test]
fn class_with_mixed_case_consts_does_not_emit_blindspot() {
    // X is UPPERCASE, y is lowercase — only 1 qualifies → below threshold
    let g = parse("class Mixed:\n    X = 1\n    y = 2\n");
    assert!(
        !blind_spot_kinds(&g).contains(&"python-class-as-enum"),
        "only 1 uppercase const must not emit BlindSpot; got: {:?}",
        blind_spot_kinds(&g)
    );
}
