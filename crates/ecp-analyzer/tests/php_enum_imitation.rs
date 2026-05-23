//! BlindSpot detection for pre-8.1 PHP enum imitations.
//!
//! PHP 8.1+ has first-class `enum` — those are handled by the `EnumVariant`
//! node track (sibling commit 3cd31a0d). This file covers the second track:
//! classes that use class-with-const as a pre-8.1 enum imitation. When an
//! LLM queries `MATCH (n:EnumVariant)` on such code it gets empty results
//! and wrongly concludes "no enums". Emitting a `BlindSpot` with kind
//! `php-enum-imitation` surfaces the imitation via `ecp schema blindspots`.
//!
//! Heuristic: `class_declaration` with ≥2 `const_declaration` children and
//! 0 `property_declaration` children. Methods do not disqualify a class.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("PhpProvider::new");
    p.parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse_file")
}

fn imitation_spots(g: &LocalGraph) -> Vec<&str> {
    g.blind_spots
        .iter()
        .filter(|b| b.kind == "php-enum-imitation")
        .map(|b| b.kind.as_str())
        .collect()
}

// ── positive cases ────────────────────────────────────────────────────────────

#[test]
fn imitation_positive_two_int_consts() {
    // Canonical pre-8.1 enum imitation — no instance state.
    let src = "<?php class Status { const ACTIVE = 1; const INACTIVE = 2; }";
    let g = parse(src);
    let spots = imitation_spots(&g);
    assert_eq!(
        spots.len(),
        1,
        "expected exactly 1 php-enum-imitation BlindSpot; got: {spots:?}"
    );
}

#[test]
fn imitation_backed_style_string_consts() {
    // Backed-enum imitation with string values (three consts).
    let src =
        r#"<?php class Color { const RED = 'red'; const BLUE = 'blue'; const GREEN = 'green'; }"#;
    let g = parse(src);
    let spots = imitation_spots(&g);
    assert_eq!(
        spots.len(),
        1,
        "expected 1 BlindSpot for string-const class; got: {spots:?}"
    );
}

#[test]
fn imitation_with_helper_method_still_emits() {
    // Methods do not disqualify — real enums can have helper methods.
    let src = r#"<?php
class Status {
    const ACTIVE = 1;
    const INACTIVE = 2;
    public function label(): string { return ''; }
}"#;
    let g = parse(src);
    let spots = imitation_spots(&g);
    assert_eq!(
        spots.len(),
        1,
        "helper method must not suppress BlindSpot; got: {spots:?}"
    );
}

// ── negative / false-positive guards ─────────────────────────────────────────

#[test]
fn fp_guard_has_instance_property() {
    // Instance state present → NOT an enum imitation.
    let src = "<?php class Status { const ACTIVE = 1; const INACTIVE = 2; private int $value; }";
    let g = parse(src);
    let spots = imitation_spots(&g);
    assert_eq!(
        spots.len(),
        0,
        "class with instance property must NOT emit BlindSpot; got: {spots:?}"
    );
}

#[test]
fn fp_guard_only_one_const() {
    // Fewer than 2 consts — insufficient density for enum imitation heuristic.
    let src = "<?php class OneConst { const X = 1; }";
    let g = parse(src);
    let spots = imitation_spots(&g);
    assert_eq!(
        spots.len(),
        0,
        "single-const class must NOT emit BlindSpot; got: {spots:?}"
    );
}

#[test]
fn fp_guard_true_81_enum_not_flagged() {
    // PHP 8.1+ first-class enum — already covered by EnumVariant track.
    // Must NOT emit an imitation BlindSpot.
    let src = "<?php enum Status { case Active; case Inactive; }";
    let g = parse(src);
    let spots = imitation_spots(&g);
    assert_eq!(
        spots.len(),
        0,
        "first-class PHP 8.1 enum must NOT emit imitation BlindSpot; got: {spots:?}"
    );
}
