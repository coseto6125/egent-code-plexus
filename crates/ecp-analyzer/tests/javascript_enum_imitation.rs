use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_js(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    provider
        .parse_file(Path::new("test.js"), src.as_bytes())
        .expect("parse_file")
}

fn freeze_enum_count(g: &ecp_core::analyzer::types::LocalGraph) -> usize {
    g.blind_spots
        .iter()
        .filter(|b| b.kind == "js-object-freeze-enum")
        .count()
}

// ── positive cases ──────────────────────────────────────────────────────────

#[test]
fn js_object_freeze_number_values_emits_blind_spot() {
    let src = "const Status = Object.freeze({ ACTIVE: 1, INACTIVE: 2 });";
    let g = parse_js(src);
    assert_eq!(
        freeze_enum_count(&g),
        1,
        "number-valued Object.freeze with ≥2 entries must emit 1 BlindSpot; got {:?}",
        g.blind_spots
    );
}

#[test]
fn js_object_freeze_string_values_emits_blind_spot() {
    let src = r#"const Color = Object.freeze({ RED: 'red', BLUE: 'blue', GREEN: 'green' });"#;
    let g = parse_js(src);
    assert_eq!(
        freeze_enum_count(&g),
        1,
        "string-valued Object.freeze with ≥2 entries must emit 1 BlindSpot; got {:?}",
        g.blind_spots
    );
}

#[test]
fn js_multiple_object_freeze_in_one_file_emits_two_blind_spots() {
    let src = "const Status = Object.freeze({ ACTIVE: 1, INACTIVE: 2 });\n\
               const Direction = Object.freeze({ UP: 'up', DOWN: 'down' });";
    let g = parse_js(src);
    assert_eq!(
        freeze_enum_count(&g),
        2,
        "two Object.freeze enum imitations must emit 2 BlindSpots; got {:?}",
        g.blind_spots
    );
}

// ── false-positive guards ───────────────────────────────────────────────────

#[test]
fn js_bare_object_literal_no_blind_spot() {
    let src = "const Status = { ACTIVE: 1, INACTIVE: 2 };";
    let g = parse_js(src);
    assert_eq!(
        freeze_enum_count(&g),
        0,
        "bare object literal without Object.freeze must NOT emit BlindSpot; got {:?}",
        g.blind_spots
    );
}

#[test]
fn js_object_freeze_single_entry_no_blind_spot() {
    let src = "const X = Object.freeze({ ONLY: 1 });";
    let g = parse_js(src);
    assert_eq!(
        freeze_enum_count(&g),
        0,
        "Object.freeze with <2 entries must NOT emit BlindSpot; got {:?}",
        g.blind_spots
    );
}

#[test]
fn js_object_freeze_function_values_no_blind_spot() {
    let src = "const Handlers = Object.freeze({ onClick: () => {}, onLoad: () => {} });";
    let g = parse_js(src);
    assert_eq!(
        freeze_enum_count(&g),
        0,
        "Object.freeze with arrow-function values must NOT emit BlindSpot; got {:?}",
        g.blind_spots
    );
}

#[test]
fn js_object_freeze_call_values_no_blind_spot() {
    let src = "const Computed = Object.freeze({ A: getValue(), B: getOther() });";
    let g = parse_js(src);
    assert_eq!(
        freeze_enum_count(&g),
        0,
        "Object.freeze with call-expression values must NOT emit BlindSpot; got {:?}",
        g.blind_spots
    );
}
