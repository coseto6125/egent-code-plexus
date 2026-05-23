use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_ts(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    provider
        .parse_file(Path::new("test.ts"), src.as_bytes())
        .expect("parse_file")
}

fn imitation_spots(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots
        .iter()
        .filter(|b| b.kind == "ts-object-freeze-enum")
        .map(|b| b.kind.as_str())
        .collect()
}

// ── Pattern A: Object.freeze ────────────────────────────────────────────────

#[test]
fn ts_object_freeze_with_two_entries_emits_blind_spot() {
    let src = "const Status = Object.freeze({ ACTIVE: 1, INACTIVE: 2 });";
    let g = parse_ts(src);
    assert_eq!(
        imitation_spots(&g).len(),
        1,
        "Object.freeze with 2 scalar entries must emit 1 BlindSpot; got: {:?}",
        g.blind_spots
    );
}

// ── Pattern B: as const ──────────────────────────────────────────────────────

#[test]
fn ts_as_const_with_string_values_emits_blind_spot() {
    let src = "const Color = { RED: 'red', BLUE: 'blue' } as const;";
    let g = parse_ts(src);
    assert_eq!(
        imitation_spots(&g).len(),
        1,
        "as const with 2 string entries must emit 1 BlindSpot; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn ts_as_const_with_number_values_emits_blind_spot() {
    let src = "const Priority = { LOW: 1, MEDIUM: 5, HIGH: 10 } as const;";
    let g = parse_ts(src);
    assert_eq!(
        imitation_spots(&g).len(),
        1,
        "as const with 3 numeric entries must emit 1 BlindSpot; got: {:?}",
        g.blind_spots
    );
}

// ── True enum NOT flagged ────────────────────────────────────────────────────

#[test]
fn ts_native_enum_produces_no_imitation_blind_spot() {
    // Native `enum` is already covered by EnumVariant nodes; must NOT double-emit.
    let src = "enum Status { Active, Inactive }";
    let g = parse_ts(src);
    assert_eq!(
        imitation_spots(&g).len(),
        0,
        "native enum must not emit ts-object-freeze-enum; got: {:?}",
        g.blind_spots
    );
}

// ── False-positive guards ────────────────────────────────────────────────────

#[test]
fn ts_plain_object_no_freeze_no_as_const_produces_no_blind_spot() {
    let src = "const Status = { ACTIVE: 1, INACTIVE: 2 };";
    let g = parse_ts(src);
    assert_eq!(
        imitation_spots(&g).len(),
        0,
        "plain object literal without freeze or as const must NOT emit; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn ts_object_freeze_with_single_entry_produces_no_blind_spot() {
    let src = "const X = Object.freeze({ ONE: 1 });";
    let g = parse_ts(src);
    assert_eq!(
        imitation_spots(&g).len(),
        0,
        "Object.freeze with only 1 entry must NOT emit (<2 entries); got: {:?}",
        g.blind_spots
    );
}

#[test]
fn ts_as_const_with_function_values_produces_no_blind_spot() {
    let src = "const Handlers = { onClick: () => {} } as const;";
    let g = parse_ts(src);
    assert_eq!(
        imitation_spots(&g).len(),
        0,
        "as const with function values must NOT emit (not scalar); got: {:?}",
        g.blind_spots
    );
}
